use std::process::ExitCode;

use er_server::telemetry::{self, StartupFailure};
use er_server::{config, routes, state, tls};

#[tokio::main]
async fn main() -> ExitCode {
    // JSON and bounded outside dev; installed before config so failures share it.
    telemetry::init("api");
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            failure.report();
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), StartupFailure> {
    let config = config::Config::from_env().map_err(StartupFailure::config)?;
    tracing::info!(environment = ?config.app_env, process = "api", "configuration validated");
    // Key/JWKS/DB/queue and TLS initialization all finish before the port opens,
    // so a task that cannot start never answers a health check.
    let state = state::AppState::build(&config)
        .await
        .map_err(StartupFailure::at("initialization"))?;
    let identity = match config.tls {
        config::ApiTls::SelfSigned => {
            Some(tls::ServerIdentity::generate().map_err(StartupFailure::at("tls"))?)
        }
        config::ApiTls::Off => None,
    };
    // Request logging is the allowlist in `telemetry::record_request`, inside the router.
    let app = routes::router(state);
    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .map_err(StartupFailure::at("bind"))?; //SIGTERM for ECS
    let served = match identity {
        Some(identity) => {
            let listener =
                tls::TlsListener::new(listener, &identity).map_err(StartupFailure::at("tls"))?;
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await
        }
        None => {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await
        }
    };
    served.map_err(StartupFailure::at("serve"))
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
