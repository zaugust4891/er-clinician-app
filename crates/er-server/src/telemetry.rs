//! Privacy-aware process telemetry (Step 8 S9).
//!
//! Logs are a second copy of whatever they record, so this module decides what
//! may be recorded instead of trusting each call site:
//!
//! - **Format.** Every process outside `APP_ENV=dev` (including an unset or
//!   unrecognized value) writes flattened JSON at a bounded level, ignoring any
//!   `RUST_LOG` directive more verbose than `info`. SQL, SDK wire and HTTP-client
//!   targets are clamped so a debug setting cannot print statements or requests.
//! - **HTTP.** [`record_request`] replaces `TraceLayer`'s default span, which
//!   carries the full URI. It records an allowlist only: a server-generated
//!   request id, method, matched route *template*, status, duration, service and
//!   release. An unmatched path is recorded as [`UNMATCHED_ROUTE`].
//! - **Errors.** [`error_kind`] reduces an error chain to a stable code. SQL,
//!   serde and provider errors can echo row values, malformed input or response
//!   bodies, so their `Display`/`Debug` text is never logged.
//! - **Volume.** [`admit`] caps repetitive lines per code, so a broken queue or a
//!   token-spraying client cannot produce an unbounded log bill.
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request};
use axum::http::{HeaderValue, Method};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument;
use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

/// Route label for a request no route matched. The supplied path is never logged.
pub const UNMATCHED_ROUTE: &str = "unmatched";
/// Response header carrying the server-generated correlation id.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

// Dependencies are silent in cloud: warning/error text can also contain SQL,
// response bodies, tokens and URLs. Only reviewed application targets are enabled.
const CLAMPED_TARGETS: &str = "er_server::telemetry=info";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelemetrySettings {
    pub service: &'static str,
    pub release: String,
    pub json: bool,
    pub cloud: bool,
    pub filter: String,
}

impl TelemetrySettings {
    /// Pure so the selection rules are testable without touching the process.
    pub fn from_values(
        service: &'static str,
        app_env: Option<&str>,
        rust_log: Option<&str>,
        release: Option<&str>,
    ) -> Self {
        let cloud = app_env != Some("dev");
        let (json, filter) = if cloud {
            // Only a quieter base level is honored in cloud.
            let base = match rust_log.map(str::trim) {
                Some("error") => "error",
                Some("warn") => "warn",
                _ => "info",
            };
            (
                true,
                format!(
                    "off,er_server={base},er_worker={base},er_migrate={base},er_db_bootstrap={base},er_onboard={base},{CLAMPED_TARGETS}"
                ),
            )
        } else {
            // Local behavior is unchanged: RUST_LOG selects JSON and the filter.
            (rust_log.is_some(), rust_log.unwrap_or("info").to_string())
        };
        Self {
            service,
            release: release_label(release),
            json,
            cloud,
            filter,
        }
    }
}

/// The release identifier is operator input that lands in every request line,
/// so only a short identifier-shaped value is accepted.
fn release_label(release: Option<&str>) -> String {
    match release {
        None => "unknown".into(),
        Some(value)
            if !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:@-".contains(&b)) =>
        {
            value.into()
        }
        Some(_) => "invalid".into(),
    }
}

static SERVICE: OnceLock<(&'static str, String)> = OnceLock::new();

fn service_and_release() -> (&'static str, &'static str) {
    SERVICE
        .get()
        .map(|(service, release)| (*service, release.as_str()))
        .unwrap_or(("unknown", "unknown"))
}

/// Install the process subscriber. Call once, before configuration parsing, so
/// a startup failure is recorded in the same format as everything else.
pub fn init(service: &'static str) -> TelemetrySettings {
    let settings = TelemetrySettings::from_values(
        service,
        std::env::var("APP_ENV").ok().as_deref(),
        std::env::var("RUST_LOG").ok().as_deref(),
        std::env::var("RELEASE_ID").ok().as_deref(),
    );
    let _ = SERVICE.set((settings.service, settings.release.clone()));
    let filter = EnvFilter::try_new(&settings.filter).unwrap_or_else(|_| EnvFilter::new("info"));
    let layer = if settings.json {
        json_layer(std::io::stdout).boxed()
    } else {
        tracing_subscriber::fmt::layer().pretty().boxed()
    };
    tracing_subscriber::registry()
        .with(filter)
        .with(layer)
        .init();
    if settings.cloud {
        install_panic_hook();
    }
    tracing::info!(
        service = settings.service,
        release = %settings.release,
        "process telemetry initialized"
    );
    settings
}

/// The cloud line format: one flattened JSON object per event, the current span's
/// fields (the request id) included, no span list and no ANSI.
pub fn json_layer<S, W>(writer: W) -> impl Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    W: for<'w> tracing_subscriber::fmt::MakeWriter<'w> + Send + Sync + 'static,
{
    tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_current_span(true)
        .with_span_list(false)
        .with_ansi(false)
        .with_writer(writer)
}

/// The default hook prints the panic payload, which for `unwrap`/`expect` is the
/// error's `Debug`. Cloud records the location only.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let (file, line) = info
            .location()
            .map(|location| (location.file(), location.line()))
            .unwrap_or(("unknown", 0));
        tracing::error!(error_kind = "panic", file, line, "process panicked");
    }));
}

