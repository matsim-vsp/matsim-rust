use crate::simulation::InternalAttributes;
use crate::simulation::config::ModalLinkSelection;
use crate::simulation::id::Id;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::facilities::ActivityFacility;
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::population::{
    InternalActivity, InternalGenericRoute, InternalLeg, InternalPerson, InternalPlanElement,
    InternalPtRoute, InternalPtRouteDescription, InternalRoute, Population,
};
use crate::simulation::scenario::transit::{TransitSchedule, TransitStopFacility};
use crate::simulation::scenario::vehicles::{Garage, InternalVehicle};
use crate::simulation::time::SimTime;
use arc_swap::ArcSwap;
use derive_builder::Builder;
use nohash_hasher::IntMap;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::collections::BinaryHeap;
use std::collections::{HashMap, HashSet};
use std::fmt::{Debug, Formatter};
use std::mem::size_of;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

pub mod a_star;
pub mod a_star_core;
pub mod alt_landmark_data;
pub mod cost;
pub mod graph;
pub mod least_cost_path_calculator;
pub mod network_converter;
pub mod network_routing;
pub mod teleportation;
pub mod travel_time_calculator;
pub mod utils;

#[derive(Debug, Clone)]
pub struct TripRouter {
    modules: IntMap<Id<String>, Arc<dyn RoutingModule>>,
    route_proposals: Arc<ArcSwap<RouteProposalTable>>,
}

const ROUTE_PROPOSAL_MAX_ENTRIES: usize = 4096;
const ROUTE_PROPOSAL_MAX_BYTES: usize = 4 * 1024 * 1024;

impl Default for TripRouter {
    fn default() -> Self {
        Self {
            modules: IntMap::default(),
            route_proposals: Arc::new(ArcSwap::from_pointee(RouteProposalTable::default())),
        }
    }
}

impl TripRouter {
    pub fn new(modules: IntMap<Id<String>, Arc<dyn RoutingModule>>) -> Self {
        Self {
            modules,
            route_proposals: Arc::new(ArcSwap::from_pointee(RouteProposalTable::default())),
        }
    }

    pub fn has_module(&self, mode: &Id<String>) -> bool {
        self.modules.contains_key(mode)
    }

    pub fn calc_route(
        &self,
        mode: &Id<String>,
        mut request: RoutingRequest,
    ) -> Result<Vec<InternalPlanElement>, RoutingError> {
        if request.candidate_path.is_none() {
            request.candidate_path =
                self.route_proposals
                    .load()
                    .candidate(mode, request.from.link(), request.to.link());
        }
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

    pub(crate) fn prepare_previous_route_proposals(&self, population: &Population) {
        let mut seeds = BTreeMap::<RouteProposalSeed, u64>::new();
        let mut estimated_bytes = 0;
        for person in population.persons.values() {
            for plan in person.plans() {
                for element in &plan.elements {
                    let Some(leg) = element.as_leg() else {
                        continue;
                    };
                    let Some(route) = leg.route.as_ref().and_then(InternalRoute::as_network) else {
                        continue;
                    };
                    let links = route.route();
                    if links.len() < 2 {
                        continue;
                    }
                    let path_link_count = links.len() - 2;
                    let seed_bytes = size_of::<RouteProposalSeed>()
                        + path_link_count
                            * size_of::<Id<crate::simulation::scenario::network::Link>>();
                    if seed_bytes > ROUTE_PROPOSAL_MAX_BYTES {
                        continue;
                    }
                    let mode = leg.routing_mode.as_ref().unwrap_or(&leg.mode).clone();
                    let key = RouteProposalKey {
                        mode,
                        from: links[0].clone(),
                        to: links[links.len() - 1].clone(),
                    };
                    let seed = RouteProposalSeed {
                        key,
                        path: links[1..links.len() - 1].to_vec(),
                    };
                    if let Some(support_count) = seeds.get_mut(&seed) {
                        *support_count = support_count.saturating_add(1);
                        continue;
                    }
                    seeds.insert(seed, 1);
                    estimated_bytes += seed_bytes;
                    while seeds.len() > ROUTE_PROPOSAL_MAX_ENTRIES
                        || estimated_bytes > ROUTE_PROPOSAL_MAX_BYTES
                    {
                        let (largest, _) = seeds.pop_last().expect("non-empty proposal seed set");
                        estimated_bytes -= size_of::<RouteProposalSeed>()
                            + largest.path.capacity()
                                * size_of::<Id<crate::simulation::scenario::network::Link>>();
                    }
                }
            }
        }

        let mut table = RouteProposalTable::default();
        let proposal_seeds = seeds.into_iter().collect::<Vec<_>>();
        for batch in proposal_seeds.chunks(256) {
            for proposal in RouteFrequencyProposalBackend.propose_batch(batch) {
                table
                    .by_request
                    .entry(proposal.seed.key)
                    .or_default()
                    .push(RouteProposal {
                        path: proposal.seed.path,
                        support_count: proposal.support_count,
                    });
            }
        }
        self.route_proposals.store(Arc::new(table));
    }
}

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd)]
struct RouteProposalKey {
    mode: Id<String>,
    from: Id<crate::simulation::scenario::network::Link>,
    to: Id<crate::simulation::scenario::network::Link>,
}

#[derive(Debug, Default)]
struct RouteProposalTable {
    by_request: BTreeMap<RouteProposalKey, Vec<RouteProposal>>,
}

#[derive(Debug)]
struct RouteProposal {
    path: Vec<Id<crate::simulation::scenario::network::Link>>,
    support_count: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct RouteProposalOutput {
    seed: RouteProposalSeed,
    support_count: u64,
}

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd)]
struct RouteProposalSeed {
    key: RouteProposalKey,
    path: Vec<Id<crate::simulation::scenario::network::Link>>,
}

struct RouteFrequencyProposalBackend;

impl RouteFrequencyProposalBackend {
    fn propose_batch(&self, batch: &[(RouteProposalSeed, u64)]) -> Vec<RouteProposalOutput> {
        batch
            .iter()
            .map(|(seed, support_count)| RouteProposalOutput {
                seed: RouteProposalSeed {
                    key: seed.key.clone(),
                    path: seed.path.clone(),
                },
                support_count: *support_count,
            })
            .collect()
    }
}

