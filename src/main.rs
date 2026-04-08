mod config;
mod error;
mod middleware;
mod porkbun;
mod webhook;

use anyhow::Result;
use axum::{middleware as axum_middleware, serve, Router};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use crate::config::Config;
use crate::webhook::routes;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing first so config errors get structured logging
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    // Initialize configuration
    let config = Config::from_env()?;

    info!("Starting Porkbun webhook provider");
    info!(
        "Listening on {}:{}",
        config.webhook_host, config.webhook_port
    );

    if let Some(ref domains) = config.domain_filter {
        info!("Domain filter: {:?}", domains);
    } else {
        info!("No domain filter set - managing all account domains");
    }

    if config.dry_run {
        info!("DRY RUN mode enabled - no changes will be applied");
    }

    // Configure body tracing
    middleware::set_trace_request_bodies(config.trace_request_bodies);

    // Create Porkbun client
    let porkbun_client = porkbun::Client::new(
        &config.porkbun_api_key,
        &config.porkbun_secret_api_key,
        &config.porkbun_api_base,
        Duration::from_secs(config.http_timeout_seconds),
    )?;

    // Build the application
    let app = Router::new()
        .merge(routes::create_routes(porkbun_client, config.clone()))
        .layer(axum_middleware::from_fn(
            middleware::error_handling_middleware,
        ))
        .layer(axum_middleware::from_fn(middleware::logging_middleware))
        .layer(TraceLayer::new_for_http());

    // Create socket address
    let addr = SocketAddr::new(config.webhook_host.parse()?, config.webhook_port);

    // Start the server with graceful shutdown
    let listener = TcpListener::bind(addr).await?;
    info!("Server started on {addr}");

    serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("Server shut down gracefully");
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal;

    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => { info!("Received Ctrl+C, initiating graceful shutdown"); },
        () = terminate => { info!("Received SIGTERM, initiating graceful shutdown"); },
    }
}
