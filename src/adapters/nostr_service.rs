use crate::actors::messages::RelayEventDispatcherMessage;
use crate::actors::{NostrPort, Subscription, ToSubscriptionId};
use anyhow::Result;
use futures::future::join_all;
use nostr_sdk::prelude::*;
use ractor::{cast, concurrency::Duration, ActorRef};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

#[derive(Clone)]
pub struct NostrService {
    client: Client,
}
impl NostrService {
    pub async fn create(relays: Vec<String>) -> Result<Self> {
        let opts = Options::new()
            .skip_disconnected_relays(true)
            .wait_for_send(false)
            .connection_timeout(Some(Duration::from_secs(5)))
            .send_timeout(Some(Duration::from_secs(5)))
            .wait_for_subscription(true);

        let client = ClientBuilder::new().opts(opts).build();
        for relay in relays.iter().cloned() {
            client.add_relay(relay).await?;
        }

        Ok(Self { client })
    }
}

#[async_trait]
impl NostrPort for NostrService {
    async fn connect(&self) -> Result<()> {
        self.client.connect().await;
        Ok(())
    }

    async fn reconnect(&self) -> Result<()> {
        self.client.disconnect().await?;
        self.connect().await?;
        Ok(())
    }

    async fn subscribe(&self, subscription: &Subscription) {
        info!("Subscribing to {:?}", subscription.filters);
        // If we ever have different type of subscriptions, we should separate
        // creation from handling. We can have a single handler for all subs.
        // See: https://github.com/rust-nostr/nostr/issues/345#issuecomment-1985925161

        self.client
            .subscribe_with_id(
                subscription.filters.to_subscription_id(),
                subscription.filters.clone(),
                None,
            )
            .await;
    }

    async fn unsubscribe(&self, subscription_id: SubscriptionId) {
        info!("Unsubscribing from {}", subscription_id);

        self.client.unsubscribe(subscription_id).await;
    }

    async fn start(
        &self,
        cancellation_token: CancellationToken,
        dispatcher_actor: ActorRef<RelayEventDispatcherMessage>,
    ) -> Result<(), anyhow::Error> {
        let client_clone = self.client.clone();
        let token_clone = cancellation_token.clone();
        tokio::spawn(async move {
            token_clone.cancelled().await;
            debug!("Cancelling relay subscription worker");
            if let Err(e) = client_clone.stop().await {
                error!("Failed to stop client: {}", e);
            }
        });

        let cancel_and_reconnect = || async {
            // If it was not cancelled we want to retry, so cancel manually and reconnect
            if !cancellation_token.is_cancelled() {
                cancellation_token.cancel();
                if let Err(e) = dispatcher_actor
                    .send_after(Duration::from_secs(10), || {
                        RelayEventDispatcherMessage::Reconnect
                    })
                    .await
                {
                    error!("Failed to send reconnect message: {}", e);
                }
            }
        };

        // If not connected don't event try to subscribe
        if all_disconnected(&self.client).await {
            error!("All relays are disconnected, not subscribing");
            cancel_and_reconnect().await;
            return Ok(());
        }

        self.client
            .handle_notifications(|notification| async {
                if cancellation_token.is_cancelled() {
                    return Ok(true);
                }

                if let RelayPoolNotification::Event { .. } = notification {
                    cast!(
                        dispatcher_actor,
                        RelayEventDispatcherMessage::EventReceived(notification)
                    )
                    .expect("Failed to cast event to dispatcher");
                }

                // True would exit from the loop
                Ok(false)
            })
            .await?;

        cancel_and_reconnect().await;
        Ok(())
    }
}

async fn all_disconnected(client: &Client) -> bool {
    let relays = client.pool().relays().await;

    let futures: Vec<_> = relays.values().map(|relay| relay.is_connected()).collect();

    let results = join_all(futures).await;

    results.iter().all(|&is_connected| !is_connected)
}
