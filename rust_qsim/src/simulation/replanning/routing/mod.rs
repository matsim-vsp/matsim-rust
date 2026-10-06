use crate::simulation::InternalAttributes;
use crate::simulation::id::Id;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::facilities::ActivityFacility;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::{InternalPerson, InternalPlanElement};
use crate::simulation::scenario::transit::TransitStopFacility;
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
            .get(&mode)
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

/// Facility is a location that has modal access to the network.
///
/// The variants borrow scenario facilities, so that building routing requests does not clone them.
#[derive(Debug, Clone, PartialEq)]
pub enum Facility<'a> {
    LinkWrapperFacility(LinkWrapperFacility),
    ActivityFacility(&'a ActivityFacility),
    TransitFacility(&'a TransitStopFacility),
}

impl Facility<'_> {
    pub fn coord(&self) -> &Coordinate {
        match self {
            Facility::LinkWrapperFacility(facility) => &facility.coord,
            Facility::ActivityFacility(facility) => &facility.coord,
            Facility::TransitFacility(facility) => &facility.coord,
        }
    }

    /// The "address" of the facility. It determines the compute partition of activities taking
    /// place at the facility, but not how the facility is connected to the network for routing.
    /// See [`Facility::modal_link`] for the latter.
    pub fn base_link(&self) -> &Id<Link> {
        match self {
            Facility::LinkWrapperFacility(facility) => &facility.link_id,
            Facility::ActivityFacility(facility) => facility.base_link(),
            Facility::TransitFacility(facility) => {
                facility.link_ref_id.as_ref().unwrap_or_else(|| {
                    panic!("Transit facility with id {} has no link id.", facility.id)
                })
            }
        }
    }

    /// The link through which the facility is connected to the network for `mode`, i.e. the
    /// access and egress link of trips with that mode.
    ///
    /// The [`Facility::base_link`] is always the fallback: it is returned whenever the facility
    /// has no dedicated link for `mode`. This is the case for modes without network links, e.g.
    /// teleported modes, for modes whose nearest link equals the base link (such entries are not
    /// stored), and for transit facilities, which have no modal links at all.
    pub fn modal_link(&self, mode: &Id<String>) -> &Id<Link> {
        let modal_link = match self {
            Facility::LinkWrapperFacility(facility) => facility.mode_to_link.get(mode),
            Facility::ActivityFacility(facility) => facility.mode_to_link.get(mode),
            Facility::TransitFacility(_) => None,
        };
        modal_link.unwrap_or_else(|| self.base_link())
    }

    pub fn new_link_wrapper(coord: Coordinate, link_id: Id<Link>) -> Facility<'static> {
        Facility::LinkWrapperFacility(LinkWrapperFacility {
            coord,
            link_id,
            mode_to_link: IntMap::default(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LinkWrapperFacility {
    pub coord: Coordinate,
    pub link_id: Id<Link>,
    pub mode_to_link: IntMap<Id<String>, Id<Link>>,
}

impl From<&ActivityFacility> for LinkWrapperFacility {
    fn from(value: &ActivityFacility) -> Self {
        LinkWrapperFacility {
            coord: value.coord.clone(),
            link_id: value.base_link().clone(),
            mode_to_link: value.mode_to_link.clone(),
        }
    }
}

impl From<&TransitStopFacility> for LinkWrapperFacility {
    fn from(value: &TransitStopFacility) -> Self {
        LinkWrapperFacility {
            coord: value.coord.clone(),
            link_id: value
                .link_ref_id
                .clone()
                .unwrap_or_else(|| panic!("Transit facility with id {} has no link id.", value.id)),
            mode_to_link: IntMap::default(),
        }
    }
}

#[derive(Builder, Clone)]
#[builder(pattern = "owned")]
pub struct RoutingRequest<'r> {
    from: &'r Facility<'r>,
    to: &'r Facility<'r>,
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
    pub fn from(&self) -> &'r Facility<'r> {
        self.from
    }

    pub fn to(&self) -> &'r Facility<'r> {
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

#[cfg(test)]
mod tests {
    use crate::simulation::InternalAttributes;
    use crate::simulation::id::Id;
    use crate::simulation::replanning::routing::{Facility, LinkWrapperFacility};
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::facilities::{ActivityFacility, ActivityOption};
    use crate::simulation::scenario::transit::TransitStopFacility;
    use macros::deterministic_id_test;
    use nohash_hasher::IntMap;

    #[deterministic_id_test]
    fn activity_facility_modal_link_uses_mode_mapping() {
        let car = Id::create("car");
        let base_link = Id::create("base-link");
        let car_link = Id::create("car-link");
        let mut mode_to_link = IntMap::default();
        mode_to_link.insert(car.clone(), car_link.clone());

        let facility = ActivityFacility {
            id: Id::create("f1"),
            coord: Coordinate::new_2d(1.0, 2.0),
            base_link: Some(base_link.clone()),
            mode_to_link,
            desc: None,
            activities: vec![ActivityOption {
                activity_type: Id::create("work"),
                capacity: None,
                open_times: Vec::new(),
            }],
            attributes: InternalAttributes::default(),
        };
        let facility = Facility::ActivityFacility(&facility);

        assert_eq!(&car_link, facility.modal_link(&car));
        assert_eq!(&base_link, facility.modal_link(&Id::create("bike")));
        assert_eq!(&base_link, facility.base_link());
    }

    #[deterministic_id_test]
    fn link_wrapper_facility_provides_coord_link_and_modal_link() {
        let walk = Id::create("walk");
        let base_link = Id::create("base-link");
        let walk_link = Id::create("walk-link");
        let mut mode_to_link = IntMap::default();
        mode_to_link.insert(walk, walk_link.clone());

        let facility = Facility::LinkWrapperFacility(LinkWrapperFacility {
            coord: Coordinate::new_2d(3.0, 4.0),
            link_id: base_link.clone(),
            mode_to_link,
        });

        assert_eq!(&Coordinate::new_2d(3.0, 4.0), facility.coord());
        assert_eq!(&base_link, facility.base_link());
        assert_eq!(&walk_link, facility.modal_link(&Id::create("walk")));
        assert_eq!(&base_link, facility.modal_link(&Id::create("car")));
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Transit facility with id stop-1 has no link id.")]
    fn transit_facility_link_panics_without_link_ref_id() {
        let stop = TransitStopFacility {
            id: Id::create("stop-1"),
            coord: Coordinate::new_2d(1.0, 2.0),
            link_ref_id: None,
            name: None,
            stop_area_id: None,
            is_blocking: None,
            attributes: InternalAttributes::default(),
        };
        let facility = Facility::TransitFacility(&stop);

        facility.base_link();
    }
}
