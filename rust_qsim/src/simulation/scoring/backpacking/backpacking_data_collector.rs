use crate::simulation::events::{
    ActivityEndEvent, ActivityStartEvent, EventTrait, LinkEnterEvent, PersonArrivalEvent,
    PersonDepartureEvent, PersonEntersVehicleEvent, PersonLeavesVehicleEvent,
    TeleportationArrivalEvent, VehicleEntersTrafficEvent, VehicleLeavesTrafficEvent,
};
use crate::simulation::framework_events::QSimId;
use crate::simulation::id::Id;
use crate::simulation::scenario::population::{InternalPerson, Population};
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::scoring::InternalScoringMessage;
use crate::simulation::scoring::backpacking::backpack::Backpack;
use crate::simulation::scoring::backpacking::backpacking_message_broker::BackpackingMessageBroker;
use nohash_hasher::{IntMap, IntSet};
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

pub struct BackpackingDataCollector {
    person_id2backpack: IntMap<Id<InternalPerson>, Backpack>,
    vehicle_id2person_ids: IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>>,
    rank: QSimId,

    message_broker: Arc<Mutex<BackpackingMessageBroker>>,

    // Vehicles that crossed into this partition in the current step but whose scoring mapping has
    // not arrived yet (it travels via the broker's AfterSimStep -> BeforeSimStep cycle, one step
    // behind the vehicle body). LinkEnterEvents for these vehicles are stored in
    // deferred_link_events and replayed once both the mapping and the backpack are available.
    pending_vehicles: IntSet<Id<InternalVehicle>>,
    deferred_link_events: Vec<LinkEnterEvent>,
}

impl BackpackingDataCollector {
    pub fn new(
        population: &Population,
        rank: QSimId,
        message_broker: Arc<Mutex<BackpackingMessageBroker>>,
    ) -> Arc<Mutex<Self>> {
        let data_collector = Arc::new(Mutex::new(Self {
            person_id2backpack: Default::default(),
            vehicle_id2person_ids: Default::default(),
            rank,
            message_broker,
            pending_vehicles: Default::default(),
            deferred_link_events: Default::default(),
        }));
        data_collector
            .lock()
            .unwrap()
            .generate_backpacks_for_population(&population);
        data_collector
    }

    fn generate_backpacks_for_population(&mut self, population: &Population) {
        for person in population.persons.keys() {
            self.person_id2backpack
                .insert(person.clone(), Backpack::new(person.clone(), self.rank));
        }
    }

    pub(crate) fn attach_senders(&mut self, senders: Vec<Sender<InternalScoringMessage>>) {
        self.message_broker.lock().unwrap().attach_senders(senders);
    }

    /// Drains the scoring message channel into this collector's maps, blocking on each pending
    /// wait_for_backpack/wait_for_vehicle registration until it is satisfied. Splits self into
    /// disjoint field borrows so the broker can write directly into the maps and clear
    /// pending_vehicles entries as vehicle mappings arrive. Called from the BeforeSimStep handler.
    pub(crate) fn drain_scoring_messages(&mut self) {
        let mut broker = self.message_broker.lock().unwrap();
        broker.recv_backpacks(
            &mut self.person_id2backpack,
            &mut self.vehicle_id2person_ids,
            &mut self.pending_vehicles,
        );
        broker.recv_vehicles(
            &mut self.person_id2backpack,
            &mut self.vehicle_id2person_ids,
            &mut self.pending_vehicles,
        );
    }

    /// Replays LinkEnterEvents that were buffered because the vehicle-to-person mapping had not
    /// yet arrived when they fired. Only called after drain_scoring_messages(), so both backpacks
    /// and vehicle mappings are guaranteed to be present.
    pub(crate) fn replay_deferred_link_events(&mut self) {
        for event in std::mem::take(&mut self.deferred_link_events) {
            self.handle_event(&event);
        }
    }

    pub(crate) fn remove_leaving_vehicles(
        &mut self,
        vehicle_id: &Id<InternalVehicle>,
    ) -> IntSet<Id<InternalPerson>> {
        // TODO Build a checker, so that it only allows missing entries for teleported modes
        self.vehicle_id2person_ids
            .remove(vehicle_id)
            .unwrap_or_else(|| {
                // warn!("Partition #{}: Tried to remove vehicle {}, which has no entry!", self.rank, vehicle_id);
                return IntSet::default();
            })
    }