// ---------------------------------------------------------------------------
// HTTP allowlist
// ---------------------------------------------------------------------------

/// Allowlisted request logging. Must be applied with `Router::layer` so axum has
/// already inserted [`MatchedPath`] for routed requests.
pub async fn record_request(mut req: Request, next: Next) -> Response {
    let started = Instant::now();
    let request_id = uuid::Uuid::new_v4();
    let method = method_label(req.method());
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_owned())
        .unwrap_or_else(|| UNMATCHED_ROUTE.to_owned());
    // Handlers may log the same id; a client-supplied id is never trusted.
    req.extensions_mut().insert(RequestId(request_id));

    let span = tracing::info_span!("http_request", %request_id);
    let mut response = next.run(req).instrument(span.clone()).await;
    let status = response.status().as_u16();
    if let Ok(value) = HeaderValue::from_str(&request_id.to_string()) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }

    // Successful load-balancer probes are the bulk of traffic and carry no signal.
    let probe = matches!(route.as_str(), "/healthz" | "/readyz");
    if !(probe && status == 200) && admit("http_request").is_some() {
        let (service, release) = service_and_release();
        tracing::info!(
            parent: &span,
            method,
            route = %route,
            status,
            duration_ms = started.elapsed().as_millis() as u64,
            service,
            release,
            "request completed"
        );
    }
    response
}

/// The server-generated correlation id for one request.
#[derive(Clone, Copy, Debug)]
pub struct RequestId(pub uuid::Uuid);

/// Standard methods only: an extension method is arbitrary client text.
pub fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::PATCH => "PATCH",
        Method::HEAD => "HEAD",
        Method::OPTIONS => "OPTIONS",
        _ => "OTHER",
    }
}

// ---------------------------------------------------------------------------
// Classified errors
// ---------------------------------------------------------------------------

/// An error whose only content is a stable code. Use it to replace transport
/// errors whose text is unsafe or noisy (queue URLs, SDK bodies).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Classified(pub &'static str);

/// Reduce an error chain to a stable code. The first classifiable cause from
/// the outside in wins; unknown errors are `unclassified`, never their text.
pub fn error_kind(error: &anyhow::Error) -> &'static str {
    let mut fallback = "unclassified";
    for cause in error.chain() {
        if let Some(classified) = cause.downcast_ref::<Classified>() {
            return classified.0;
        }
        if let Some(audio) = cause.downcast_ref::<crate::audio::AudioError>() {
            return audio.0;
        }
        if let Some(error) = cause.downcast_ref::<sqlx::Error>() {
            return sqlx_kind(error);
        }
        if let Some(error) = cause.downcast_ref::<serde_json::Error>() {
            return json_kind(error);
        }
        if let Some(error) = cause.downcast_ref::<er_ai::error::AiError>() {
            return ai_kind(error);
        }
        if let Some(error) = cause.downcast_ref::<er_store::StoreError>() {
            match error {
                // The boxed driver error is the next cause; classify that.
                er_store::StoreError::Backend(_) => fallback = "store_backend",
                er_store::StoreError::Db(_) => return "store_sqlite",
                er_store::StoreError::Serialization(error) => return json_kind(error),
                er_store::StoreError::Parse { .. } => return "store_parse",
                er_store::StoreError::Remote { .. } => return "store_remote",
            }
        }
    }
    fallback
}

