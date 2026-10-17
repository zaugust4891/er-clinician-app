use er_server::{config, routes, state, tls};

use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    // JSON in prod, pretty locally (RUST_LOG)
    let config = config::Config::from_env()?;
    tracing::info!(environment = ?config.app_env, process = "api", "configuration validated");
    // Key/JWKS/DB/queue and TLS initialization all finish before the port opens,
    // so a task that cannot start never answers a health check.
    let state = state::AppState::build(&config)
        .await
        .map_err(|_| anyhow::anyhow!("api_initialization_failed"))?;
    let identity = match config.tls {
        config::ApiTls::SelfSigned => Some(tls::ServerIdentity::generate()?),
        config::ApiTls::Off => None,
    };
    let app = routes::router(state).layer(tower_http::trace::TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(&config.bind).await?; //SIGTERM for ECS
    match identity {
        Some(identity) => {
            let listener = tls::TlsListener::new(listener, &identity)?;
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await?;
        }
        None => {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await?;
        }
    }
    Ok(())
}

fn init_tracing() {
    let fmt_layer = if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::fmt::layer().json().boxed()
    } else {
        tracing_subscriber::fmt::layer().pretty().boxed()
    };

    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(fmt_layer)
        .init();
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");

    tokio::select! {
        _ = sigterm.recv() => {},
        _ = sigint.recv() => {},
    }
}