    pub(crate) fn remove_leaving_backpack(&mut self, person_id: &Id<InternalPerson>) -> Backpack {
        self.person_id2backpack
            .remove(person_id)
            .unwrap_or_else(|| {
                panic!("Tried to remove an agent, for which no backpack is available!")
            })
    }

    pub(crate) fn get_vehicles_mut(
        &mut self,
    ) -> &mut IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>> {
        &mut self.vehicle_id2person_ids
    }

    pub(crate) fn get_pending_vehicles_mut(&mut self) -> &mut IntSet<Id<InternalVehicle>> {
        &mut self.pending_vehicles
    }

    /// This method's main purpose is to forward relevant events to the backpacks affected by given event.
    /// Events which do not affect the Backpack of any person will be ignored.
    /// TODO This method is quite clunky as there is no HasPersonId/HasVehicleId trait as there is in Java MATSim. Adding a trait could make the function much easier. Ask PH.
    pub(crate) fn handle_event(&mut self, event: &dyn EventTrait) {
        let affected_persons = if let Some(e) = event.as_any().downcast_ref::<LinkEnterEvent>() {
            match self.vehicle_id2person_ids.get(&e.vehicle) {
                Some(persons) => persons.iter().cloned().collect(),
                // The vehicle-to-person mapping arrives one step after the vehicle body (broker
                // AfterSimStep -> BeforeSimStep). Buffer for replay once both the mapping and the
                // backpack are present (see replay_deferred_link_events).
                None if self.pending_vehicles.contains(&e.vehicle) => {
                    self.deferred_link_events.push(e.clone());
                    return;
                }
                None => return, // untracked vehicle (e.g. teleportation)
            }
        } else if let Some(e) = event.as_any().downcast_ref::<PersonArrivalEvent>() {
            vec![e.person.clone()]
        } else if let Some(e) = event.as_any().downcast_ref::<PersonDepartureEvent>() {
            vec![e.person.clone()]
        } else if let Some(e) = event.as_any().downcast_ref::<ActivityStartEvent>() {
            vec![e.person.clone()]
        } else if let Some(e) = event.as_any().downcast_ref::<ActivityEndEvent>() {
            vec![e.person.clone()]
        } else if let Some(e) = event.as_any().downcast_ref::<TeleportationArrivalEvent>() {
            vec![e.person.clone()]
        } else if let Some(e) = event.as_any().downcast_ref::<PersonEntersVehicleEvent>() {
            vec![e.person.clone()]
        } else if let Some(e) = event.as_any().downcast_ref::<PersonLeavesVehicleEvent>() {
            vec![e.person.clone()]
        } else if let Some(e) = event.as_any().downcast_ref::<VehicleEntersTrafficEvent>() {
            self.vehicle_id2person_ids
                .get(&e.vehicle)
                .map(|persons| persons.iter().cloned().collect())
                .unwrap_or_default()
        } else if let Some(e) = event.as_any().downcast_ref::<VehicleLeavesTrafficEvent>() {
            self.vehicle_id2person_ids
                .get(&e.vehicle)
                .map(|persons| persons.iter().cloned().collect())
                .unwrap_or_default()
        } else {
            return;
        };

        affected_persons.into_iter().for_each(|person| {
            self.person_id2backpack
                .get_mut(&person)
                .unwrap()
                .handle_event(event);
        });
    }

    pub(crate) fn finish(&mut self) -> Population {
        let mut leaving_person_ids: Vec<_> = Vec::default();

        // Send foreign backpacks to their home partition
        for (person, backpack) in self.person_id2backpack.iter() {
            if backpack.get_starting_partion() != self.rank {
                leaving_person_ids.push(person.clone());
            }
        }

        for person_id in leaving_person_ids.drain(..) {
            let leaving_backpack = self.remove_leaving_backpack(&person_id);
            self.message_broker.lock().unwrap().add_leaving_backpack(
                leaving_backpack.get_starting_partion(),
                person_id,
                leaving_backpack,
            );
        }

        {
            let mut broker = self.message_broker.lock().unwrap();
            broker.finish_send_recv(
                &mut self.person_id2backpack,
                &mut self.vehicle_id2person_ids,
                &mut self.pending_vehicles,
            );
        }

        let persons: HashMap<Id<InternalPerson>, InternalPerson> = self
            .person_id2backpack
            .drain()
            .map(|(person_id, backpack)| (person_id, backpack.finish()))
            .collect();

        Population { persons }
    }
}
