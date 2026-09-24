use crate::simulation::framework_events::{QSimId, WorkerListenerRegisterFunction};
use std::any::Any;
use std::sync::mpsc::Sender;

pub mod backpacking;
pub mod partial_plans;

pub trait Message: Any + Send {
    fn as_any(&self) -> &dyn Any;

    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

impl<T: Any + Send> Message for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

pub struct InternalScoringMessage {
    pub(crate) from_process: QSimId,
    #[allow(unused)]
    pub(crate) to_process: QSimId,
    pub(crate) message: Box<dyn Message>,
}

/// Trait for a scoring engine that can be initialized and finished by the controller.
pub trait ScoringEngine: Send + Sync {
    /// Attaches the senders to the internal structs managing message handling.
    fn attach_senders(&mut self, senders: Vec<Sender<InternalScoringMessage>>);

    /// Returns the register functions, given to the Partitions
    fn register_fn(&self) -> Box<WorkerListenerRegisterFunction>;

    /// Called from the Controller after the mobsim is finished. Shall finish remaining tasks,
    /// that can only be done after the iteration end.
    fn finish(&self);

    /// Actual scoring.
    fn scoring(&self);
}
