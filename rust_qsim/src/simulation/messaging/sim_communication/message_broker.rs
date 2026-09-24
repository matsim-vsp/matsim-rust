use crate::simulation::agents::SimulationAgentLogic;
use crate::simulation::id::Id;
use crate::simulation::messaging::messages::{InternalSyncMessage, ScheduledTeleportation};
use crate::simulation::messaging::sim_communication::SimCommunicator;
use crate::simulation::network::sim_network::{SimNetworkPartition, StorageUpdate};
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::time::Tick;
use crate::simulation::vehicles::SimulationVehicle;
use ahash::HashMapExt;
use nohash_hasher::{IntMap, IntSet};
use std::collections::BinaryHeap;
use std::rc::Rc;

pub struct NetMessageBroker<C>
where
    C: SimCommunicator,
{
    communicator: Rc<C>,
    out_messages: IntMap<u32, InternalSyncMessage>,
    in_messages: BinaryHeap<InternalSyncMessage>,
    // store link mapping with internal ids instead of id structs, because vehicles only store internal
    // ids (usize) and this way we don't need to keep a reference to the global network's id store
    link_mapping: IntMap<Id<Link>, u32>,
    neighbors: IntSet<u32>,
    global_sync: bool,
}

impl<C> NetMessageBroker<C>
where
    C: SimCommunicator,
{
    pub fn new(
        comm: Rc<C>,
        global_network: &Network,
        net: &SimNetworkPartition,
        global_sync: bool,
    ) -> Self {
        let neighbors = net.neighbors().iter().copied().collect();
        let link_mapping = global_network
            .links()
            .iter()
            .map(|link| (link.id.clone(), link.partition))
            .collect();

        Self {
            communicator: comm,
            out_messages: Default::default(),
            in_messages: Default::default(),
            link_mapping,
            neighbors,
            global_sync,
        }
    }

    pub fn rank(&self) -> u32 {
        self.communicator.rank()
    }

    pub fn rank_for_link(&self, link_id: &Id<Link>) -> u32 {
        *self.link_mapping.get(link_id).unwrap()
    }

    pub fn add_veh(&mut self, vehicle: SimulationVehicle, now: impl Into<Tick>) {
        let now = now.into();
        let link_id = vehicle.curr_link_id().unwrap();
        let partition = *self.link_mapping.get(link_id).unwrap();
        let rank = self.rank();
        let message = self
            .out_messages
            .entry(partition)
            .or_insert_with(|| InternalSyncMessage::new(now, rank, partition));
        message.add_veh(vehicle);
    }

    pub(crate) fn add_teleportation(
        &mut self,
        teleportation: ScheduledTeleportation,
        now: impl Into<Tick>,
    ) {
        let now = now.into();
        let link_id = teleportation.agent().curr_link_id().unwrap();
        let partition = *self.link_mapping.get(link_id).unwrap();
        let rank = self.rank();
        let message = self
            .out_messages
            .entry(partition)
            .or_insert_with(|| InternalSyncMessage::new(now, rank, partition));
        message.add_teleportation(teleportation);
    }

    pub fn add_cap_update(&mut self, cap: StorageUpdate, now: impl Into<Tick>) {
        let now = now.into();
        let rank = self.rank();
        let message = self
            .out_messages
            .entry(cap.from_part)
            .or_insert_with(|| InternalSyncMessage::new(now, rank, cap.from_part));
        message.add_storage_cap(StorageUpdate {
            link_id: cap.link_id,
            from_part: rank,
            released: cap.released,
        });
    }

    pub fn send_recv(&mut self, now: impl Into<Tick>) -> Vec<InternalSyncMessage> {
        let now = now.into();
        let vehicles = self.prepare_send_recv(now);

        let mut result: Vec<InternalSyncMessage> = Vec::new();
        let mut expected_vehicle_messages = self.neighbors.clone();

        self.pop_from_cache(&mut expected_vehicle_messages, &mut result, now);

        // get refs to communicator and in_messages, so that we can have mut refs to both, instead
        // of passing self around, which would lock them because we would hold multiple mut refs to self
        let comm_ref = &self.communicator;
        let in_msgs_ref = &mut self.in_messages;

        // If enabled, wait for all processes to send their messages at the same time step.
        // With external functionality like a DRT service, this makes it much easier to produce deterministic results.
        // However, it also means that the simulation will be slower.
        if self.global_sync {
            comm_ref.barrier();
        }

        comm_ref.send_receive_vehicles(vehicles, &mut expected_vehicle_messages, now, |msg| {
            Self::handle_incoming_msg(msg, &mut result, in_msgs_ref, now)
        });

        result
    }

    fn handle_incoming_msg(
        msg: InternalSyncMessage,
        result: &mut Vec<InternalSyncMessage>,
        in_messages: &mut BinaryHeap<InternalSyncMessage>,
        now: Tick,
    ) {
        if msg.time() <= now {
            result.push(msg);
        } else {
            in_messages.push(msg);
        }
    }

    fn pop_from_cache(
        &mut self,
        expected_messages: &mut IntSet<u32>,
        messages: &mut Vec<InternalSyncMessage>,
        now: Tick,
    ) {
        while let Some(msg) = self.in_messages.peek() {
            if msg.time() <= now {
                expected_messages.remove(&msg.from_process());
                messages.push(self.in_messages.pop().unwrap())
            } else {
                break; // important! otherwise this is an infinite loop
            }
        }
    }

    fn prepare_send_recv(&mut self, now: Tick) -> IntMap<u32, InternalSyncMessage> {
        let capacity = self.out_messages.len();
        let mut messages =
            std::mem::replace(&mut self.out_messages, IntMap::with_capacity(capacity));

        for partition in &self.neighbors {
            let neighbor_rank = *partition;
            messages
                .entry(neighbor_rank)
                .or_insert_with(|| InternalSyncMessage::new(now, self.rank(), neighbor_rank));
        }
        messages
    }
}

