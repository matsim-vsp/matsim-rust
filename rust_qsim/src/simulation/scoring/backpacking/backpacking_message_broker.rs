use crate::simulation::framework_events::QSimId;
use crate::simulation::id::Id;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::scoring::InternalScoringMessage;
use crate::simulation::scoring::backpacking::backpack::Backpack;
use nohash_hasher::{IntMap, IntSet};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::warn;

pub struct BackpackingMessageBroker {
    receiver: Receiver<InternalScoringMessage>,
    senders: Vec<Sender<InternalScoringMessage>>,
    rank: QSimId,

    leaving_buffer_backpacks: IntMap<QSimId, IntMap<Id<InternalPerson>, Backpack>>,
    leaving_buffer_vehicles:
        IntMap<QSimId, IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>>>,
    wait_backpacks: IntSet<Id<InternalPerson>>,
    wait_vehicles: IntSet<Id<InternalVehicle>>,
}

impl BackpackingMessageBroker {
    pub(crate) fn new(
        receiver: Receiver<InternalScoringMessage>,
        senders: Vec<Sender<InternalScoringMessage>>,
        rank: QSimId,
    ) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            receiver,
            senders,
            rank,
            leaving_buffer_backpacks: IntMap::default(),
            leaving_buffer_vehicles: IntMap::default(),
            wait_backpacks: IntSet::default(),
            wait_vehicles: IntSet::default(),
        }))
    }

    pub(crate) fn attach_senders(&mut self, senders: Vec<Sender<InternalScoringMessage>>) {
        self.senders.extend(senders);
    }

    pub(crate) fn add_leaving_backpack(
        &mut self,
        target: QSimId,
        agent_id: Id<InternalPerson>,
        backpack: Backpack,
    ) {
        self.leaving_buffer_backpacks
            .entry(target)
            .or_insert_with(|| IntMap::default())
            .insert(agent_id, backpack);
    }

    pub(crate) fn add_leaving_vehicle(
        &mut self,
        target: QSimId,
        vehicle_id: Id<InternalVehicle>,
        passengers: IntSet<Id<InternalPerson>>,
    ) {
        self.leaving_buffer_vehicles
            .entry(target)
            .or_insert_with(|| IntMap::default())
            .insert(vehicle_id, passengers);
    }

    pub(crate) fn wait_for_backpack(&mut self, person_id: Id<InternalPerson>) {
        self.wait_backpacks.insert(person_id);
    }

    pub(crate) fn wait_for_vehicle(&mut self, vehicle_id: Id<InternalVehicle>) {
        self.wait_vehicles.insert(vehicle_id);
    }

    pub(crate) fn send(&mut self) {
        for (target, vehicles) in self.leaving_buffer_vehicles.drain() {
            let msg = InternalScoringMessage {
                from_process: self.rank,
                to_process: target,
                message: Box::new(VehicleMessage { vehicles }),
            };

            self.senders[target as usize].send(msg).unwrap_or_else(|e| {
                panic!(
                    "Error sending EventMessage to rank {} with error {}",
                    target, e
                )
            });
        }

        for (target, backpacks) in self.leaving_buffer_backpacks.drain() {
            let msg = InternalScoringMessage {
                from_process: self.rank,
                to_process: target,
                message: Box::new(BackpackingMessage { backpacks }),
            };

            self.senders[target as usize].send(msg).unwrap_or_else(|e| {
                panic!(
                    "Error sending EventMessage to rank {} with error {}",
                    target, e
                )
            });
        }
    }

    /// General receive logic for backpacking messages: writes an incoming scoring message directly
    /// into the data collector's maps. Vehicle mappings also clear their entry from
    /// `pending_vehicles` so `deferred_link_events` can be replayed. Called by `recv_backpacks`,
    /// `recv_vehicles`, and `finish_send_recv`.
    fn recv(
        received_msg: InternalScoringMessage,
        person_id2backpack: &mut IntMap<Id<InternalPerson>, Backpack>,
        vehicle_id2person_ids: &mut IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>>,
        pending_vehicles: &mut IntSet<Id<InternalVehicle>>,
    ) {
        let boxed_any = received_msg.message.into_any();

        match () {
            _ if boxed_any.is::<VehicleMessage>() => {
                let m = boxed_any.downcast::<VehicleMessage>().unwrap();
                for (vehicle_id, persons) in m.vehicles {
                    pending_vehicles.remove(&vehicle_id);
                    vehicle_id2person_ids.insert(vehicle_id, persons);
                }
            }
            _ if boxed_any.is::<BackpackingMessage>() => {
                let m = boxed_any.downcast::<BackpackingMessage>().unwrap();
                person_id2backpack.extend(m.backpacks);
            }
            _ => {
                panic!("Received unknown message type!");
            }
        }
    }

    /// Called upon a PersonEntersPartitionEvent. It checks whether the backpacks of arrived
    /// persons are present. If not, the function blocks the thread until said backpack has arrived.
    /// This function is called once for each person, that has entered the partition and assures
    /// that the scoring module is only using backpacks which are present.
    pub(crate) fn recv_backpacks(
        &mut self,
        person_id2backpack: &mut IntMap<Id<InternalPerson>, Backpack>,
        vehicle_id2person_ids: &mut IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>>,
        pending_vehicles: &mut IntSet<Id<InternalVehicle>>,
    ) {
        let pending_backpacks = self.wait_backpacks.drain().collect::<Vec<_>>();
        for person_id in pending_backpacks {
            // Check the map: the backpack may have been received in an earlier
            // recv_backpacks/recv_vehicles/finish_send_recv call while we were waiting for a
            // different person, and would already be in person_id2backpack.
            if person_id2backpack.contains_key(&person_id) {
                continue;
            }

            // Recv backpacks, until the backpack for the current person arrives
            while !person_id2backpack.contains_key(&person_id) {
                match self.receiver.recv_timeout(Duration::from_secs(10)) {
                    Ok(received_msg) => Self::recv(
                        received_msg,
                        person_id2backpack,
                        vehicle_id2person_ids,
                        pending_vehicles,
                    ),
                    Err(_) => warn!(
                        "Partition #{}: stuck waiting for backpack of person {:?}",
                        self.rank, person_id
                    ),
                }
            }
        }
    }

    /// Called upon a VehicleEntersPartitionEvent. It checks whether the passenger info of arrived
    /// vehicles is present. If not, the function blocks the thread until said passenger info has arrived.
    /// This function is called once for each vehicle, that has entered the partition and assures
    /// that the scoring module is only using passenger info which is present.
    pub(crate) fn recv_vehicles(
        &mut self,
        person_id2backpack: &mut IntMap<Id<InternalPerson>, Backpack>,
        vehicle_id2person_ids: &mut IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>>,
        pending_vehicles: &mut IntSet<Id<InternalVehicle>>,
    ) {
        let pending = self.wait_vehicles.drain().collect::<Vec<_>>();
        for vehicle_id in pending {
            // Same reasoning as recv_backpacks: the mapping may already be installed from an
            // earlier call.
            if vehicle_id2person_ids.contains_key(&vehicle_id) {
                continue;
            }

            while !vehicle_id2person_ids.contains_key(&vehicle_id) {
                match self.receiver.recv_timeout(Duration::from_secs(10)) {
                    Ok(received_msg) => Self::recv(
                        received_msg,
                        person_id2backpack,
                        vehicle_id2person_ids,
                        pending_vehicles,
                    ),
                    Err(_) => warn!(
                        "Partition #{}: stuck waiting for vehicle mapping of vehicle {:?}",
                        self.rank, vehicle_id
                    ),
                }
            }
        }
    }

    /// The last send-recv operation before the iteration ends.
    /// Since there are no Partition Events that finalize the iteration, the certification is done
    /// manually by sending O(n^2) finish-messages.
    pub(crate) fn finish_send_recv(
        &mut self,
        person_id2backpack: &mut IntMap<Id<InternalPerson>, Backpack>,
        vehicle_id2person_ids: &mut IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>>,
        pending_vehicles: &mut IntSet<Id<InternalVehicle>>,
    ) {
        self.send();

        // Send a finish message to all partitions
        for target in 0..self.senders.len() {
            if target == self.rank as usize {
                continue;
            }

            let msg = InternalScoringMessage {
                from_process: self.rank,
                to_process: target as QSimId,
                message: Box::new(FinishMessage {}),
            };

            self.senders[target].send(msg).unwrap_or_else(|e| {
                panic!(
                    "Error sending EventMessage to rank {} with error {}",
                    target, e
                )
            });
        }

        let mut finished_partitions: IntSet<QSimId> = IntSet::default();
        while finished_partitions.len() < self.senders.len() - 1 {
            let received_msg = self.receiver.recv().expect("Error receiving message");
            let boxed_any = received_msg.message.as_any();

            match () {
                _ if boxed_any.is::<FinishMessage>() => {
                    // Add finish message to set for break condition
                    finished_partitions.insert(received_msg.from_process);
                }
                _ => {
                    // Process arriving data directly into the collector's maps.
                    Self::recv(
                        received_msg,
                        person_id2backpack,
                        vehicle_id2person_ids,
                        pending_vehicles,
                    );
                }
            }
        }
    }
}

struct VehicleMessage {
    vehicles: IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>>,
}

struct BackpackingMessage {
    backpacks: IntMap<Id<InternalPerson>, Backpack>,
}

struct FinishMessage {}
