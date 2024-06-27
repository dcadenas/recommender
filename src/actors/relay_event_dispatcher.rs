use crate::actors::messages::RelayEventDispatcherMessage;
use crate::service_manager::ServiceManager;
use anyhow::Result;
use metrics::counter;
use nostr_sdk::prelude::*;
use ractor::{port::output, Actor, ActorProcessingErr, ActorRef, OutputPort};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

pub struct RelayEventDispatcher<T: NostrPort> {
    _phantom: std::marker::PhantomData<T>,
}

impl<T: NostrPort> Default for RelayEventDispatcher<T> {
    fn default() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

pub struct Subscription {
    pub filters: Vec<Filter>,
    output_port: OutputPort<Event>,
}

impl Subscription {
    fn new(filters: Vec<Filter>) -> Self {
        let output_port = OutputPort::default();

        Self {
            filters,
            output_port,
        }
    }

    pub fn send(&self, event: Event) {
        self.output_port.send(event);
    }
}

pub struct State<T: NostrPort> {
    subscription_task_manager: Option<ServiceManager>,
    subscriptions: HashMap<SubscriptionId, Subscription>,
    nostr_client: T,
}

impl<T: NostrPort> State<T> {
    pub fn new(nostr_client: T) -> Self {
        Self {
            subscription_task_manager: None,
            subscriptions: HashMap::new(),
            nostr_client,
        }
    }

    pub async fn maybe_subscribe(&mut self, filters: Vec<Filter>) -> &Subscription {
        let key = filters.to_subscription_id();

        // Check if the subscription exists first without returning it immediately
        if self.subscriptions.contains_key(&key) {
            // If the subscription exists, we now retrieve it in a separate scope
            return self.subscriptions.get(&key).unwrap();
        }

        // Create and insert the new subscription
        let sub = Subscription::new(filters);
        self.nostr_client.subscribe(&sub).await;
        self.subscriptions.insert(key.clone(), sub);

        // Finally, retrieve and return the newly inserted subscription
        self.subscriptions.get(&key).unwrap()
    }
}

impl<T> RelayEventDispatcher<T>
where
    T: NostrPort,
{
    async fn handle_subscriptions(
        &self,
        myself: ActorRef<RelayEventDispatcherMessage>,
        state: &mut State<T>,
        action: &str,
    ) -> Result<()> {
        info!("{}", action);
        if let Some(subscription_task_manager) = &state.subscription_task_manager {
            subscription_task_manager.stop().await;
        }

        match spawn_subscription_task(myself.clone(), state).await {
            Ok(subscription_task_manager) => {
                state.subscription_task_manager = Some(subscription_task_manager);
            }
            Err(e) => {
                error!("Failed to spawn subscription task: {}", e);
            }
        }
        Ok(())
    }
}

#[async_trait]
pub trait NostrPort: Send + Sync + Clone + 'static {
    async fn connect(&self) -> Result<()>;
    async fn reconnect(&self) -> Result<()>;

    async fn start(
        &self,
        cancellation_token: CancellationToken,
        dispatcher_actor: ActorRef<RelayEventDispatcherMessage>,
    ) -> Result<(), anyhow::Error>;

    async fn subscribe(&self, subscription: &Subscription);
    async fn unsubscribe(&self, subscription_id: SubscriptionId);
}

