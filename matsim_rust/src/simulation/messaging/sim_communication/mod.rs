use crate::simulation::messaging::messages::InternalSyncMessage;
use crate::simulation::time::Tick;
use nohash_hasher::{IntMap, IntSet};

pub mod local_communicator;
pub mod message_broker;

pub trait SimCommunicator {
    fn send_receive_vehicles<F>(
        &self,
        vehicles: IntMap<u32, InternalSyncMessage>,
        expected_vehicle_messages: &mut IntSet<u32>,
        now: Tick,
        on_msg: F,
    ) where
        F: FnMut(InternalSyncMessage);

    fn barrier(&self);

    fn rank(&self) -> u32;
}
