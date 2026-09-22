use crate::simulation::events::{
    ActivityEndEvent, ActivityStartEvent, EventTrait, LinkEnterEvent, PersonArrivalEvent,
    PersonDepartureEvent, PersonEntersVehicleEvent, PersonLeavesVehicleEvent,
    TeleportationArrivalEvent, VehicleEntersTrafficEvent, VehicleLeavesTrafficEvent,
};
use crate::simulation::framework_events::QSimId;
use crate::simulation::id::Id;
use crate::simulation::scenario::population::{InternalPerson, Population};
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::scoring::homesending::homesending_message_broker::HomeSendingMessageBroker;
use crate::simulation::scoring::partial_plans::PartialPlan;
use nohash_hasher::{IntMap, IntSet};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub struct HomeSendingDataCollector {
    person_id2home_partition: IntMap<Id<InternalPerson>, QSimId>,
    rank: QSimId,

    message_broker: Arc<Mutex<HomeSendingMessageBroker>>,

    person_id2events: IntMap<Id<InternalPerson>, Vec<Box<dyn EventTrait>>>,
    vehicle_id2person_ids: IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>>,

    // Vehicles that crossed into this partition in the current step but whose scoring mapping has
    // not arrived yet (it travels via the broker's AfterSimStep -> BeforeSimStep cycle, one step
    // behind the vehicle body). LinkEnterEvents for these vehicles are stored in
    // deferred_link_events and replayed once the mapping is available.
    pending_vehicles: IntSet<Id<InternalVehicle>>,
    deferred_link_events: Vec<LinkEnterEvent>,
}

impl HomeSendingDataCollector {
    pub fn new(
        population: &Population,
        person_id2home_partition: IntMap<Id<InternalPerson>, QSimId>,
        rank: QSimId,
        message_broker: Arc<Mutex<HomeSendingMessageBroker>>,
    ) -> Arc<Mutex<Self>> {
        let data_collector = Arc::new(Mutex::new(Self {
            person_id2home_partition,
            rank,
            message_broker,
            person_id2events: IntMap::default(),
            vehicle_id2person_ids: IntMap::default(),
            pending_vehicles: IntSet::default(),
            deferred_link_events: Vec::new(),
        }));
        data_collector
            .lock()
            .unwrap()
            .generate_event_vectors_for_population(&population);
        data_collector
    }

    fn generate_event_vectors_for_population(&mut self, population: &Population) {
        for person in population.persons.keys() {
            self.person_id2events.insert(person.clone(), Vec::default());
        }
    }

    pub(crate) fn is_person_at_home(&self, person_id: &Id<InternalPerson>) -> bool {
        *self.person_id2home_partition.get(person_id).unwrap() == self.rank
    }

    pub(crate) fn add_arriving_vehicles(
        &mut self,
        arriving_vehicles: IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>>,
    ) {
        for (vehicle_id, persons) in arriving_vehicles {
            self.pending_vehicles.remove(&vehicle_id);
            self.vehicle_id2person_ids.insert(vehicle_id, persons);
        }
    }

    pub fn get_vehicles(&self) -> &IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>> {
        &self.vehicle_id2person_ids
    }

    pub(crate) fn get_vehicles_mut(
        &mut self,
    ) -> &mut IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>> {
        &mut self.vehicle_id2person_ids
    }

    pub(crate) fn get_persons(&self) -> &IntMap<Id<InternalPerson>, QSimId> {
        &self.person_id2home_partition
    }

    pub(crate) fn get_pending_vehicles_mut(&mut self) -> &mut IntSet<Id<InternalVehicle>> {
        &mut self.pending_vehicles
    }

    pub(crate) fn get_rank(&self) -> &QSimId {
        &self.rank
    }

    /// Replays LinkEnterEvents that were buffered because the vehicle-to-person mapping had not
    /// yet arrived when they fired. Only called from BeforeSimStep, after recv_vehicles() has
    /// run, so the vehicle mapping is guaranteed to be present.
    pub(crate) fn replay_deferred_link_events(&mut self) {
        for event in std::mem::take(&mut self.deferred_link_events) {
            self.handle_event(&event);
        }
    }