fn sqlx_kind(error: &sqlx::Error) -> &'static str {
    use sqlx::error::ErrorKind;
    match error {
        sqlx::Error::PoolTimedOut => "db_pool_timeout",
        sqlx::Error::PoolClosed | sqlx::Error::WorkerCrashed => "db_pool_closed",
        sqlx::Error::Io(_) | sqlx::Error::Tls(_) => "db_connection",
        sqlx::Error::Configuration(_) => "db_configuration",
        sqlx::Error::Protocol(_) => "db_protocol",
        sqlx::Error::RowNotFound => "db_row_not_found",
        sqlx::Error::Migrate(_) => "db_migration",
        sqlx::Error::Database(db) => match db.kind() {
            ErrorKind::UniqueViolation => "db_unique_violation",
            ErrorKind::ForeignKeyViolation => "db_foreign_key_violation",
            ErrorKind::NotNullViolation => "db_not_null_violation",
            ErrorKind::CheckViolation => "db_check_violation",
            _ => "db_statement_failed",
        },
        sqlx::Error::ColumnDecode { .. }
        | sqlx::Error::Decode(_)
        | sqlx::Error::ColumnNotFound(_)
        | sqlx::Error::ColumnIndexOutOfBounds { .. }
        | sqlx::Error::TypeNotFound { .. } => "db_decode",
        _ => "db_other",
    }
}

fn json_kind(error: &serde_json::Error) -> &'static str {
    use serde_json::error::Category;
    match error.classify() {
        Category::Io => "json_io",
        Category::Syntax => "json_syntax",
        Category::Data => "json_data",
        Category::Eof => "json_eof",
    }
}

pub(crate) fn ai_kind(error: &er_ai::error::AiError) -> &'static str {
    use er_ai::error::AiError;
    match error {
        AiError::Api(_) => "provider_http",
        AiError::Transport(_) => "provider_transport",
        AiError::RateLimited(_) => "provider_rate_limited",
        AiError::Refused { .. } => "provider_refused",
        AiError::Summarization(_) => "invalid_model_output",
    }
}

// ---------------------------------------------------------------------------
// Startup failures
// ---------------------------------------------------------------------------

/// Why a process could not start. `main` returning `anyhow::Error` would print
/// its full `Debug` chain to stderr, which reaches the log group unredacted.
#[derive(Debug)]
pub struct StartupFailure {
    pub stage: &'static str,
    pub error_kind: &'static str,
}

impl StartupFailure {
    pub fn config(_error: anyhow::Error) -> Self {
        Self {
            stage: "configuration",
            error_kind: "configuration_invalid",
        }
    }

    /// A failure already reduced to a code by the caller.
    pub fn code(stage: &'static str, error_kind: &'static str) -> Self {
        Self { stage, error_kind }
    }

    /// `map_err(StartupFailure::at("database"))`.
    pub fn at<E: Into<anyhow::Error>>(stage: &'static str) -> impl FnOnce(E) -> Self {
        move |error| Self {
            stage,
            error_kind: error_kind(&error.into()),
        }
    }

    pub fn report(&self) {
        let (service, release) = service_and_release();
        tracing::error!(
            stage = self.stage,
            error_kind = self.error_kind,
            service,
            release,
            "startup failed"
        );
    }
}

// ---------------------------------------------------------------------------
// Log volume
// ---------------------------------------------------------------------------

/// Fixed-window cap per static code. Keys are `&'static str`, so the map is
/// bounded by the codes compiled into the binary, not by input.
pub struct LogBudget {
    limit: u32,
    window: Duration,
    windows: Mutex<HashMap<&'static str, Window>>,
}

struct Window {
    started: Instant,
    emitted: u32,
    suppressed: u64,
}

