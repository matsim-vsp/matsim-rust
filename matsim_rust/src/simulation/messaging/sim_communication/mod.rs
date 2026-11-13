use crate::simulation::messaging::messages::InternalSyncMessage;
use crate::simulation::time::Tick;
use nohash_hasher::{IntMap, IntSet};

pub mod barrier;
pub mod local_communicator;
pub mod message_broker;

pub trait SimCommunicator {
    /// Sends each message to the partition it is keyed by. Does not block.
    fn send_vehicles(&self, vehicles: IntMap<u32, InternalSyncMessage>);

    /// Passes received messages to `on_msg`. Blocks until a message of time step `now` has arrived
    /// from each partition in `expected_vehicle_messages`, and then also passes all messages that
    /// have arrived so far, e.g. from other partitions.
    fn receive_vehicles<F>(
        &self,
        expected_vehicle_messages: &mut IntSet<u32>,
        now: Tick,
        on_msg: F,
    ) where
        F: FnMut(InternalSyncMessage);

    /// Returns all messages that have been received but not read yet, without blocking.
    fn receive_pending(&self) -> Vec<InternalSyncMessage>;

    fn barrier(&self);

    fn rank(&self) -> u32;
}