#[cfg(test)]
mod tests {
    use crate::simulation::agents::{AgentEvent, EnvironmentalEventObserver};
    use crate::simulation::config;
    use crate::simulation::id::Id;
    use crate::simulation::messaging::sim_communication::local_communicator::ChannelSimCommunicator;
    use crate::simulation::messaging::sim_communication::message_broker::NetMessageBroker;
    use crate::simulation::network::sim_network::SimNetworkPartition;
    use crate::simulation::network::sim_network::StorageUpdate;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Network, Node};
    use crate::simulation::time::SimTime;
    use crate::simulation::vehicles::SimulationVehicle;
    use crate::test_utils::create_agent;
    use macros::deterministic_id_test;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    #[deterministic_id_test]
    fn send_recv_empty_msgs() {
        let sends = Arc::new(AtomicUsize::new(0));

        execute_test(move |communicator| {
            let mut broker = create_net_message_broker(communicator);

            sends.fetch_add(1, Ordering::Relaxed);
            let result = broker.send_recv(0);

            // all threads should block on receive. Therefore, the send count should be equal to 3, as
            // 0,1 have 3 as a remote neighbor. It is possible for 0 and 1 to move on before 3 has
            // increased the send count. Most of the time it should be 4 though. I don't know how
            // good this test is in this case. I guess the remaining asserts are also fine.
            assert!(
                3 <= sends.load(Ordering::Relaxed),
                "# {} Failed on send count of {}",
                broker.rank(),
                sends.load(Ordering::Relaxed)
            );

            // the different partitions expect varying numbers of sync messags.
            match broker.rank() {
                0 | 1 => assert_eq!(2, result.len()),
                2 => assert_eq!(3, result.len()),
                3 => assert_eq!(1, result.len()),
                _ => panic!("Not expecting this rank!"),
            };

            for msg in result {
                assert!(msg.vehicles().is_empty());
            }
        });
    }

    /// This test moves a vehicle from partition 0 to 2 and then to partition 3. The test involves
    /// Two send_recv steps.
    #[deterministic_id_test]
    fn send_recv_local_vehicle_msg() {
        execute_test(|communicator| {
            let mut broker = create_net_message_broker(communicator);

            // place vehicle into partition 0
            if broker.rank() == 0 {
                let agent = create_agent(0, vec!["2", "6"]);
                let vehicle = SimulationVehicle::from_parts(0, 0, 0., 0., agent);
                broker.add_veh(vehicle, 0);
            }

            // do sync step for all partitions
            let result_0 = broker.send_recv(0);

            // we expect broker 2 to have received the vehicle all other messages should have no vehicles
            if broker.rank() == 2 {
                let mut msg = result_0
                    .into_iter()
                    .find(|msg| msg.from_process() == 0)
                    .unwrap();
                assert_eq!(0, msg.time());
                assert_eq!(1, msg.vehicles().len());
                let mut vehicle = msg.vehicles_mut().remove(0);
                vehicle.notify_event(&mut AgentEvent::LeftLink(), SimTime::default());
                broker.add_veh(vehicle, 1);
            } else {
                for msg in result_0 {
                    if !msg.vehicles().is_empty() {
                        panic!("Process {} received vehicles: {msg:?}", broker.rank());
                    }
                }
            }

            // do second sync step for all partitions
            let result_1 = broker.send_recv(1);

            // we expect broker 3 to have received the vehicle all other messages should have no vehicles
            if broker.rank() == 3 {
                let mut msg = result_1
                    .into_iter()
                    .find(|msg| msg.from_process() == 2)
                    .unwrap();
                assert_eq!(1, msg.time());
                assert_eq!(1, msg.vehicles().len());
                let vehicle = msg.vehicles_mut().remove(0);
                broker.add_veh(vehicle, 1);
            } else {
                for msg in result_1 {
                    assert!(msg.vehicles().is_empty());
                }
            }
        });
    }

    #[deterministic_id_test]
    fn send_recv_remote_message() {
        execute_test(|communicator| {
            let mut broker = create_net_message_broker(communicator);

            // place vehicle into partition 0 with a future timestamp
            if broker.rank() == 0 {
                let agent = create_agent(0, vec!["6"]);
                let vehicle = SimulationVehicle::from_parts(0, 0, 0., 0., agent);
                broker.add_veh(vehicle, 1);
            }

            // do sync step for all partitions for "current" time step
            let result_0 = broker.send_recv(0);

            for msg in result_0 {
                assert_eq!(0, msg.time());
                assert!(msg.vehicles().is_empty());
            }

            // do sync step for all partitions for "future" time step
            let result_1 = broker.send_recv(1);

            for msg in result_1 {
                if broker.rank() == 3 && msg.from_process() == 0 {
                    assert_eq!(1, msg.vehicles().len());
                }

                assert_eq!(1, msg.time());
            }
        });
    }

    #[deterministic_id_test]
    fn send_recv_local_and_remote_msg() {
        execute_test(|communicator| {
            let mut broker = create_net_message_broker(communicator);

            if broker.rank() == 0 {
                // place vehicle into partition 0 with a future timestamp with remote destination
                let agent = create_agent(0, vec!["6"]);
                let vehicle = SimulationVehicle::from_parts(0, 0, 0., 0., agent);
                broker.add_veh(vehicle, 1);
            }

            // do sync step for all partitions for "current" time step
            let result_0 = broker.send_recv(0);

            for msg in result_0 {
                assert_eq!(0, msg.time());
                assert!(msg.vehicles().is_empty());
            }

            if broker.rank() == 2 {
                // place vehicle into partition 2 with a current timestamp with neighbor destination
                let agent = create_agent(1, vec!["6"]);
                let vehicle = SimulationVehicle::from_parts(1, 0, 0., 0., agent);
                broker.add_veh(vehicle, 1);
            }

            // do sync step for all partitions for "future" time step
            let result_1 = broker.send_recv(1);

            for msg in result_1 {
                if broker.rank() == 3 && msg.from_process() == 0 {
                    assert_eq!(1, msg.vehicles().len());
                    assert_eq!("0", msg.vehicles().first().unwrap().id().external());
                } else if broker.rank() == 3 && msg.from_process() == 2 {
                    assert_eq!(1, msg.vehicles().len());
                    assert_eq!("1", msg.vehicles().first().unwrap().id().external());
                } else {
                    assert_eq!(0, msg.vehicles().len());
                }

                assert_eq!(1, msg.time());
            }
        });
    }

    fn create_net_message_broker(
        communicator: ChannelSimCommunicator,
    ) -> NetMessageBroker<ChannelSimCommunicator> {
        let rank = communicator.rank();
        let mut config = config::Config::default();
        config.qsim_mut().start_time = 0;
        config.qsim_mut().end_time = 0;
        config.qsim_mut().ticks_per_second = 1;
        config.qsim_mut().sample_size = 0.0;
        config.qsim_mut().stuck_threshold = 0;
        config.qsim_mut().main_modes.clear();
        let network = create_network();
        let partition = SimNetworkPartition::from_network_for_test(&network, rank, &config);

        assert_eq!(partition.get_node_ids().len(), 1);

        if rank == 0 {
            assert_eq!(partition.get_link_ids().len(), 2);
        } else if rank == 1 {
            assert_eq!(partition.get_link_ids().len(), 2);
        } else if rank == 2 {
            assert_eq!(partition.get_link_ids().len(), 3);
        } else if rank == 3 {
            assert_eq!(partition.get_link_ids().len(), 1);
        }

        NetMessageBroker::new(Rc::new(communicator), &create_network(), &partition, false)
    }

    #[deterministic_id_test]
    fn send_recv_storage_cap() {
        execute_test(|communicator| {
            let mut broker = create_net_message_broker(communicator);
            // add a storage cap message for link 4, which connects parts 1 -> 2
            if broker.rank() == 2 {
                broker.add_cap_update(
                    StorageUpdate {
                        link_id: Id::create("4"),
                        released: 42.0,
                        from_part: 1,
                    },
                    0,
                );
            }

            // do sync step
            let result_0 = broker.send_recv(0);

            // broker 1 should have received the StorageCap message
            // all others should not have any storage cap messages.
            for msg in result_0 {
                if msg.from_process() == 2 && msg.to_process() == 1 {
                    assert_eq!(1, msg.storage_capacities().len(), "{msg:?}")
                } else {
                    assert!(msg.storage_capacities().is_empty(), "{msg:?}");
                }
            }
        });
    }

    fn execute_test<F>(test: F)
    where
        F: Fn(ChannelSimCommunicator) + Send + Sync + 'static,
    {
        let network = create_network();
        let communicators = ChannelSimCommunicator::create_n_2_n(network.nodes().len() as u32);

        let mut join_handles = Vec::new();

        let test_ref = Arc::new(test);

        for c in communicators {
            let cloned_test_ref = test_ref.clone();
            let handle = thread::spawn(move || cloned_test_ref(c));
            join_handles.push(handle)
        }

        for handle in join_handles {
            handle.join().expect("Some thread crashed");
        }
    }

    /// use example with four partitions
    /// 0 --- 2 --- 3
    /// |   /
    /// 1--/
    /// 0, 1, 2, are neighbors, 3 is only neighbor to 2
    fn create_network() -> Network {
        let mut result = Network::new();
        result.add_node(create_node(0, 0));
        result.add_node(create_node(1, 1));
        result.add_node(create_node(2, 2));
        result.add_node(create_node(3, 3));

        // connection 0 <-> 1
        result.add_link(create_link(0, 0, 1, 1));
        result.add_link(create_link(1, 1, 0, 0));

        // connection 0 <-> 2
        result.add_link(create_link(2, 0, 2, 2));
        result.add_link(create_link(3, 2, 0, 0));

        // connection 1 <-> 2
        result.add_link(create_link(4, 1, 2, 2));
        result.add_link(create_link(5, 2, 1, 1));

        // connection 2 <-> 3
        result.add_link(create_link(6, 2, 3, 3));
        result.add_link(create_link(7, 3, 2, 2));

        result
    }

    fn create_node(id: u64, partition: u32) -> Node {
        Node::new(
            Id::create(&id.to_string()),
            Coordinate::default(),
            partition,
            1,
        )
    }

    fn create_link(id: u64, from: u64, to: u64, partition: u32) -> Link {
        Link {
            id: Id::create(&id.to_string()),
            from: Id::create(&from.to_string()),
            to: Id::create(&to.to_string()),
            length: 10.0,
            capacity: 1.0,
            freespeed: 1.0,
            permlanes: 0.0,
            modes: Default::default(),
            partition,
            attributes: Default::default(),
        }
    }
}
