use metrics::counter;
use nostr_sdk::prelude::*;
use ractor::{port::OutputPortSubscriber, RpcReplyPort};
use std::fmt::Debug;
use tracing::error;

pub enum SupervisorMessage {}

pub enum RelayEventDispatcherMessage {
    Connect,
    Reconnect,
    SubscribeToEventReceived(Vec<Filter>, OutputPortSubscriber<Event>),
    EventReceived(RelayPoolNotification),
}

#[derive(Debug)]
pub enum EventEnqueuerMessage {
    Enqueue(Event),
}

#[derive(Debug, Clone)]
pub enum TestActorMessage<T> {
    EventHappened(T),
}
