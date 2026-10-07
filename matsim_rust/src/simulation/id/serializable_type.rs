use crate::simulation::scenario::facilities::ActivityFacility;
use crate::simulation::scenario::network::{Link, Node};
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::transit::{
    TransitDeparture, TransitLine, TransitRoute, TransitStopFacility,
};
use crate::simulation::scenario::vehicles::{InternalVehicle, InternalVehicleType};
pub trait StableTypeId {
    fn stable_type_id() -> u64;
}

impl StableTypeId for String {
    fn stable_type_id() -> u64 {
        STRING_TYPE_ID
    }
}

impl StableTypeId for Link {
    fn stable_type_id() -> u64 {
        LINK_TYPE_ID
    }
}

impl StableTypeId for Node {
    fn stable_type_id() -> u64 {
        NODE_TYPE_ID
    }
}

impl StableTypeId for InternalVehicleType {
    fn stable_type_id() -> u64 {
        VEHICLE_TYPE_ID
    }
}

impl StableTypeId for InternalVehicle {
    fn stable_type_id() -> u64 {
        VEHICLE_TYPE_ID
    }
}

impl StableTypeId for () {
    fn stable_type_id() -> u64 {
        0
    }
}

impl StableTypeId for i32 {
    fn stable_type_id() -> u64 {
        I32_TYPE_ID
    }
}

impl StableTypeId for i64 {
    fn stable_type_id() -> u64 {
        I64_TYPE_ID
    }
}

impl StableTypeId for u32 {
    fn stable_type_id() -> u64 {
        U32_TYPE_ID
    }
}

impl StableTypeId for f32 {
    fn stable_type_id() -> u64 {
        F32_TYPE_ID
    }
}

impl StableTypeId for InternalPerson {
    fn stable_type_id() -> u64 {
        PERSON_TYPE_ID
    }
}

impl StableTypeId for TransitLine {
    fn stable_type_id() -> u64 {
        TRANSIT_LINE_TYPE_ID
    }
}

impl StableTypeId for TransitRoute {
    fn stable_type_id() -> u64 {
        TRANSIT_ROUTE_TYPE_ID
    }
}

impl StableTypeId for TransitStopFacility {
    fn stable_type_id() -> u64 {
        TRANSIT_STOP_FACILITY_TYPE_ID
    }
}

impl StableTypeId for TransitDeparture {
    fn stable_type_id() -> u64 {
        TRANSIT_DEPARTURE_TYPE_ID
    }
}

impl StableTypeId for ActivityFacility {
    fn stable_type_id() -> u64 {
        FACILITY_TYPE_ID
    }
}

pub const STRING_TYPE_ID: u64 = 1;
pub const PERSON_TYPE_ID: u64 = 2;
pub const LINK_TYPE_ID: u64 = 3;
pub const NODE_TYPE_ID: u64 = 4;
pub const VEHICLE_TYPE_TYPE_ID: u64 = 5;
pub const VEHICLE_TYPE_ID: u64 = 6;
pub const I32_TYPE_ID: u64 = 7;
pub const I64_TYPE_ID: u64 = 8;
pub const U32_TYPE_ID: u64 = 9;
pub const F32_TYPE_ID: u64 = 10;
pub const TRANSIT_LINE_TYPE_ID: u64 = 11;
pub const TRANSIT_ROUTE_TYPE_ID: u64 = 12;
pub const TRANSIT_STOP_FACILITY_TYPE_ID: u64 = 13;
pub const TRANSIT_DEPARTURE_TYPE_ID: u64 = 14;
pub const FACILITY_TYPE_ID: u64 = 15;
