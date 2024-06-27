mod actors;
mod adapters;
mod service_manager;

use crate::actors::Supervisor;
use crate::adapters::{HttpServer, NostrService};
use crate::service_manager::ServiceManager;
use actors::NostrPort;
use anyhow::{Context, Result};
use std::env;
use tracing::info;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    let relays = get_relays()?;
    let nostr_subscriber = NostrService::create(relays).await?;

    start_server(nostr_subscriber).await
}

async fn start_server(nostr_subscriber: impl NostrPort) -> Result<()> {
    let mut manager = ServiceManager::new();

    // Spawn actors and wire them together
    let supervisor = manager
        .spawn_actor(Supervisor::default(), nostr_subscriber)
        .await?;

    manager.spawn_service(|cancellation_token| HttpServer::run(cancellation_token, supervisor));

    manager
        .listen_stop_signals()
        .await
        .context("Failed to spawn actors")
}

fn get_relays() -> Result<Vec<String>> {
    let Ok(value) = env::var("RELAY_ADDRESSES_CSV") else {
        return Err(anyhow::anyhow!("RELAY_ADDRESSES_CSV env variable not set"));
    };

    if value.trim().is_empty() {
        return Err(anyhow::anyhow!("RELAY_ADDRESSES_CSV env variable is empty"));
    }

    let relays = value
        .trim()
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();

    info!("Using relays: {:?}", relays);
    Ok(relays)
}