#[ractor::async_trait]
impl<T: NostrPort> Actor for RelayEventDispatcher<T> {
    type Msg = RelayEventDispatcherMessage;
    type State = State<T>;
    type Arguments = T;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        nostr_client: T,
    ) -> Result<Self::State, ActorProcessingErr> {
        let subscriptions: HashMap<String, Subscription> = HashMap::new();

        let state = State {
            subscription_task_manager: None,
            subscriptions: HashMap::new(),
            nostr_client,
        };

        Ok(state)
    }

    async fn post_stop(
        &self,
        _: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if let Some(subscription_task_manager) = &state.subscription_task_manager {
            subscription_task_manager.stop().await;
            debug!("Subscription task manager stopped");
        }

        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            // TODO: Connect and Reconnect should probably be instead Fetch with
            // a limit, which would be sent initially from main and then from
            // the event enqueuer actor when it's done with the previous batch.
            // This would reduce risk of backpressure because ractor has a
            // hardcoded broadcast buffer size of 10 items. For the moment, we
            // avoid this risk by just having a since filter for the Nostr
            // request. DMs are not so common but we should fix this to avoid
            // DOS
            Self::Msg::Connect => {
                if let Err(e) = state.nostr_client.connect().await {
                    counter!("connect_error").increment(1);
                    error!("Failed to connect: {}", e);
                    return Ok(());
                }

                if let Err(e) = self.handle_subscriptions(myself, state, "Connecting").await {
                    counter!("connect_error").increment(1);
                    error!("Failed to connect: {}", e);
                    return Ok(());
                }

                counter!("connect").increment(1);
            }
            Self::Msg::Reconnect => {
                if let Err(e) = state.nostr_client.reconnect().await {
                    counter!("reconnect_error").increment(1);
                    error!("Failed to reconnect: {}", e);
                    return Ok(());
                }

                if let Err(e) = self
                    .handle_subscriptions(myself, state, "Reconnecting")
                    .await
                {
                    counter!("reconnect_error").increment(1);
                    error!("Failed to reconnect: {}", e);
                    return Ok(());
                }
                counter!("reconnect").increment(1);
            }
            Self::Msg::SubscribeToEventReceived(filters, subscriber) => {
                info!(
                    "Subscribing to {:?} with filter {:?}",
                    myself.get_name(),
                    filters
                );
                let subscription = state.maybe_subscribe(filters).await;
                subscriber.subscribe_to_port(&subscription.output_port);
            }
            Self::Msg::EventReceived(event_notif) => {
                let RelayPoolNotification::Event {
                    event,
                    relay_url,
                    subscription_id,
                } = event_notif
                else {
                    return Ok(());
                };

                let Some(subscription) = state.subscriptions.get(&subscription_id) else {
                    error!("No subscription {} found", subscription_id);
                    state.nostr_client.unsubscribe(subscription_id).await;
                    return Ok(());
                };

                subscription.send(*event);
                counter!("event_received").increment(1);
            }
        }

        Ok(())
    }
}

// We don't want to run long running tasks from inside an actor message handle
// so we spawn a task specifically for this. See
// https://github.com/slawlor/ractor/issues/133#issuecomment-1666947314
async fn spawn_subscription_task<T>(
    dispatcher_ref: ActorRef<RelayEventDispatcherMessage>,
    state: &State<T>,
) -> Result<ServiceManager, ActorProcessingErr>
where
    T: NostrPort,
{
    let subscription_task_manager = ServiceManager::new();

    let nostr_client_clone = state.nostr_client.clone();
    subscription_task_manager.spawn_blocking_service(|cancellation_token| async move {
        nostr_client_clone
            .start(cancellation_token, dispatcher_ref)
            .await
    });

    Ok(subscription_task_manager)
}

pub trait ToSubscriptionId {
    fn to_subscription_id(&self) -> SubscriptionId;
}

impl ToSubscriptionId for Vec<Filter> {
    fn to_subscription_id(&self) -> SubscriptionId {
        let mut hasher = DefaultHasher::new();
        for filter in self {
            let json_representation = filter.as_json();
            json_representation.hash(&mut hasher);
        }
        let hash = hasher.finish();
        SubscriptionId::new(format!("{:016x}", hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]

    fn test_to_subscription_id() {
        let filter1_1 = vec![Filter::new().kind(Kind::Metadata)];
        let filter1_2 = vec![Filter::new().kind(Kind::Metadata)];
        let filter2 = vec![Filter::new().kind(Kind::TextNote)];
        let keys = Keys::generate();
        let filter3 = vec![Filter::new().kind(Kind::TextNote).author(keys.public_key())];

        let sub1_1 = filter1_1.to_subscription_id();
        let sub1_2 = filter1_2.to_subscription_id();
        let sub2 = filter2.to_subscription_id();
        let sub3 = filter3.to_subscription_id();

        assert_eq!(sub1_1, SubscriptionId::new("0c9fae203a3c806d"));
        assert_ne!(sub1_1, sub2);
        assert_ne!(sub2, sub3);
    }
}
