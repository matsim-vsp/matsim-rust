use crate::simulation::messaging::messages::{InternalSimMessage, InternalSyncMessage};
use crate::simulation::messaging::sim_communication::SimCommunicator;
use crate::simulation::time::Tick;
use nohash_hasher::{IntMap, IntSet};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Barrier};

pub struct ChannelSimCommunicator {
    receiver: Receiver<InternalSimMessage>,
    senders: Vec<Sender<InternalSimMessage>>,
    rank: u32,
    barrier: Arc<Barrier>,
}

impl SimCommunicator for ChannelSimCommunicator {
    fn send_receive_vehicles<F>(
        &self,
        vehicles: IntMap<u32, InternalSyncMessage>,
        expected_vehicle_messages: &mut IntSet<u32>,
        now: Tick,
        mut on_msg: F,
    ) where
        F: FnMut(InternalSyncMessage),
    {
        // send messages to everyone
        for (target, msg) in vehicles {
            let sender = self.senders.get(target as usize).unwrap();
            sender
                .send(InternalSimMessage::from_sync_message(msg))
                .unwrap_or_else(|e| {
                    panic!(
                        "Error while sending message to rank {} with error {}",
                        target, e
                    )
                });
        }

        // receive messages from everyone
        while !expected_vehicle_messages.is_empty() {
            let received_msg = self
                .receiver
                .recv()
                .expect("Error while receiving messages")
                .sync_message();
            let from_rank = received_msg.from_process();

            // If a message was received from a neighbor partition for this very time step, remove
            // that partition from expected messages which indicates which partitions we are waiting
            // for
            if received_msg.time() == now {
                expected_vehicle_messages.remove(&from_rank);
            }

            // publish the received message to the message broker
            on_msg(received_msg);
        }
    }

    fn barrier(&self) {
        self.barrier.wait();
    }

    fn rank(&self) -> u32 {
        self.rank
    }
}

impl ChannelSimCommunicator {
    pub fn create_n_2_n(num_parts: u32) -> Vec<ChannelSimCommunicator> {
        let mut senders: Vec<_> = Vec::new();
        let mut comms: Vec<_> = Vec::new();
        let barrier = Arc::new(Barrier::new(num_parts as usize));

        for rank in 0..num_parts {
            let (sender, receiver) = channel();
            let comm = ChannelSimCommunicator {
                receiver,
                senders: vec![],
                rank,
                barrier: barrier.clone(),
            };
            senders.push(sender);
            comms.push(comm);
        }

        for comm in &mut comms {
            for sender in &senders {
                comm.senders.push(sender.clone());
            }
        }

        comms
    }

    pub fn rank(&self) -> u32 {
        self.rank
    }
}