impl LogBudget {
    pub fn new(limit: u32, window: Duration) -> Self {
        Self {
            limit,
            window,
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// `Some(n)` admits a line and reports `n` lines suppressed since the last
    /// admitted one; `None` means drop it.
    pub fn admit_at(&self, key: &'static str, now: Instant) -> Option<u64> {
        let mut windows = self.windows.lock().unwrap_or_else(PoisonError::into_inner);
        let window = windows.entry(key).or_insert(Window {
            started: now,
            emitted: 0,
            suppressed: 0,
        });
        if now.saturating_duration_since(window.started) >= self.window {
            window.started = now;
            window.emitted = 0;
        }
        if window.emitted < self.limit {
            window.emitted += 1;
            Some(std::mem::take(&mut window.suppressed))
        } else {
            window.suppressed += 1;
            None
        }
    }
}

/// 20 lines per code per minute: enough to diagnose, bounded under a flood.
static BUDGET: LazyLock<LogBudget> = LazyLock::new(|| LogBudget::new(20, Duration::from_secs(60)));

/// Process-wide budget for repetitive warning/error lines.
pub fn admit(key: &'static str) -> Option<u64> {
    BUDGET.admit_at(key, Instant::now())
}

// ---------------------------------------------------------------------------
// Bounded aggregate operational signals. No identifier or provider-supplied label.
// These counters are best-effort telemetry, not billing records. A killed process
// may lose its current minute; durable attribution stays in usage_log.
// ---------------------------------------------------------------------------
#[derive(Default)]
struct Metrics {
    last_flush: Option<Instant>,
    values: std::collections::BTreeMap<(&'static str, &'static str), u64>,
}
static METRICS: LazyLock<Mutex<Metrics>> = LazyLock::new(|| Mutex::new(Metrics::default()));

fn metric(kind: &'static str, name: &'static str, value: u64) {
    let mut metrics = METRICS.lock().unwrap_or_else(PoisonError::into_inner);
    let counter = metrics.values.entry((kind, name)).or_default();
    *counter = counter.saturating_add(value);
}

pub fn job_finished(kind: &'static str, failed: bool) {
    metric(
        kind,
        if failed {
            "jobs_failed"
        } else {
            "jobs_completed"
        },
        1,
    );
}

pub fn provider_finished(kind: &'static str, started: Instant, error: Option<&'static str>) {
    metric(kind, "provider_calls", 1);
    metric(
        kind,
        "provider_latency_ms",
        started.elapsed().as_millis().min(u64::MAX as u128) as u64,
    );
    if let Some(code) = error {
        metric(kind, "provider_failures", 1);
        // A static classifier code, never a provider message/model name.
        metric(kind, code, 1);
    }
}

pub fn usage(kind: &'static str, usage: &er_ai::transport::Usage) {
    metric(kind, "input_tokens", usage.input_tokens);
    metric(kind, "output_tokens", usage.output_tokens);
    metric(kind, "cache_read_tokens", usage.cache_read_input_tokens);
    metric(
        kind,
        "cache_write_tokens",
        usage.cache_creation_input_tokens,
    );
}

pub fn usage_write_failed() {
    metric("all", "usage_write_failed", 1);
}

/// Called only after a successful receive AND completion of all returned handlers.
/// Idle completed long polls count. A receive failure or wedged handler cannot
/// advance this signal. No timer or lease-renewal task calls this function.
/// At most one flush per minute (including under an empty-queue busy loop).
pub fn poll_completed() {
    let mut metrics = METRICS.lock().unwrap_or_else(PoisonError::into_inner);
    metric_flush(&mut metrics, Instant::now());
}

fn metric_flush(metrics: &mut Metrics, now: Instant) {
    if metrics
        .last_flush
        .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_secs(60))
    {
        return;
    }
    metrics.last_flush = Some(now);
    tracing::info!(
        metric_name = "worker_progress",
        kind = "all",
        value = 1u64,
        "operational metric"
    );
    for ((kind, metric_name), value) in std::mem::take(&mut metrics.values) {
        tracing::info!(metric_name, kind, value, "operational metric");
    }
}

/// Fixed-field metrics; no tenant/patient dimensions or provider response text.
pub fn mdm_duration(started: Instant) {
    tracing::info!(
        mdm_duration_ms = started.elapsed().as_millis() as u64,
        "MDM job duration"
    );
}
pub fn stt_duration(duration_ms: i32) {
    tracing::info!(
        stt_seconds = f64::from(duration_ms.max(0)) / 1000.0,
        "STT audio duration"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn cloud_is_json_and_bounded_whatever_rust_log_says() {
        for env in [Some("staging"), Some("prod"), None, Some("DEV")] {
            let s = TelemetrySettings::from_values("api", env, Some("trace,sqlx=debug"), None);
            assert!(s.cloud && s.json, "{env:?}");
            assert!(s.filter.starts_with("off,er_server=info,"), "{}", s.filter);
            assert!(!s.filter.contains("debug") && !s.filter.contains("trace"));
            assert!(!s.filter.contains("sqlx="));
        }
        let quiet = TelemetrySettings::from_values("worker", Some("prod"), Some("warn"), None);
        assert!(quiet.filter.starts_with("off,er_server=warn,"));
        let dev = TelemetrySettings::from_values("api", Some("dev"), None, None);
        assert!(!dev.cloud && !dev.json);
        assert_eq!(dev.filter, "info");
    }

    #[test]
    fn release_must_look_like_an_identifier() {
        let ok = TelemetrySettings::from_values("api", None, None, Some("sha256:abc-1.2"));
        assert_eq!(ok.release, "sha256:abc-1.2");
        let bad = TelemetrySettings::from_values("api", None, None, Some("x\"} injected"));
        assert_eq!(bad.release, "invalid");
        assert_eq!(
            TelemetrySettings::from_values("api", None, None, None).release,
            "unknown"
        );
    }

    #[test]
    fn errors_reduce_to_codes_without_their_text() {
        let canary = "CANARY-7f3a";
        let protocol = anyhow::Error::new(sqlx::Error::Protocol(canary.into()));
        assert_eq!(error_kind(&protocol), "db_protocol");

        let wrapped = anyhow::Error::new(er_store::StoreError::backend(sqlx::Error::PoolTimedOut))
            .context(format!("load {canary}"));
        assert_eq!(error_kind(&wrapped), "db_pool_timeout");

        #[derive(Debug, serde::Deserialize)]
        enum Kind {
            #[allow(dead_code)]
            Known,
        }
        let serde = serde_json::from_str::<Kind>(&format!("\"{canary}\"")).unwrap_err();
        assert!(serde.to_string().contains(canary), "serde echoes input");
        assert_eq!(error_kind(&anyhow::Error::new(serde)), "json_data");

        let refused = er_ai::error::AiError::Refused {
            status: 400,
            message: canary.into(),
        };
        assert_eq!(error_kind(&anyhow::Error::new(refused)), "provider_refused");
        assert_eq!(error_kind(&anyhow::anyhow!("{canary}")), "unclassified");
        assert_eq!(
            error_kind(&anyhow::Error::new(Classified("queue_send_failed")).context(canary)),
            "queue_send_failed"
        );
    }

    #[test]
    fn startup_failures_keep_only_codes() {
        let failure = StartupFailure::at("database")(sqlx::Error::Protocol("CANARY".into()));
        assert_eq!(
            (failure.stage, failure.error_kind),
            ("database", "db_protocol")
        );
        assert!(
            !format!("{:?}", StartupFailure::config(anyhow::anyhow!("CANARY"))).contains("CANARY")
        );
    }

    #[test]
    fn budget_caps_per_key_and_reports_suppressed_lines() {
        let budget = LogBudget::new(2, Duration::from_secs(60));
        let t0 = Instant::now();
        assert_eq!(budget.admit_at("a", t0), Some(0));
        assert_eq!(budget.admit_at("a", t0), Some(0));
        assert_eq!(budget.admit_at("a", t0), None);
        assert_eq!(budget.admit_at("a", t0), None);
        // Independent key, independent budget.
        assert_eq!(budget.admit_at("b", t0), Some(0));
        let t1 = t0 + Duration::from_secs(60);
        assert_eq!(budget.admit_at("a", t1), Some(2));
        assert_eq!(budget.admit_at("a", t1), Some(0));
        assert_eq!(budget.admit_at("a", t1), None);
    }

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn json_lines_are_flat_and_carry_the_request_span() {
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::registry().with(json_layer(move || writer.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("http_request", request_id = "r-1");
            let _entered = span.enter();
            tracing::error!(error_kind = "db_protocol", "internal error");
        });
        let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        let line: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(line["error_kind"], "db_protocol");
        assert_eq!(line["span"]["request_id"], "r-1");
        assert!(line.get("spans").is_none());
    }
    #[test]
    fn progress_and_aggregate_flush_are_bounded_without_losing_counts() {
        let capture = Capture::default();
        let writer = capture.clone();
        let settings =
            TelemetrySettings::from_values("worker", Some("staging"), Some("error"), None);
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new(settings.filter))
            .with(json_layer(move || writer.clone()));
        let mut metrics = Metrics::default();
        let now = Instant::now();
        tracing::subscriber::with_default(subscriber, || {
            metric_flush(&mut metrics, now);
            metrics.values.insert(("mdm", "jobs_failed"), 19);
            metric_flush(&mut metrics, now + Duration::from_secs(59));
            assert_eq!(metrics.values[&("mdm", "jobs_failed")], 19);
            metric_flush(&mut metrics, now + Duration::from_secs(60));
            assert!(metrics.values.is_empty());
        });
        let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(
            lines
                .iter()
                .filter(|l| l["metric_name"] == "worker_progress")
                .count(),
            2
        );
        assert!(
            lines
                .iter()
                .any(|l| l["metric_name"] == "jobs_failed" && l["value"] == 19)
        );
        assert!(
            lines
                .iter()
                .all(|l| l.get("job_id").is_none() && l.get("patient_id").is_none())
        );
    }
}
