use crate::simulation::events::{
    ActivityEndEvent, ActivityStartEvent, EventHandlerRegisterFn, EventTrait, EventsManager,
    LinkEnterEvent, PersonArrivalEvent, PersonDepartureEvent, PersonEntersVehicleEvent,
    PersonLeavesVehicleEvent, TeleportationArrivalEvent, VehicleEntersTrafficEvent,
    VehicleLeavesTrafficEvent,
};
use crate::simulation::id::Id;
use crate::simulation::scenario::population::{InternalPerson, InternalPlan, Population};
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::scoring::mapping::mapping_message_broker::MappingCollectorMessageBroker;
use std::collections::HashMap;
use std::mem::take;
use std::sync::{Arc, Mutex};
pub struct MappingDataForwarder {
    person_hash_function: Box<dyn Fn(Id<InternalPerson>) -> u32 + Send>,
    vehicle_hash_function: Box<dyn Fn(Id<InternalVehicle>) -> u32 + Send>,
    num_partitions: u32,

    person_id2internal_person: HashMap<Id<InternalPerson>, InternalPerson>,
    message_broker: Arc<Mutex<MappingCollectorMessageBroker>>,
}

impl MappingDataForwarder {
    pub fn new(
        person_hash_function: Box<dyn Fn(Id<InternalPerson>) -> u32 + Send>,
        vehicle_hash_function: Box<dyn Fn(Id<InternalVehicle>) -> u32 + Send>,
        num_partitions: u32,
        mapping_collector_message_broker: Arc<Mutex<MappingCollectorMessageBroker>>,
    ) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            person_hash_function,
            vehicle_hash_function,
            num_partitions,
            person_id2internal_person: HashMap::new(),
            message_broker: mapping_collector_message_broker,
        }))
    }

    fn forward_person_event(&self, person_id: Id<InternalPerson>, event: Box<dyn EventTrait>) {
        let target = (self.person_hash_function)(person_id.clone()) + self.num_partitions;
        self.message_broker
            .lock()
            .unwrap()
            .add_leaving_person_event(target, person_id.clone(), event);
    }

    fn forward_vehicle_event(&self, vehicle_id: Id<InternalVehicle>, event: Box<dyn EventTrait>) {
        let target = (self.vehicle_hash_function)(vehicle_id.clone()) + self.num_partitions;
        self.message_broker
            .lock()
            .unwrap()
            .add_leaving_vehicle_event(target, vehicle_id.clone(), event);
    }

    pub(crate) fn add_arriving_plan(
        &mut self,
        person_id: Id<InternalPerson>,
        arriving_plan: InternalPlan,
    ) {
        self.person_id2internal_person.insert(
            person_id.clone(),
            InternalPerson::new(person_id, arriving_plan),
        );
    }

    pub(crate) fn finish(&mut self) -> Population {
        Population {
            persons: take(&mut self.person_id2internal_person),
        }
    }

    fn handle_event(&mut self, event: &dyn EventTrait) {
        if let Some(e) = event.as_any().downcast_ref::<LinkEnterEvent>() {
            self.forward_vehicle_event(e.vehicle.clone(), Box::new(e.clone()));
        } else if let Some(e) = event.as_any().downcast_ref::<PersonArrivalEvent>() {
            self.forward_person_event(e.person.clone(), Box::new(e.clone()));
        } else if let Some(e) = event.as_any().downcast_ref::<PersonDepartureEvent>() {
            self.forward_person_event(e.person.clone(), Box::new(e.clone()));
        } else if let Some(e) = event.as_any().downcast_ref::<ActivityStartEvent>() {
            self.forward_person_event(e.person.clone(), Box::new(e.clone()));
        } else if let Some(e) = event.as_any().downcast_ref::<ActivityEndEvent>() {
            self.forward_person_event(e.person.clone(), Box::new(e.clone()));
        } else if let Some(e) = event.as_any().downcast_ref::<TeleportationArrivalEvent>() {
            self.forward_person_event(e.person.clone(), Box::new(e.clone()));
        } else if let Some(e) = event.as_any().downcast_ref::<PersonEntersVehicleEvent>() {
            self.forward_vehicle_event(e.vehicle.clone(), Box::new(e.clone()));
        } else if let Some(e) = event.as_any().downcast_ref::<PersonLeavesVehicleEvent>() {
            self.forward_vehicle_event(e.vehicle.clone(), Box::new(e.clone()));
        } else if let Some(e) = event.as_any().downcast_ref::<VehicleEntersTrafficEvent>() {
            self.forward_vehicle_event(e.vehicle.clone(), Box::new(e.clone()));
        } else if let Some(e) = event.as_any().downcast_ref::<VehicleLeavesTrafficEvent>() {
            self.forward_vehicle_event(e.vehicle.clone(), Box::new(e.clone()));
        }
    }

    pub(crate) fn register_event_fn(
        data_collector: Arc<Mutex<MappingDataForwarder>>,
    ) -> Box<EventHandlerRegisterFn> {
        Box::new(move |events: &mut EventsManager| {
            // General event forwarding
            let collector1 = Arc::clone(&data_collector);
            events.on_any(move |e: &dyn EventTrait| {
                let mut mdf = collector1.lock().unwrap();
                mdf.handle_event(e);
            });
        })
    }
}
