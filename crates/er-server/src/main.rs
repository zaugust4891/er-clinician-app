use er_server::{config, routes, state};

use std::time::Duration;
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};





#[tokio::main]
async fn main() -> anyhow::Result<()> {
    
    init_tracing();
    // JSON in prod, pretty locally (RUST_LOG)
    let config = config::Config::from_env()?;
    let state = state::AppState::build(&config).await?;
    let app = routes::router(state)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(axum::http::StatusCode::REQUEST_TIMEOUT, Duration::from_secs(30)));
    let listener = tokio::net::TcpListener::bind(&config.bind).await?; //SIGTERM for ECS
    axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;
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
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");

    tokio::select! {
        _ = sigterm.recv() => {},
        _ = sigint.recv() => {},
    }
}