use crate::simulation::id::Id;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::vehicles::InternalVehicle;

mod mapping_data_collector;
mod mapping_data_forwarder;
mod mapping_message_broker;
pub(crate) mod mapping_scoring_engine;

pub fn person_hash(num_collectors: u32) -> Box<dyn Fn(Id<InternalPerson>) -> u32 + Send> {
    Box::new(move |id: Id<InternalPerson>| (id.internal() % num_collectors as u64) as u32)
}

pub fn vehicle_hash(num_collectors: u32) -> Box<dyn Fn(Id<InternalVehicle>) -> u32 + Send> {
    Box::new(move |id: Id<InternalVehicle>| (id.internal() % num_collectors as u64) as u32)
}