    /// Adds the events to the corresponding event vectors. Assumes, that events as well as the
    /// calls of this function are already sorted! Delivering unsorted events or blocks will cause
    /// the scoring module to panic!
    pub(crate) fn add_arriving_events(
        &mut self,
        person_id: Id<InternalPerson>,
        arriving_events: Vec<Box<dyn EventTrait>>,
    ) {
        self.person_id2events
            .get_mut(&person_id)
            .unwrap()
            .extend(arriving_events);
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

    /// This method's main purpose is to forward relevant events to the plan affected by given event.
    /// Events which do not affect the Plan of any person will be ignored.
    /// TODO This method is quite clunky as there is no HasPersonId/HasVehicleId trait as there is in Java MATSim. Adding a trait could make the function much easier. Ask PH.
    pub(crate) fn handle_event(&mut self, event: &dyn EventTrait) {
        let affected_persons: Vec<(Id<InternalPerson>, Box<dyn EventTrait>)> =
            if let Some(e) = event.as_any().downcast_ref::<LinkEnterEvent>() {
                match self.vehicle_id2person_ids.get(&e.vehicle) {
                    Some(persons) => persons
                        .iter()
                        .cloned()
                        .map(|person| (person, Box::new(e.clone()) as Box<dyn EventTrait>))
                        .collect::<Vec<_>>(),
                    // The vehicle-to-person mapping arrives one step after the vehicle body
                    // (broker AfterSimStep -> BeforeSimStep). Buffer for replay once the
                    // mapping is present (see replay_deferred_link_events).
                    None if self.pending_vehicles.contains(&e.vehicle) => {
                        self.deferred_link_events.push(e.clone());
                        return;
                    }
                    None => return, // untracked vehicle (e.g. teleportation)
                }
            } else if let Some(e) = event.as_any().downcast_ref::<PersonArrivalEvent>() {
                vec![(e.person.clone(), Box::new(e.clone()))]
            } else if let Some(e) = event.as_any().downcast_ref::<PersonDepartureEvent>() {
                vec![(e.person.clone(), Box::new(e.clone()))]
            } else if let Some(e) = event.as_any().downcast_ref::<ActivityStartEvent>() {
                vec![(e.person.clone(), Box::new(e.clone()))]
            } else if let Some(e) = event.as_any().downcast_ref::<ActivityEndEvent>() {
                vec![(e.person.clone(), Box::new(e.clone()))]
            } else if let Some(e) = event.as_any().downcast_ref::<TeleportationArrivalEvent>() {
                vec![(e.person.clone(), Box::new(e.clone()))]
            } else if let Some(e) = event.as_any().downcast_ref::<PersonEntersVehicleEvent>() {
                vec![(e.person.clone(), Box::new(e.clone()))]
            } else if let Some(e) = event.as_any().downcast_ref::<PersonLeavesVehicleEvent>() {
                vec![(e.person.clone(), Box::new(e.clone()))]
            } else if let Some(e) = event.as_any().downcast_ref::<VehicleEntersTrafficEvent>() {
                self.vehicle_id2person_ids
                    .get(&e.vehicle)
                    .into_iter()
                    .flatten()
                    .cloned()
                    .map(|person| (person, Box::new(e.clone()) as Box<dyn EventTrait>))
                    .collect::<Vec<_>>()
            } else if let Some(e) = event.as_any().downcast_ref::<VehicleLeavesTrafficEvent>() {
                self.vehicle_id2person_ids
                    .get(&e.vehicle)
                    .into_iter()
                    .flatten()
                    .cloned()
                    .map(|person| (person, Box::new(e.clone()) as Box<dyn EventTrait>))
                    .collect::<Vec<_>>()
            } else {
                return;
            };

        affected_persons
            .into_iter()
            .for_each(move |(person_id, boxed_event)| {
                let target = self.person_id2home_partition.get(&person_id).unwrap();

                if *target == self.rank {
                    // For full correctness, the events need to pass the arriving_events buffer in the
                    // message broker. Use the internal method to bypass the message sending
                    self.message_broker.lock().unwrap().push_events_on_block(
                        person_id,
                        self.rank,
                        vec![boxed_event],
                    );
                } else {
                    self.message_broker.lock().unwrap().add_leaving_event(
                        *target,
                        person_id,
                        boxed_event,
                    );
                }
            });
    }

    pub(crate) fn finish(&mut self) -> Population {
        let persons: HashMap<Id<InternalPerson>, InternalPerson> = self
            .person_id2events
            .drain()
            .map(|(person_id, events)| {
                let mut plan = PartialPlan::default();

                for event in events {
                    plan.handle_event(&*event);
                }

                (
                    person_id.clone(),
                    InternalPerson::new(person_id, plan.finish()),
                )
            })
            .collect();

        Population { persons }
    }
}
