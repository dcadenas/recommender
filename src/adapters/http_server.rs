mod router;
use crate::actors::messages::SupervisorMessage;
use anyhow::{Context, Result};
use axum::Router;
use ractor::ActorRef;
use router::create_router;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::info;

#[derive(Clone)]
pub struct WebAppState {
    event_dispatcher: ActorRef<SupervisorMessage>,
}

pub struct HttpServer;
impl HttpServer {
    pub async fn run(
        cancellation_token: CancellationToken,
        event_dispatcher: ActorRef<SupervisorMessage>,
    ) -> Result<()> {
        let router = create_router(event_dispatcher)?;

        start_http_server(router, cancellation_token).await
    }
}

async fn start_http_server(router: Router, cancellation_token: CancellationToken) -> Result<()> {
    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let token_clone = cancellation_token.clone();
    let server_future = tokio::spawn(async {
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_hook(token_clone))
            .await
            .context("Failed to start HTTP server")
    });

    await_shutdown(cancellation_token, server_future).await;

    Ok(())
}

async fn await_shutdown(
    cancellation_token: CancellationToken,
    server_future: tokio::task::JoinHandle<Result<()>>,
) {
    cancellation_token.cancelled().await;
    info!("Shutdown signal received.");
    match timeout(Duration::from_secs(5), server_future).await {
        Ok(_) => info!("HTTP service exited successfully."),
        Err(e) => info!("HTTP service exited after timeout: {}", e),
    }
}

async fn shutdown_hook(cancellation_token: CancellationToken) {
    cancellation_token.cancelled().await;
    info!("Exiting the process");
}