impl RouteProposalTable {
    fn candidate(
        &self,
        mode: &Id<String>,
        from: &Id<crate::simulation::scenario::network::Link>,
        to: &Id<crate::simulation::scenario::network::Link>,
    ) -> Option<Vec<Id<crate::simulation::scenario::network::Link>>> {
        self.by_request
            .get(&RouteProposalKey {
                mode: mode.clone(),
                from: from.clone(),
                to: to.clone(),
            })
            .and_then(|paths| {
                // Most support wins. `b.path.cmp(&a.path)` makes the smaller stored path the
                // greater one, so equal support falls back to the order of the table.
                paths.iter().max_by(|a, b| {
                    a.support_count
                        .cmp(&b.support_count)
                        .then_with(|| b.path.cmp(&a.path))
                })
            })
            .map(|proposal| proposal.path.clone())
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
    /// An attribute a routing policy depends on could not be parsed. Reported instead of
    /// guessed: reading it as "no" would silently invent trips the agent may not make.
    #[error("Attribute {key} of person {person} is malformed: {reason}")]
    MalformedAttribute {
        person: String,
        key: String,
        reason: String,
    },
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
    pub fn link(&self) -> &Id<Link> {
        self.base_link()
    }

    /// The link through which the facility is connected to the network for `mode`, i.e. the
    /// access and egress link of trips with that mode.
    ///
    /// How modal links are chosen is configured by [`ModalLinkSelection`]. The
    /// [`Facility::base_link`] is always the fallback: it is returned whenever the facility has no
    /// dedicated link for `mode`. This is the case for modes whose selected modal link is the base
    /// link, for modes without network links, e.g. teleported modes, and for transit facilities,
    /// which have no modal links at all.
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

    /// Creates a link wrapper facility for an activity without a facility that is routed with
    /// `mode`. The activity's link becomes the base link. The modal link for `mode` is chosen by
    /// `selection`, exactly as for activity facilities (see [`Network::modal_link`]).
    ///
    /// The modal link is computed on the fly instead of being stored for every link, because a
    /// nearest-link query is cheap compared to routing.
    pub fn new_link_wrapper_for_mode(
        coord: Coordinate,
        link_id: Id<Link>,
        mode: &Id<String>,
        network: &Network,
        selection: ModalLinkSelection,
    ) -> Facility<'static> {
        let mut mode_to_link = IntMap::default();
        let modal_link = network.modal_link(&link_id, &coord, mode, selection);
        if modal_link != link_id {
            mode_to_link.insert(mode.clone(), modal_link);
        }
        Facility::LinkWrapperFacility(LinkWrapperFacility {
            coord,
            link_id,
            mode_to_link,
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
    candidate_path: Option<Vec<Id<crate::simulation::scenario::network::Link>>>,
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

    pub fn candidate_path(&self) -> Option<&[Id<crate::simulation::scenario::network::Link>]> {
        self.candidate_path.as_deref()
    }

    pub fn attributes(&self) -> &InternalAttributes {
        &self.attributes
    }
}

pub trait RoutingModule: Send + Sync {
    fn calc_route(&self, request: RoutingRequest)
    -> Result<Vec<InternalPlanElement>, RoutingError>;
    fn mode(&self) -> &Id<String>;
}

/// MATSim's boolean person attribute that states the agent owns a car and may drive one. The
/// population loaders generate a `{person}_car` vehicle for every person as well, but that is
/// an execution resource, not a declaration of ownership.
const OWNS_CAR: &str = "ownsCar";

pub struct TransitRoutingModule {
    mode: Id<String>,
    schedule: Arc<TransitSchedule>,
    stops_by_cell: HashMap<(i32, i32), Vec<Id<TransitStopFacility>>>,
    routes_by_stop: HashMap<Id<TransitStopFacility>, Vec<RouteStopRef>>,
    cell_size: f64,
    walk_speed: f64,
    walk_distance_factor: f64,
    garage: Arc<Garage>,
    fallback: Option<Arc<dyn RoutingModule>>,
    /// Let a request that carries no person fall back to the car router. That is the legacy
    /// behaviour SILO's zone-to-zone queries rely on; it is an application policy, not a
    /// passenger one, so it stays off unless a config asks for it. See `with_personless_fallback`.
    personless_fallback: bool,
    passenger_modes: std::collections::BTreeMap<String, String>,
    use_passenger_mode_mapping: bool,
    passenger_mode_travel_utilities: std::collections::BTreeMap<String, f64>,
    performing_utility_per_hour: f64,
    default_pt_travel_utility_per_hour: f64,
}

#[derive(Clone)]
struct RouteStopRef {
    line_id: Id<crate::simulation::scenario::transit::TransitLine>,
    route_id: Id<crate::simulation::scenario::transit::TransitRoute>,
    stop_index: usize,
}

/// One ride in a transit vehicle from boarding to alighting.
#[derive(Clone)]
struct Ride {
    line: Id<crate::simulation::scenario::transit::TransitLine>,
    route: Id<crate::simulation::scenario::transit::TransitRoute>,
    board: Id<TransitStopFacility>,
    alight: Id<TransitStopFacility>,
    boarding_time: SimTime,
    alighting_time: SimTime,
    distance: f64,
    passenger_mode: String,
}

/// A door-to-door transit connection.
#[derive(Clone)]
struct TransitPath {
    arrival: SimTime,
    access_distance: f64,
    egress_distance: f64,
    rides: Vec<Ride>,
}

#[derive(Clone)]
struct TransitPathState {
    state_id: u64,
    arrival: SimTime,
    cost: f64,
    access_distance: f64,
    rides: Vec<Ride>,
}

// ponytail: This search cap is fixed at 20 until MATSim's configurable transfer limit is ported.
const RAPTOR_MAX_TRANSFERS: usize = 20;
const RAPTOR_MIN_TRANSFER_TIME: Duration = Duration::from_secs(60);
// With MATSim's pinned defaults, PT/walk time costs 12 utils per hour and a line switch costs
// 1 utility, equivalent to 300 seconds of travel time.
// ponytail: Keep these at pinned Java defaults until configurable RAPTOR scoring is in scope.
const RAPTOR_TRANSFER_COST: Duration = Duration::from_secs(300);

impl RoutingModule for TransitRoutingModule {
    fn calc_route(
        &self,
        request: RoutingRequest,
    ) -> Result<Vec<InternalPlanElement>, RoutingError> {
        let from = request.from.coord();
        let to = request.to.coord();
        let access_stops = self.nearest_stops(from);
        let egress_stops: HashSet<_> = self
            .nearest_stops(to)
            .into_iter()
            .map(|(stop_id, _)| stop_id)
            .collect();
        let best = self.find_best_path(to, request.departure_time, &access_stops, &egress_stops);
        let Some(path) = best else {
            // An origin/destination pair that no transit line connects is not an error: SILO
            // expects a car trip instead of teleporting the agent across the city on foot.
            // But a car trip is only a legitimate answer for an agent that declares owning one
            // and has a car to drive. Everyone else gets the no-path outcome, which the caller
            // turns into a walking trip.
            if let Some(fallback) = &self.fallback
                && self.permits_car_fallback(&request)?
            {
                let person = request.person();
                let vehicle = self.car_vehicle(person);
                // A personless query is never simulated, so it needs no vehicle.
                if vehicle.is_some() || person.is_none() {
                    let car_request = RoutingRequestBuilder::default()
                        .from(request.from)
                        .to(request.to)
                        .departure_time(request.departure_time)
                        .person(person)
                        .vehicle(vehicle)
                        .attributes(request.attributes.clone())
                        .build()
                        .expect("required fallback routing request fields are set");
                    return fallback.calc_route(car_request);
                }
            }
            return Err(RoutingError::NoPath {
                from: request.from.link().external().to_string(),
                to: request.to.link().external().to_string(),
                mode: self.mode.external().to_string(),
            });
        };
        let direct_walk_distance = Coordinate::euclidean_distance(from, to);
        let direct_walk_time = self.walk_time(direct_walk_distance);
        if direct_walk_time.as_secs_f64()
            < self.path_cost_equivalent_seconds(&path, request.departure_time)
        {
            return Ok(vec![InternalPlanElement::Leg(InternalLeg::new(
                InternalRoute::Generic(InternalGenericRoute::new(
                    request.from.link().clone(),
                    request.to.link().clone(),
                    Some(direct_walk_time),
                    Some(direct_walk_distance * self.walk_distance_factor),
                    None,
                )),
                Self::WALK_MODE,
                self.mode.external(),
                direct_walk_time,
                Some(request.departure_time),
            ))]);
        }
        Ok(self.stop_to_stop_trip(&request, &path))
    }

    fn mode(&self) -> &Id<String> {
        &self.mode
    }
}

impl TransitRoutingModule {
    const CELL_SIZE: f64 = 1_000.0;
    const CANDIDATE_COUNT: usize = 12;
    const WALK_MODE: &'static str = "walk";
    const INTERACTION: &'static str = "pt interaction";

    /// Lets requests without a person fall back to the car router, the legacy behaviour SILO
    /// relies on. Kept separate from car ownership because a zone-to-zone query is not a
    /// passenger: nobody is asked whether they own a car.
    pub(crate) fn with_personless_fallback(mut self, enabled: bool) -> Self {
        self.personless_fallback = enabled;
        self
    }

    pub(crate) fn with_passenger_mode_mapping(
        mut self,
        enabled: bool,
        mappings: std::collections::BTreeMap<String, String>,
        scoring: &[crate::simulation::config::ModeParameter],
        agent_scoring: &[crate::simulation::config::AgentParameter],
    ) -> Self {
        self.use_passenger_mode_mapping = enabled;
        self.passenger_modes = mappings;
        self.passenger_mode_travel_utilities = scoring
            .iter()
            .map(|params| (params.mode.clone(), params.marginal_utility_of_traveling))
            .collect();
        self.performing_utility_per_hour = agent_scoring
            .iter()
            .find(|params| params.subpopulation == "person")
            .map_or(6.0, |params| params.performing);
        self.default_pt_travel_utility_per_hour = self
            .passenger_mode_travel_utilities
            .get("pt")
            .copied()
            .unwrap_or(-6.0);
        self
    }

    /// Whether a request that transit cannot connect may be answered with a car trip.
    ///
    /// Only the person's own `ownsCar` attribute decides. A malformed value is an error rather
    /// than a denial, so a bad input cannot pass for a deliberate one.
    fn permits_car_fallback(&self, request: &RoutingRequest) -> Result<bool, RoutingError> {
        let Some(person) = request.person() else {
            return Ok(self.personless_fallback);
        };
        let owns_car = person.attributes().get_bool(OWNS_CAR).map_err(|reason| {
            RoutingError::MalformedAttribute {
                person: person.id().external().to_string(),
                key: OWNS_CAR.to_string(),
                reason,
            }
        })?;
        Ok(owns_car.unwrap_or(false))
    }

    /// The vehicle a fallback car trip drives. Ownership alone is not enough: the leg engine
    /// looks up `{person}_car` when the route carries no vehicle, and a trip without one would
    /// only fail later, during simulation.
    fn car_vehicle(&self, person: Option<&InternalPerson>) -> Option<&InternalVehicle> {
        let person = person?;
        let vehicle_id = Id::try_get_from_ext(format!("{}_car", person.id().external()).as_str())?;
        self.garage.vehicles.get(&vehicle_id)
    }

    /// The trip MATSim's transit router returns: walk to the first stop, one `pt` leg per ride
    /// with a `pt interaction` at every stop, and walk from the last stop. A `pt` leg's travel
    /// time includes the wait for its vehicle.
    fn stop_to_stop_trip(
        &self,
        request: &RoutingRequest,
        path: &TransitPath,
    ) -> Vec<InternalPlanElement> {
        let walk_leg = |from: &Id<Link>, to: &Id<Link>, distance: f64, departure: SimTime| {
            let travel_time = self.walk_time(distance);
            InternalPlanElement::Leg(InternalLeg::new(
                InternalRoute::Generic(InternalGenericRoute::new(
                    from.clone(),
                    to.clone(),
                    Some(travel_time),
                    Some(distance * self.walk_distance_factor),
                    None,
                )),
                Self::WALK_MODE,
                self.mode.external(),
                travel_time,
                Some(departure),
            ))
        };
        let interaction = |stop: &Id<TransitStopFacility>| {
            let facility = self.schedule.get_facility(stop);
            InternalPlanElement::Activity(InternalActivity::new(
                Some(facility.coord.clone()),
                Self::INTERACTION,
                self.stop_link(stop).clone(),
                None,
                None,
                Some(Duration::ZERO),
            ))
        };

        let first_stop = &path.rides[0].board;
        let mut elements = vec![
            walk_leg(
                request.from.link(),
                self.stop_link(first_stop),
                path.access_distance,
                request.departure_time,
            ),
            interaction(first_stop),
        ];
        let mut time = request
            .departure_time
            .saturating_add(self.walk_time(path.access_distance));
        for (ride_index, ride) in path.rides.iter().enumerate() {
            let travel_time = ride.alighting_time.duration_since(time);
            let route = InternalRoute::Pt(InternalPtRoute {
                generic_delegate: InternalGenericRoute::new(
                    self.stop_link(&ride.board).clone(),
                    self.stop_link(&ride.alight).clone(),
                    Some(travel_time),
                    Some(ride.distance),
                    None,
                ),
                description: InternalPtRouteDescription {
                    transit_route_id: ride.route.external().to_string(),
                    boarding_time: Some(ride.boarding_time),
                    transit_line_id: ride.line.external().to_string(),
                    access_facility_id: ride.board.external().to_string(),
                    egress_facility_id: ride.alight.external().to_string(),
                },
            });
            elements.push(InternalPlanElement::Leg(InternalLeg::new(
                route,
                &ride.passenger_mode,
                self.mode.external(),
                travel_time,
                Some(time),
            )));
            elements.push(interaction(&ride.alight));
            time = ride.alighting_time;
            if let Some(next_ride) = path.rides.get(ride_index + 1) {
                let transfer_distance = Coordinate::euclidean_distance(
                    &self.schedule.get_facility(&ride.alight).coord,
                    &self.schedule.get_facility(&next_ride.board).coord,
                );
                elements.push(walk_leg(
                    self.stop_link(&ride.alight),
                    self.stop_link(&next_ride.board),
                    transfer_distance,
                    time,
                ));
                time = time.saturating_add(self.walk_time(transfer_distance));
                elements.push(interaction(&next_ride.board));
            }
        }
        let last_stop = &path.rides.last().unwrap().alight;
        elements.push(walk_leg(
            self.stop_link(last_stop),
            request.to.link(),
            path.egress_distance,
            time,
        ));
        elements
    }

    fn stop_link(&self, stop: &Id<TransitStopFacility>) -> &Id<Link> {
        self.schedule
            .get_facility(stop)
            .link_ref_id
            .as_ref()
            .unwrap_or_else(|| panic!("Transit stop {stop} has no link to walk to."))
    }

    pub(crate) fn new(
        schedule: Arc<TransitSchedule>,
        walk_speed: f64,
        walk_distance_factor: f64,
        garage: Arc<Garage>,
        fallback: Option<Arc<dyn RoutingModule>>,
    ) -> Self {
        let mut stops_by_cell: HashMap<(i32, i32), Vec<Id<TransitStopFacility>>> = HashMap::new();
        for facility in schedule.facilities().values() {
            stops_by_cell
                .entry((
                    (facility.coord.x / Self::CELL_SIZE).floor() as i32,
                    (facility.coord.y / Self::CELL_SIZE).floor() as i32,
                ))
                .or_default()
                .push(facility.id.clone());
        }
        let mut routes_by_stop: HashMap<Id<TransitStopFacility>, Vec<RouteStopRef>> =
            HashMap::new();
        for line in schedule.lines().values() {
            for route in line.routes.values() {
                for (stop_index, stop) in route.stops.iter().enumerate() {
                    routes_by_stop
                        .entry(stop.facility_id.clone())
                        .or_default()
                        .push(RouteStopRef {
                            line_id: line.id.clone(),
                            route_id: route.id.clone(),
                            stop_index,
                        });
                }
            }
        }
        for route_refs in routes_by_stop.values_mut() {
            route_refs.sort_by(|left, right| {
                (
                    left.line_id.external(),
                    left.route_id.external(),
                    left.stop_index,
                )
                    .cmp(&(
                        right.line_id.external(),
                        right.route_id.external(),
                        right.stop_index,
                    ))
            });
        }
        // Routing can run on multiple replanning threads, so register emitted plan element ids
        // before any of them route concurrently.
        Id::<String>::create(Self::WALK_MODE);
        Id::<String>::create(Self::INTERACTION);
        Self {
            mode: Id::create("pt"),
            schedule,
            stops_by_cell,
            routes_by_stop,
            cell_size: Self::CELL_SIZE,
            walk_speed: walk_speed.max(0.1),
            walk_distance_factor,
            garage,
            fallback,
            personless_fallback: false,
            passenger_modes: std::collections::BTreeMap::new(),
            use_passenger_mode_mapping: false,
            passenger_mode_travel_utilities: std::collections::BTreeMap::new(),
            performing_utility_per_hour: 6.0,
            default_pt_travel_utility_per_hour: -6.0,
        }
    }

    pub fn new_for_skim(
        schedule: Arc<TransitSchedule>,
        walk_speed: f64,
        walk_distance_factor: f64,
    ) -> Self {
        Self::new(
            schedule,
            walk_speed,
            walk_distance_factor,
            Arc::new(Garage::default()),
            None,
        )
    }

    pub fn skim_travel_time(
        &self,
        from: Coordinate,
        to: Coordinate,
        departure_time: SimTime,
    ) -> Duration {
        self.skim_times_from_origin(&from, std::slice::from_ref(&to), departure_time)[0]
    }

    pub fn skim_times_from_origin(
        &self,
        origin: &Coordinate,
        destinations: &[Coordinate],
        departure_time: SimTime,
    ) -> Vec<Duration> {
        let mut arrivals: HashMap<Id<TransitStopFacility>, SimTime> = HashMap::new();
        let mut queue = BinaryHeap::new();
        for (stop_id, distance) in self.nearest_stops(origin) {
            let arrival = departure_time.saturating_add(self.walk_time(distance));
            if arrivals.get(&stop_id).is_none_or(|old| arrival < *old) {
                arrivals.insert(stop_id.clone(), arrival);
                queue.push(Reverse((arrival, stop_id)));
            }
        }

        while let Some(Reverse((arrival, stop_id))) = queue.pop() {
            if arrivals.get(&stop_id) != Some(&arrival) {
                continue;
            }
            let Some(route_refs) = self.routes_by_stop.get(&stop_id) else {
                continue;
            };
            for route_ref in route_refs {
                let line = self.schedule.get_line(&route_ref.line_id);
                let route = line.routes.get(&route_ref.route_id).unwrap();
                let board_stop = &route.stops[route_ref.stop_index];
                if !board_stop.allow_boarding {
                    continue;
                }
                let board_offset = board_stop.departure_offset.unwrap_or_default();
                for alight_stop in route.stops.iter().skip(route_ref.stop_index + 1) {
                    if !alight_stop.allow_alighting {
                        continue;
                    }
                    let arrival_offset = alight_stop
                        .arrival_offset
                        .or(alight_stop.departure_offset)
                        .unwrap_or_default();
                    let next_arrival = route
                        .departures
                        .iter()
                        .filter_map(|departure| {
                            let boarding = departure.departure_time.saturating_add(board_offset);
                            let alighting = departure.departure_time.saturating_add(arrival_offset);
                            (boarding >= arrival && alighting >= boarding).then_some(alighting)
                        })
                        .min();
                    let Some(next_arrival) = next_arrival else {
                        continue;
                    };
                    if arrivals
                        .get(&alight_stop.facility_id)
                        .is_none_or(|old| next_arrival < *old)
                    {
                        arrivals.insert(alight_stop.facility_id.clone(), next_arrival);
                        queue.push(Reverse((next_arrival, alight_stop.facility_id.clone())));
                    }
                }
            }
        }

        destinations
            .iter()
            .map(|destination| {
                let direct_walk =
                    self.walk_time(Coordinate::euclidean_distance(origin, destination));
                self.nearest_stops(destination)
                    .into_iter()
                    .filter_map(|(stop_id, distance)| {
                        arrivals.get(&stop_id).map(|arrival| {
                            arrival
                                .saturating_add(self.walk_time(distance))
                                .duration_since(departure_time)
                        })
                    })
                    .fold(direct_walk, Duration::min)
            })
            .collect()
    }

    fn cell(&self, coordinate: &Coordinate) -> (i32, i32) {
        (
            (coordinate.x / self.cell_size).floor() as i32,
            (coordinate.y / self.cell_size).floor() as i32,
        )
    }

    fn nearest_stops(&self, coordinate: &Coordinate) -> Vec<(Id<TransitStopFacility>, f64)> {
        let (center_x, center_y) = self.cell(coordinate);
        let mut candidates = Vec::new();
        for radius in 0..=20 {
            if radius == 0 {
                self.collect_cell(center_x, center_y, coordinate, &mut candidates);
            } else {
                for offset in -radius..=radius {
                    self.collect_cell(
                        center_x + offset,
                        center_y - radius,
                        coordinate,
                        &mut candidates,
                    );
                    self.collect_cell(
                        center_x + offset,
                        center_y + radius,
                        coordinate,
                        &mut candidates,
                    );
                    if offset != -radius && offset != radius {
                        self.collect_cell(
                            center_x - radius,
                            center_y + offset,
                            coordinate,
                            &mut candidates,
                        );
                        self.collect_cell(
                            center_x + radius,
                            center_y + offset,
                            coordinate,
                            &mut candidates,
                        );
                    }
                }
            }
            if radius >= 2
                && candidates.len() >= Self::CANDIDATE_COUNT
                && (radius as f64 - 1.0) * self.cell_size
                    > candidates
                        .iter()
                        .map(|(_, distance)| *distance)
                        .fold(0.0, f64::max)
            {
                break;
            }
        }
        candidates.sort_by(|a, b| {
            a.1.total_cmp(&b.1)
                .then_with(|| a.0.external().cmp(b.0.external()))
        });
        candidates.truncate(Self::CANDIDATE_COUNT);
        candidates
    }

    fn collect_cell(
        &self,
        x: i32,
        y: i32,
        coordinate: &Coordinate,
        candidates: &mut Vec<(Id<TransitStopFacility>, f64)>,
    ) {
        if let Some(stop_ids) = self.stops_by_cell.get(&(x, y)) {
            for stop_id in stop_ids {
                let facility = self.schedule.get_facility(stop_id);
                candidates.push((
                    stop_id.clone(),
                    Coordinate::euclidean_distance(coordinate, &facility.coord),
                ));
            }
        }
    }

    fn walk_time(&self, distance: f64) -> Duration {
        Duration::from_secs_f64(distance * self.walk_distance_factor / self.walk_speed)
    }

    fn path_cost(&self, path: &TransitPath, departure_time: SimTime) -> Duration {
        let transfer_count = path.rides.len().saturating_sub(1) as u32;
        path.arrival
            .duration_since(departure_time)
            .saturating_add(RAPTOR_TRANSFER_COST.saturating_mul(transfer_count))
    }

    fn path_cost_equivalent_seconds(&self, path: &TransitPath, departure_time: SimTime) -> f64 {
        self.cost_equivalent_seconds(
            path.arrival,
            path.access_distance,
            &path.rides,
            departure_time,
        )
    }

    fn cost_equivalent_seconds(
        &self,
        arrival: SimTime,
        access_distance: f64,
        rides: &[Ride],
        departure_time: SimTime,
    ) -> f64 {
        let transfer_count = rides.len().saturating_sub(1) as u32;
        let base = arrival
            .duration_since(departure_time)
            .saturating_add(RAPTOR_TRANSFER_COST.saturating_mul(transfer_count))
            .as_secs_f64();
        if !self.use_passenger_mode_mapping {
            return base;
        }
        base + rides
            .iter()
            .enumerate()
            .map(|(index, ride)| {
                let utility = self
                    .passenger_mode_travel_utilities
                    .get(&ride.passenger_mode)
                    .copied()
                    .unwrap_or(self.default_pt_travel_utility_per_hour);
                let baseline =
                    self.performing_utility_per_hour - self.default_pt_travel_utility_per_hour;
                let mode_cost_factor = (self.performing_utility_per_hour - utility) / baseline;
                let leg_start = if index == 0 {
                    departure_time.saturating_add(self.walk_time(access_distance))
                } else {
                    let previous = &rides[index - 1];
                    let transfer_distance = Coordinate::euclidean_distance(
                        &self.schedule.get_facility(&previous.alight).coord,
                        &self.schedule.get_facility(&ride.board).coord,
                    );
                    previous
                        .alighting_time
                        .saturating_add(self.walk_time(transfer_distance))
                };
                ride.alighting_time.duration_since(leg_start).as_secs_f64()
                    * (mode_cost_factor - 1.0)
            })
            .sum::<f64>()
    }

    fn find_best_path(
        &self,
        destination: &Coordinate,
        departure_time: SimTime,
        access_stops: &[(Id<TransitStopFacility>, f64)],
        egress_stops: &HashSet<Id<TransitStopFacility>>,
    ) -> Option<TransitPath> {
        let mut states: HashMap<(Id<TransitStopFacility>, usize), Vec<TransitPathState>> =
            HashMap::new();
        let mut queue = BinaryHeap::new();
        let mut next_state_id = 0;
        for (stop_id, access_distance) in access_stops {
            let state = TransitPathState {
                state_id: next_state_id,
                arrival: departure_time.saturating_add(self.walk_time(*access_distance)),
                cost: self.walk_time(*access_distance).as_secs_f64(),
                access_distance: *access_distance,
                rides: Vec::new(),
            };
            let key = (stop_id.clone(), 0);
            let labels = states.entry(key).or_default();
            if labels.iter().all(|old| state.arrival < old.arrival) {
                queue.push(Reverse((state.arrival, 0, stop_id.clone(), next_state_id)));
                labels.clear();
                labels.push(state);
                next_state_id += 1;
            }
        }

        let mut best: Option<TransitPath> = None;
        while let Some(Reverse((arrival, rides_used, stop_id, state_id))) = queue.pop() {
            let Some(current) = states
                .get(&(stop_id.clone(), rides_used))
                .and_then(|labels| labels.iter().find(|state| state.state_id == state_id))
                .cloned()
            else {
                continue;
            };
            if current.arrival != arrival {
                continue;
            }

            if egress_stops.contains(&stop_id) && !current.rides.is_empty() {
                let facility = self.schedule.get_facility(&stop_id);
                let egress_distance = Coordinate::euclidean_distance(&facility.coord, destination);
                let candidate = TransitPath {
                    arrival: arrival.saturating_add(self.walk_time(egress_distance)),
                    access_distance: current.access_distance,
                    egress_distance,
                    rides: current.rides.clone(),
                };
                if best.as_ref().is_none_or(|old| {
                    self.path_cost_equivalent_seconds(&candidate, departure_time)
                        < self.path_cost_equivalent_seconds(old, departure_time)
                        || (self.path_cost_equivalent_seconds(&candidate, departure_time)
                            == self.path_cost_equivalent_seconds(old, departure_time)
                            && transit_path_tiebreak(&candidate, old).is_lt())
                }) {
                    best = Some(candidate);
                }
            }

            if rides_used >= RAPTOR_MAX_TRANSFERS + 1 {
                continue;
            }
            let Some(route_refs) = self.routes_by_stop.get(&stop_id) else {
                continue;
            };
            for route_ref in route_refs {
                let line = self.schedule.get_line(&route_ref.line_id);
                let route = line.routes.get(&route_ref.route_id).unwrap();
                let board_stop = &route.stops[route_ref.stop_index];
                if !board_stop.allow_boarding {
                    continue;
                }
                let board_offset = board_stop.departure_offset.unwrap_or_default();
                let earliest_boarding = if current.rides.is_empty() {
                    arrival
                } else {
                    arrival.saturating_add(RAPTOR_MIN_TRANSFER_TIME)
                };

                for (alight_index, alight_stop) in route
                    .stops
                    .iter()
                    .enumerate()
                    .skip(route_ref.stop_index + 1)
                {
                    if !alight_stop.allow_alighting {
                        continue;
                    }
                    let arrival_offset = alight_stop
                        .arrival_offset
                        .or(alight_stop.departure_offset)
                        .unwrap_or_default();
                    for departure in &route.departures {
                        let boarding_time = departure.departure_time.saturating_add(board_offset);
                        if boarding_time < earliest_boarding {
                            continue;
                        }
                        let stop_arrival = departure.departure_time.saturating_add(arrival_offset);
                        if stop_arrival < boarding_time {
                            continue;
                        }
                        let ride_distance = route.stops[route_ref.stop_index..=alight_index]
                            .windows(2)
                            .map(|pair| {
                                let a = self.schedule.get_facility(&pair[0].facility_id);
                                let b = self.schedule.get_facility(&pair[1].facility_id);
                                Coordinate::euclidean_distance(&a.coord, &b.coord)
                            })
                            .sum::<f64>();
                        let mut rides = current.rides.clone();
                        rides.push(Ride {
                            line: line.id.clone(),
                            route: route.id.clone(),
                            board: stop_id.clone(),
                            alight: alight_stop.facility_id.clone(),
                            boarding_time,
                            alighting_time: stop_arrival,
                            distance: ride_distance,
                            passenger_mode: if self.use_passenger_mode_mapping {
                                self.passenger_modes
                                    .get(route.transport_mode.external())
                                    .cloned()
                                    .unwrap_or_else(|| self.mode.external().to_owned())
                            } else {
                                self.mode.external().to_owned()
                            },
                        });
                        let next_stop = alight_stop.facility_id.clone();
                        let next_rides_used = rides_used + 1;
                        let key = (next_stop.clone(), next_rides_used);
                        let candidate_state = TransitPathState {
                            state_id: next_state_id,
                            arrival: stop_arrival,
                            cost: self.cost_equivalent_seconds(
                                stop_arrival,
                                current.access_distance,
                                &rides,
                                departure_time,
                            ),
                            access_distance: current.access_distance,
                            rides,
                        };
                        let candidate_cost = candidate_state.cost;
                        let labels = states.entry(key).or_default();
                        if labels
                            .iter()
                            .any(|old| old.arrival <= stop_arrival && old.cost <= candidate_cost)
                        {
                            continue;
                        }
                        labels.retain(|old| {
                            !(stop_arrival <= old.arrival && candidate_cost <= old.cost)
                        });
                        queue.push(Reverse((
                            stop_arrival,
                            next_rides_used,
                            next_stop,
                            next_state_id,
                        )));
                        labels.push(candidate_state);
                        next_state_id += 1;
                    }
                }
            }
        }
        best
    }
}

fn transit_path_tiebreak(left: &TransitPath, right: &TransitPath) -> std::cmp::Ordering {
    left.rides
        .iter()
        .map(|ride| {
            (
                ride.line.external(),
                ride.route.external(),
                ride.board.external(),
                ride.alight.external(),
            )
        })
        .cmp(right.rides.iter().map(|ride| {
            (
                ride.line.external(),
                ride.route.external(),
                ride.board.external(),
                ride.alight.external(),
            )
        }))
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
    use crate::simulation::config::ModalLinkSelection;
    use crate::simulation::id::Id;
    use crate::simulation::replanning::routing::{Facility, LinkWrapperFacility};
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::facilities::{ActivityFacility, ActivityOption};
    use crate::simulation::scenario::network::{Link, Network, Node};
    use crate::simulation::scenario::transit::TransitStopFacility;
    use macros::deterministic_id_test;
    use nohash_hasher::{IntMap, IntSet};

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
    fn link_wrapper_for_mode_follows_modal_link_selection() {
        // Car links at y=0 and y=20 and a bike link at y=10, all spanning x=0..100.
        let mut network = Network::new();
        for (link_id, y, mode) in [
            ("car-0", 0.0, "car"),
            ("bike-10", 10.0, "bike"),
            ("car-20", 20.0, "car"),
        ] {
            let from = Node::new(
                Id::create(&format!("{link_id}-from")),
                Coordinate::new_2d(0.0, y),
                0,
                1,
            );
            let to = Node::new(
                Id::create(&format!("{link_id}-to")),
                Coordinate::new_2d(100.0, y),
                0,
                1,
            );
            let link = Link::new(
                Id::create(link_id),
                from.id.clone(),
                to.id.clone(),
                100.0,
                1.0,
                1.0,
                1.0,
                IntSet::from_iter([Id::create(mode)]),
                0,
            );
            network.add_node(from);
            network.add_node(to);
            network.add_link(link);
        }
        let car = Id::get_from_ext("car");
        let bike = Id::get_from_ext("bike");
        let walk = Id::create("walk");
        let car_link = Id::<Link>::get_from_ext("car-0");

        let wrapper = |coord: Coordinate, mode: &Id<String>, selection| {
            Facility::new_link_wrapper_for_mode(coord, car_link.clone(), mode, &network, selection)
        };

        // The base link allows car, but another car link is nearer.
        let near_other = Coordinate::new_2d(50.0, 19.0);
        let base_first = wrapper(near_other.clone(), &car, ModalLinkSelection::BaseLinkFirst);
        assert_eq!(&car_link, base_first.modal_link(&car));
        assert_eq!(&car_link, base_first.base_link());
        let nearest = wrapper(near_other, &car, ModalLinkSelection::NearestLink);
        assert_eq!("car-20", nearest.modal_link(&car).external());
        assert_eq!(&car_link, nearest.base_link());

        let near_base = Coordinate::new_2d(50.0, 1.0);
        for selection in [
            ModalLinkSelection::BaseLinkFirst,
            ModalLinkSelection::NearestLink,
        ] {
            // The base link does not allow bike: the nearest bike link is the modal link.
            let bike_wrapper = wrapper(near_base.clone(), &bike, selection);
            assert_eq!("bike-10", bike_wrapper.modal_link(&bike).external());
            // No link allows walk: the base link is the fallback.
            let walk_wrapper = wrapper(near_base.clone(), &walk, selection);
            assert_eq!(&car_link, walk_wrapper.modal_link(&walk));
        }
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

#[cfg(test)]
mod route_proposal_tests {
    use super::{
        Facility, OWNS_CAR, RouteFrequencyProposalBackend, RouteProposal, RouteProposalKey,
        RouteProposalSeed, RouteProposalTable, RoutingError, RoutingModule, RoutingRequest,
        RoutingRequestBuilder, TransitRoutingModule, TripRouter,
    };
    use crate::simulation::InternalAttributes;
    use crate::simulation::id::Id;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::Link;
    use crate::simulation::scenario::population::{
        InternalGenericRoute, InternalLeg, InternalPerson, InternalPlan, InternalPlanElement,
        InternalRoute,
    };
    use crate::simulation::scenario::transit::{TransitLine, TransitRoute, TransitSchedule};
    use crate::simulation::scenario::vehicles::{Garage, InternalVehicle};
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use nohash_hasher::IntMap;
    use serde_json::{Value, json};
    use std::collections::HashSet;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn reference_schedule() -> TransitSchedule {
        TransitSchedule::from_file(
            "./tests/resources/pt_reference/routing_direct_vs_transfer/transit_schedule.xml"
                .as_ref(),
        )
    }

    fn reference_router(schedule: TransitSchedule, walk_speed: f64) -> TransitRoutingModule {
        TransitRoutingModule::new(
            Arc::new(schedule),
            walk_speed,
            1.0,
            Arc::new(Garage::default()),
            None,
        )
    }

    #[deterministic_id_test]
    fn mapped_passenger_modes_change_route_cost_and_are_returned_on_rides() {
        let mut schedule = reference_schedule();
        schedule
            .lines_mut()
            .get_mut(&Id::<TransitLine>::create("Reference Line"))
            .unwrap()
            .routes
            .get_mut(&Id::<TransitRoute>::create("b_to_c"))
            .unwrap()
            .transport_mode = Id::create("bus");
        let scoring = [
            crate::simulation::config::ModeParameter {
                mode: "rail".to_owned(),
                marginal_utility_of_traveling: -1.0,
                ..Default::default()
            },
            crate::simulation::config::ModeParameter {
                mode: "road".to_owned(),
                marginal_utility_of_traveling: -24.0,
                ..Default::default()
            },
        ];
        let router = reference_router(schedule, 0.8333333333333334).with_passenger_mode_mapping(
            true,
            [
                ("train".to_owned(), "rail".to_owned()),
                ("bus".to_owned(), "road".to_owned()),
            ]
            .into(),
            &scoring,
            &[crate::simulation::config::AgentParameter::default()],
        );
        let destination = Coordinate::new_2d(3950.0, 1050.0);
        let access = [(Id::create("ra"), 0.0)];
        let egress = HashSet::from([Id::create("rc")]);
        let path = router
            .find_best_path(&destination, SimTime::from_secs(8 * 3600), &access, &egress)
            .unwrap();

        assert_eq!("direct", path.rides[0].route.external());
        assert_eq!("rail", path.rides[0].passenger_mode);

        let from = Facility::new_link_wrapper(
            Coordinate::new_2d(1050.0, 1050.0),
            Id::<Link>::create("11"),
        );
        let to = Facility::new_link_wrapper(
            Coordinate::new_2d(3950.0, 1050.0),
            Id::<Link>::create("33"),
        );
        let elements = router
            .calc_route(
                RoutingRequestBuilder::default()
                    .from(&from)
                    .to(&to)
                    .departure_time(SimTime::from_secs(8 * 3600))
                    .build()
                    .unwrap(),
            )
            .unwrap();
        assert!(elements.iter().any(|element| {
            element
                .as_leg()
                .is_some_and(|leg| leg.mode.external() == "rail")
        }));
    }

    #[deterministic_id_test]
    fn shared_stop_bus_rail_transfer_is_considered_with_rail_rail() {
        let mut schedule = reference_schedule();
        let line = schedule
            .lines_mut()
            .get_mut(&Id::<TransitLine>::create("Reference Line"))
            .unwrap();
        line.routes
            .get_mut(&Id::<TransitRoute>::create("b_to_c"))
            .unwrap()
            .transport_mode = Id::create("bus");
        let router = reference_router(schedule, 0.8333333333333334);
        let destination = Coordinate::new_2d(3950.0, 1050.0);
        let access = [(Id::create("ra"), 0.0)];
        let egress = HashSet::from([Id::create("rc")]);
        let departure = SimTime::from_secs(8 * 3600);
        let path = router
            .find_best_path(&destination, departure, &access, &egress)
            .unwrap();
        let repeated = router
            .find_best_path(&destination, departure, &access, &egress)
            .unwrap();

        assert_eq!(
            path.rides
                .iter()
                .map(|ride| ride.route.external())
                .collect::<Vec<_>>(),
            ["a_to_b", "b_to_c"]
        );
        assert_eq!(
            path.rides
                .iter()
                .map(|ride| ride.route.external())
                .collect::<Vec<_>>(),
            repeated
                .rides
                .iter()
                .map(|ride| ride.route.external())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            router.path_cost(&path, departure),
            Duration::from_secs(30 * 60)
        );
    }

    #[deterministic_id_test]
    fn final_departure_is_available_but_schedule_does_not_repeat_after_24_hours() {
        let router = reference_router(reference_schedule(), 0.8333333333333334);
        let access = [(Id::create("ra"), 0.0)];
        let egress = HashSet::from([Id::create("rc")]);
        let destination = Coordinate::new_2d(3950.0, 1050.0);

        let final_departure = router
            .find_best_path(&destination, SimTime::from_secs(9 * 3600), &access, &egress)
            .unwrap();
        assert_eq!(final_departure.rides[0].route.external(), "direct");
        assert_eq!(
            final_departure.arrival,
            SimTime::from_secs(9 * 3600 + 50 * 60)
        );

        assert!(
            router
                .find_best_path(
                    &destination,
                    SimTime::from_secs(24 * 3600),
                    &access,
                    &egress,
                )
                .is_none()
        );

        let mut extended_schedule = reference_schedule();
        extended_schedule
            .lines_mut()
            .get_mut(&Id::create("Reference Line"))
            .unwrap()
            .routes
            .get_mut(&Id::create("direct"))
            .unwrap()
            .departures
            .last_mut()
            .unwrap()
            .departure_time = SimTime::from_secs(25 * 3600);
        let extended_router = reference_router(extended_schedule, 0.8333333333333334);
        let after_midnight = extended_router
            .find_best_path(
                &destination,
                SimTime::from_secs(25 * 3600),
                &access,
                &egress,
            )
            .unwrap();
        assert_eq!(after_midnight.rides[0].route.external(), "direct");
        assert_eq!(
            after_midnight.arrival,
            SimTime::from_secs(25 * 3600 + 50 * 60)
        );
    }

    #[deterministic_id_test]
    fn a_direct_walk_can_beat_the_best_transit_itinerary() {
        let router = reference_router(reference_schedule(), 1.0);
        let (from, to) = trip_endpoints();

        let elements = router.calc_route(request(&from, &to, None)).unwrap();
        assert!(matches!(
            elements.as_slice(),
            [InternalPlanElement::Leg(leg)] if leg.mode.external() == "walk"
        ));
    }

    #[deterministic_id_test]
    fn batch_proposes_most_frequent_previous_path_deterministically() {
        let mode = Id::create("car");
        let from = Id::<Link>::create("from");
        let to = Id::<Link>::create("to");
        let middle = Id::<Link>::create("middle");
        let alternative = Id::<Link>::create("alternative");
        let key = RouteProposalKey {
            mode: mode.clone(),
            from: from.clone(),
            to: to.clone(),
        };
        let batch = vec![
            (
                RouteProposalSeed {
                    key: key.clone(),
                    path: vec![middle],
                },
                2,
            ),
            (
                RouteProposalSeed {
                    key: key.clone(),
                    path: vec![alternative.clone()],
                },
                5,
            ),
        ];
        let first = RouteFrequencyProposalBackend.propose_batch(&batch);
        let second = RouteFrequencyProposalBackend.propose_batch(&batch);
        assert_eq!(first, second);

        let mut table = RouteProposalTable::default();
        table
            .by_request
            .entry(key)
            .or_default()
            .extend(first.into_iter().map(|proposal| RouteProposal {
                path: proposal.seed.path,
                support_count: proposal.support_count,
            }));
        assert_eq!(table.candidate(&mode, &from, &to), Some(vec![alternative]));
    }

    #[deterministic_id_test]
    fn equal_proposal_support_falls_back_to_stored_path_order() {
        let mode = Id::create("car");
        let from = Id::<Link>::create("from");
        let to = Id::<Link>::create("to");
        // Lexicographic order of the stored paths decides, independent of the insertion order.
        let first = Id::<Link>::create("a");
        let second = Id::<Link>::create("b");
        let key = RouteProposalKey {
            mode: mode.clone(),
            from: from.clone(),
            to: to.clone(),
        };

        for paths in [
            vec![
                RouteProposal {
                    path: vec![second.clone()],
                    support_count: 3,
                },
                RouteProposal {
                    path: vec![first.clone()],
                    support_count: 3,
                },
            ],
            vec![
                RouteProposal {
                    path: vec![first.clone()],
                    support_count: 3,
                },
                RouteProposal {
                    path: vec![second.clone()],
                    support_count: 3,
                },
            ],
        ] {
            let mut table = RouteProposalTable::default();
            table.by_request.insert(key.clone(), paths);
            assert_eq!(
                table.candidate(&mode, &from, &to),
                Some(vec![first.clone()])
            );
        }
    }

    /// A recording stand-in for the car router, so the test only observes the fallback.
    struct FallbackSpy {
        mode: Id<String>,
        calls: Arc<AtomicUsize>,
        /// What a real car router answers for a pair its network does not connect.
        error: Option<RoutingError>,
    }

    impl RoutingModule for FallbackSpy {
        fn calc_route(
            &self,
            request: RoutingRequest,
        ) -> Result<Vec<InternalPlanElement>, RoutingError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(error) = &self.error {
                return Err(error.clone());
            }
            let route = InternalGenericRoute::new(
                request.from.link().clone(),
                request.to.link().clone(),
                Some(Duration::from_secs(60)),
                Some(1_000.0),
                None,
            );
            Ok(vec![InternalPlanElement::Leg(InternalLeg::new(
                InternalRoute::Generic(route),
                "car",
                "car",
                Duration::from_secs(60),
                None,
            ))])
        }

        fn mode(&self) -> &Id<String> {
            &self.mode
        }
    }

    fn spy(calls: &Arc<AtomicUsize>) -> Arc<dyn RoutingModule> {
        Arc::new(FallbackSpy {
            mode: Id::create("car"),
            calls: calls.clone(),
            error: None,
        })
    }

    /// A person with `ownsCar` set to the given value, or without it, next to the garage the
    /// population loaders build: a `{person}_car` vehicle exists whether or not the person owns
    /// a car, because it is only there to be driven.
    fn person_with_owns_car(id: &str, owns_car: Option<Value>) -> (InternalPerson, Garage) {
        let mut person = InternalPerson::new(
            Id::create(id),
            InternalPlan {
                score: None,
                selected: true,
                elements: Vec::new(),
                attributes: InternalAttributes::default(),
            },
        );
        if let Some(owns_car) = owns_car {
            person.attributes_mut().insert(OWNS_CAR, owns_car);
        }
        let mut garage = Garage::default();
        garage.add_veh(InternalVehicle {
            id: Id::create(&format!("{id}_car")),
            max_v: 10.0,
            pce: 1.0,
            vehicle_type: Id::create("car"),
            attributes: InternalAttributes::default(),
        });
        (person, garage)
    }

    /// A transit router whose schedule connects nothing, so every request has to fall back.
    fn pt_without_transit(garage: Garage, calls: &Arc<AtomicUsize>) -> TransitRoutingModule {
        TransitRoutingModule::new(
            Arc::new(TransitSchedule::default()),
            1.0,
            1.0,
            Arc::new(garage),
            Some(spy(calls)),
        )
    }

    fn trip_endpoints() -> (Facility<'static>, Facility<'static>) {
        (
            Facility::new_link_wrapper(Coordinate::new_2d(0.0, 0.0), Id::<Link>::create("1")),
            Facility::new_link_wrapper(Coordinate::new_2d(10.0, 10.0), Id::<Link>::create("5")),
        )
    }

    fn request<'r>(
        from: &'r Facility<'r>,
        to: &'r Facility<'r>,
        person: Option<&'r InternalPerson>,
    ) -> RoutingRequest<'r> {
        RoutingRequestBuilder::default()
            .from(from)
            .to(to)
            .departure_time(SimTime::from_duration(Duration::ZERO))
            .person(person)
            .build()
            .expect("all required routing request fields are set")
    }

    /// The no-path outcome is what an agent without a permitted fallback receives.
    fn assert_no_path(result: Result<Vec<InternalPlanElement>, RoutingError>) {
        assert!(
            matches!(result, Err(RoutingError::NoPath { .. })),
            "{result:?}"
        );
    }

    /// A car trip may replace a missing transit connection, but only for a person who declares
    /// owning a car.
    #[deterministic_id_test]
    fn a_car_owner_falls_back_to_the_car_router() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (person, garage) = person_with_owns_car("1", Some(json!(true)));
        let module = pt_without_transit(garage, &calls);
        let (from, to) = trip_endpoints();

        let elements = module
            .calc_route(request(&from, &to, Some(&person)))
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(elements.iter().any(|element| element.as_leg().is_some()));
    }

    /// The generated `{person}_car` vehicle is an execution resource, not a declaration, so
    /// neither a missing nor a false `ownsCar` permits the fallback.
    #[deterministic_id_test]
    fn ownership_that_is_missing_or_false_denies_the_car_fallback() {
        for owns_car in [None, Some(json!(false))] {
            let calls = Arc::new(AtomicUsize::new(0));
            let (person, garage) = person_with_owns_car("1", owns_car);
            let module = pt_without_transit(garage, &calls);
            let (from, to) = trip_endpoints();

            assert_no_path(module.calc_route(request(&from, &to, Some(&person))));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }

    /// A malformed value is a bad input, not a denial, so it is reported instead of read as
    /// "does not own a car".
    #[deterministic_id_test]
    fn malformed_ownership_is_reported() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (person, garage) = person_with_owns_car("1", Some(json!("yes")));
        let module = pt_without_transit(garage, &calls);
        let (from, to) = trip_endpoints();

        assert!(matches!(
            module.calc_route(request(&from, &to, Some(&person))),
            Err(RoutingError::MalformedAttribute { person, key, .. })
                if person == "1" && key == OWNS_CAR
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// Ownership still needs a car to drive. Without one the leg engine would look for
    /// `{person}_car` and find nothing, so the agent gets the no-path outcome now.
    #[deterministic_id_test]
    fn ownership_without_a_usable_car_reports_no_path() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (person, _garage) = person_with_owns_car("1", Some(json!(true)));
        let module = pt_without_transit(Garage::default(), &calls);
        let (from, to) = trip_endpoints();

        assert_no_path(module.calc_route(request(&from, &to, Some(&person))));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// A permitted fallback that cannot route is still a failure: the caller must not receive
    /// an invented trip.
    #[deterministic_id_test]
    fn a_failing_car_route_is_propagated() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (person, garage) = person_with_owns_car("1", Some(json!(true)));
        let module = TransitRoutingModule::new(
            Arc::new(TransitSchedule::default()),
            1.0,
            1.0,
            Arc::new(garage),
            Some(Arc::new(FallbackSpy {
                mode: Id::create("car"),
                calls: calls.clone(),
                error: Some(RoutingError::NoPath {
                    mode: "car".to_string(),
                    from: "1".to_string(),
                    to: "5".to_string(),
                }),
            })),
        );
        let (from, to) = trip_endpoints();

        assert!(matches!(
            module.calc_route(request(&from, &to, Some(&person))),
            Err(RoutingError::NoPath { mode, .. }) if mode == "car"
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A zone-to-zone query carries no person, so nobody claims to own a car and it gets the
    /// no-path outcome instead of a silent car trip.
    #[deterministic_id_test]
    fn pt_without_a_person_does_not_use_the_car_router() {
        let calls = Arc::new(AtomicUsize::new(0));
        let module = pt_without_transit(Garage::default(), &calls);
        let (from, to) = trip_endpoints();

        assert_no_path(module.calc_route(request(&from, &to, None)));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// The legacy behaviour SILO's zone-to-zone queries rely on stays available, but only when
    /// a config asks for it. Bangkok's earlier Java runs got the same behaviour from
    /// BangkokPtFallbackModule, which was installed as a controler-wide override.
    #[deterministic_id_test]
    fn pt_without_a_person_uses_the_car_router_only_when_configured() {
        let calls = Arc::new(AtomicUsize::new(0));
        let module = pt_without_transit(Garage::default(), &calls).with_personless_fallback(true);
        let (from, to) = trip_endpoints();

        let elements = module.calc_route(request(&from, &to, None)).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(elements.iter().any(|element| element.as_leg().is_some()));
    }

    /// The gate only applies where transit finds no connection: a passenger who owns a car
    /// keeps the train where one exists.
    #[deterministic_id_test]
    fn a_transit_connection_keeps_a_car_owner_on_transit() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (person, garage) = person_with_owns_car("1", Some(json!(true)));
        let module = TransitRoutingModule::new(
            Arc::new(TransitSchedule::from_file(Path::new(
                "./assets/pt_tutorial/transitschedule.xml",
            ))),
            1.0,
            1.0,
            Arc::new(garage),
            Some(spy(&calls)),
        );
        // Stops 1 and 3 of the tutorial's Blue Line, requested at its 06:00 departure.
        let from = Facility::new_link_wrapper(
            Coordinate::new_2d(1050.0, 1050.0),
            Id::<Link>::create("11"),
        );
        let to = Facility::new_link_wrapper(
            Coordinate::new_2d(3950.0, 1050.0),
            Id::<Link>::create("33"),
        );
        let request = RoutingRequestBuilder::default()
            .from(&from)
            .to(&to)
            .departure_time(SimTime::from_secs(6 * 3600))
            .person(Some(&person))
            .build()
            .unwrap();

        let elements = module.calc_route(request).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(elements.iter().any(|element| {
            element
                .as_leg()
                .is_some_and(|leg| matches!(leg.route, Some(InternalRoute::Pt(_))))
        }));
    }

    /// The route service SILO queries asks `TripRouter` for one passenger's trip, so the gate
    /// has to hold there too, and a request without a person has to stay unanswered.
    #[deterministic_id_test]
    fn trip_router_gates_the_car_fallback_by_ownership() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (person, garage) = person_with_owns_car("1", Some(json!(true)));
        let pt: Arc<dyn RoutingModule> = Arc::new(pt_without_transit(garage, &calls));
        let router = TripRouter::new(IntMap::from_iter([(Id::create("pt"), pt)]));
        let (from, to) = trip_endpoints();

        let elements = router
            .calc_route(&Id::create("pt"), request(&from, &to, Some(&person)))
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(elements.iter().any(|element| element.as_leg().is_some()));
        assert_no_path(router.calc_route(&Id::create("pt"), request(&from, &to, None)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// Without a fallback router there is nothing to answer with, so the caller gets the
    /// no-path error rather than a silently wrong travel time.
    #[deterministic_id_test]
    fn pt_without_transit_or_fallback_reports_no_path() {
        let module = TransitRoutingModule::new(
            Arc::new(TransitSchedule::default()),
            1.0,
            1.0,
            Arc::new(Garage::default()),
            None,
        );
        let (from, to) = trip_endpoints();

        assert_no_path(module.calc_route(request(&from, &to, None)));
    }
}
