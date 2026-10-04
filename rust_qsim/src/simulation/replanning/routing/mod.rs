use crate::simulation::InternalAttributes;
use crate::simulation::id::Id;
use crate::simulation::scenario::facilities::Facility;
use crate::simulation::scenario::population::{InternalPerson, InternalPlanElement};
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::SimTime;
use derive_builder::Builder;
use nohash_hasher::IntMap;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use thiserror::Error;

pub mod a_star;
mod a_star_core;
pub mod alt_landmark_data;
pub mod cost;
mod graph;
pub mod least_cost_path_calculator;
mod network_converter;
pub mod network_routing;
pub mod teleportation;
pub mod travel_time_calculator;
pub mod utils;

#[derive(Debug, Clone, Default)]
pub struct TripRouter {
    modules: IntMap<Id<String>, Arc<dyn RoutingModule>>,
}

impl TripRouter {
    pub fn new(modules: IntMap<Id<String>, Arc<dyn RoutingModule>>) -> Self {
        TripRouter { modules }
    }

    pub fn has_module(&self, mode: &Id<String>) -> bool {
        self.modules.contains_key(mode)
    }

    pub fn calc_route(
        &self,
        mode: &Id<String>,
        request: RoutingRequest,
    ) -> Result<Vec<InternalPlanElement>, RoutingError> {
        let mut elements = self
            .modules
            .get(mode)
            .ok_or_else(|| RoutingError::MissingModule {
                mode: mode.external().to_string(),
            })?
            .calc_route(request)?;

        for element in &mut elements {
            if let InternalPlanElement::Leg(leg) = element {
                leg.routing_mode = Some(mode.clone());
            }
        }

        Ok(elements)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RoutingError {
    #[error("No routing module found for mode {mode}")]
    MissingModule { mode: String },
    #[error("No route found from {from} to {to} with mode {mode}")]
    NoPath {
        mode: String,
        from: String,
        to: String,
    },
    #[error("Routing for mode {mode} produced elements without a determinable end time")]
    MissingEndTime { mode: String },
    #[error("Routing for mode {mode} is not implemented")]
    Unsupported { mode: String },
}

#[derive(Builder, Clone)]
#[builder(pattern = "owned")]
pub struct RoutingRequest<'r> {
    from: &'r Facility,
    to: &'r Facility,
    #[builder(default)]
    departure_time: SimTime,
    #[builder(default)]
    person: Option<&'r InternalPerson>,
    #[builder(default)]
    vehicle: Option<&'r InternalVehicle>,
    #[builder(default)]
    attributes: InternalAttributes,
}

impl<'r> RoutingRequest<'r> {
    pub fn from(&self) -> &'r Facility {
        self.from
    }

    pub fn to(&self) -> &'r Facility {
        self.to
    }

    pub fn departure_time(&self) -> SimTime {
        self.departure_time
    }

    pub fn person(&self) -> Option<&'r InternalPerson> {
        self.person
    }

    pub fn vehicle(&self) -> Option<&'r InternalVehicle> {
        self.vehicle
    }

    pub fn attributes(&self) -> &InternalAttributes {
        &self.attributes
    }
}

/// Calculates complete trip elements for one routing mode.
///
/// Implementors must be thread-safe because routing may be called from multiple threads in
/// parallel. A successful result must form a valid trip: every leg must contain its required route
/// data and times, and any activities must be stage activities that do not create new trips.
/// `TripRouter` assigns the requested routing mode to every returned leg.
pub trait RoutingModule: Send + Sync {
    fn calc_route(&self, request: RoutingRequest)
    -> Result<Vec<InternalPlanElement>, RoutingError>;
    fn mode(&self) -> &Id<String>;
}

#[allow(dead_code)]
struct TransitRoutingModule {}

impl RoutingModule for TransitRoutingModule {
    fn calc_route(
        &self,
        _request: RoutingRequest,
    ) -> Result<Vec<InternalPlanElement>, RoutingError> {
        Err(RoutingError::Unsupported {
            mode: "pt".to_string(),
        })
    }

    fn mode(&self) -> &Id<String> {
        todo!()
    }
}

impl Debug for dyn RoutingModule {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        // write the name of the module
        write!(f, "RoutingModule({})", self.mode())
    }
}
