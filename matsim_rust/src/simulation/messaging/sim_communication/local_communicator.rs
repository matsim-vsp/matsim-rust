use crate::simulation::messaging::messages::{InternalSimMessage, InternalSyncMessage};
use crate::simulation::messaging::sim_communication::SimCommunicator;
use crate::simulation::messaging::sim_communication::barrier::{Barrier, WaitPolicy};
use crate::simulation::time::Tick;
use hotpath::wrap::std::sync::mpsc::{Receiver, Sender};
use nohash_hasher::{IntMap, IntSet};
use std::sync::Arc;
use std::sync::mpsc::{TryRecvError, channel};
use std::time::{Duration, Instant};

/// How long a partition polls for the next message before it blocks on the channel. Blocking
/// parks the thread, so that the sender has to wake it up with a syscall and the receiver has to
/// wait until the scheduler runs it again. With few agents, a time step takes only a few
/// microseconds, so these costs dominate the step. Polling avoids them, but keeps a core busy while
/// waiting. Thus, the polling is bounded, and slow neighbors are still waited for by blocking.
/// The barrier at which all partitions wait in every time step with `global_sync` polls for as long
/// before it sleeps, for the same reasons.
const POLL_BEFORE_BLOCKING: Duration = Duration::from_micros(50);
/// How long a partition busy-waits while polling. Afterwards, it yields its core between polls.
/// Busy-waiting notices a message fastest. But with more partitions than cores, it keeps the
/// partitions that have to send the awaited messages from running. The barrier busy-waits for as
/// long, too.
const SPIN_BEFORE_YIELDING: Duration = Duration::from_micros(5);

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
            let received_msg = self.recv().sync_message();
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

    fn receive_pending(&self) -> Vec<InternalSyncMessage> {
        self.receiver
            .try_iter()
            .filter_map(|msg| match msg {
                InternalSimMessage::Sync(msg) => Some(msg),
                InternalSimMessage::Barrier => None,
            })
            .collect()
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
        let barrier = Arc::new(Barrier::with_policy(
            num_parts as usize,
            WaitPolicy {
                spin: SPIN_BEFORE_YIELDING,
                yield_until: POLL_BEFORE_BLOCKING,
            },
        ));

        for rank in 0..num_parts {
            // if the feature `hotpath` is deactivated, this is a no-op!
            let (sender, receiver) = hotpath::channel!(
                channel::<InternalSimMessage>(),
                label = "partition-messages"
            );
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

    /// Returns the next message. Polls for up to `POLL_BEFORE_BLOCKING` before it blocks.
    fn recv(&self) -> InternalSimMessage {
        let start = Instant::now();
        loop {
            match self.receiver.try_recv() {
                Ok(msg) => return msg,
                Err(TryRecvError::Empty) => {
                    let waited = start.elapsed();
                    if waited < SPIN_BEFORE_YIELDING {
                        std::hint::spin_loop();
                    } else if waited < POLL_BEFORE_BLOCKING {
                        std::thread::yield_now();
                    } else {
                        break;
                    }
                }
                // Block, which also reports a disconnected channel.
                Err(TryRecvError::Disconnected) => break,
            }
        }
        self.receiver
            .recv()
            .expect("Error while receiving messages")
    }
}

#[cfg(test)]
mod tests {
    use crate::simulation::messaging::messages::InternalSyncMessage;
    use crate::simulation::messaging::sim_communication::SimCommunicator;
    use crate::simulation::messaging::sim_communication::local_communicator::ChannelSimCommunicator;
    use crate::simulation::time::Tick;
    use nohash_hasher::{IntMap, IntSet};

    /// Rank 1 does not expect messages from rank 0, so it does not read the message while
    /// sending and receiving. Only `receive_pending` returns it.
    #[test]
    fn receive_pending_returns_unread_messages() {
        let mut comms = ChannelSimCommunicator::create_n_2_n(2);
        let comm_1 = comms.pop().unwrap();
        let comm_0 = comms.pop().unwrap();

        let message = InternalSyncMessage::new(Tick::new(7), 0, 1);
        comm_0.send_receive_vehicles(
            IntMap::from_iter([(1, message)]),
            &mut IntSet::default(),
            Tick::new(7),
            |_| panic!("Rank 0 expects no messages"),
        );
        let mut received = vec![];
        comm_1.send_receive_vehicles(
            IntMap::default(),
            &mut IntSet::default(),
            Tick::new(7),
            |msg| received.push(msg),
        );
        assert!(received.is_empty());

        let pending = comm_1.receive_pending();
        assert_eq!(1, pending.len());
        assert_eq!(0, pending[0].from_process());
        assert_eq!(Tick::new(7), pending[0].time());

        assert!(comm_1.receive_pending().is_empty());
        assert!(comm_0.receive_pending().is_empty());
    }
}
