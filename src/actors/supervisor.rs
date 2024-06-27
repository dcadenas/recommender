use crate::actors::{
    messages::{RelayEventDispatcherMessage, SupervisorMessage},
    NostrPort, RelayEventDispatcher,
};
use anyhow::Result;
use metrics::counter;
use ractor::{call_t, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use tracing::error;

pub struct Supervisor<T> {
    _phantom: std::marker::PhantomData<T>,
}
impl<T> Default for Supervisor<T>
where
    T: NostrPort,
{
    fn default() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

#[ractor::async_trait]
impl<T> Actor for Supervisor<T>
where
    T: NostrPort,
{
    type Msg = SupervisorMessage;
    type State = ActorRef<RelayEventDispatcherMessage>;
    type Arguments = T;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        nostr_subscriber: T,
    ) -> Result<Self::State, ActorProcessingErr> {
        // Spawn actors and wire them together
        let (event_dispatcher, _event_dispatcher_handle) = Actor::spawn_linked(
            Some("event_dispatcher".to_string()),
            RelayEventDispatcher::default(),
            nostr_subscriber,
            myself.get_cell(),
        )
        .await?;

        Ok(event_dispatcher)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        event_dispatcher: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {}
        Ok(())
    }

    // For the moment we just log the errors and exit the whole system
    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        message: SupervisionEvent,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisionEvent::ActorTerminated(who, _state, maybe_msg) => {
                if let Some(msg) = maybe_msg {
                    error!("Actor terminated: {:?}, reason: {}", who, msg);
                } else {
                    error!("Actor terminated: {:?}", who);
                }
                myself.stop(None)
            }
            SupervisionEvent::ActorPanicked(dead_actor, panic_msg) => {
                counter!("actor_panicked").increment(1);
                error!("Actor panicked: {:?}, panic: {}", dead_actor, panic_msg);
            }
            SupervisionEvent::ActorStarted(_actor) => {}
            SupervisionEvent::ProcessGroupChanged(_group) => {}
        }

        Ok(())
    }
}
