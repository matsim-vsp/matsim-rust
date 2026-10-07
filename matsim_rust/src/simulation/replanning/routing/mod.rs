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
    /// Emit a walk leg to the first stop, one `pt` leg per ride between stops and a walk leg
    /// from the last stop, as MATSim's transit router does. Simulated transit vehicles need
    /// this; teleported PT gets a single door-to-door leg.
    stop_to_stop_legs: bool,
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
    arrival: SimTime,
    access_distance: f64,
    rides: Vec<Ride>,
}

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
        let mut best: Option<TransitPath> = None;

        for (access_id, access_distance) in &access_stops {
            let Some(routes) = self.routes_by_stop.get(access_id) else {
                continue;
            };
            let access_walk_time = self.walk_time(*access_distance);
            let earliest_boarding = request.departure_time.saturating_add(access_walk_time);

            for route_ref in routes {
                let line = self.schedule.get_line(&route_ref.line_id);
                let route = line.routes.get(&route_ref.route_id).unwrap();
                let board_stop = &route.stops[route_ref.stop_index];
                if !board_stop.allow_boarding {
                    continue;
                }
                let board_offset = board_stop.departure_offset.unwrap_or_default();

                for (alight_index, alight_stop) in route
                    .stops
                    .iter()
                    .enumerate()
                    .skip(route_ref.stop_index + 1)
                {
                    if !alight_stop.allow_alighting
                        || !egress_stops.contains(&alight_stop.facility_id)
                    {
                        continue;
                    }
                    let egress_facility = self.schedule.get_facility(&alight_stop.facility_id);
                    let egress_distance =
                        Coordinate::euclidean_distance(&egress_facility.coord, to);
                    let egress_walk_time = self.walk_time(egress_distance);
                    let arrival_offset = alight_stop
                        .arrival_offset
                        .or(alight_stop.departure_offset)
                        .unwrap_or_default();
                    let ride_distance = route.stops[route_ref.stop_index..=alight_index]
                        .windows(2)
                        .map(|pair| {
                            let a = self.schedule.get_facility(&pair[0].facility_id);
                            let b = self.schedule.get_facility(&pair[1].facility_id);
                            Coordinate::euclidean_distance(&a.coord, &b.coord)
                        })
                        .sum::<f64>();

                    for departure in &route.departures {
                        let boarding_time = departure.departure_time.saturating_add(board_offset);
                        if boarding_time < earliest_boarding {
                            continue;
                        }
                        let arrival_time = departure.departure_time.saturating_add(arrival_offset);
                        if arrival_time < boarding_time {
                            continue;
                        }
                        let final_arrival = arrival_time.saturating_add(egress_walk_time);
                        if best
                            .as_ref()
                            .is_some_and(|best| final_arrival >= best.arrival)
                        {
                            continue;
                        }
                        best = Some(TransitPath {
                            arrival: final_arrival,
                            access_distance: *access_distance,
                            egress_distance,
                            rides: vec![Ride {
                                line: line.id.clone(),
                                route: route.id.clone(),
                                board: access_id.clone(),
                                alight: egress_facility.id.clone(),
                                boarding_time,
                                alighting_time: arrival_time,
                                distance: ride_distance,
                            }],
                        });
                    }
                }
            }
        }

        let best = best.or_else(|| {
            self.find_transfer_path(to, request.departure_time, &access_stops, &egress_stops)
        });
        let Some(path) = best else {
            // An origin/destination pair that no transit line connects is not an error: SILO
            // expects a car trip instead of teleporting the agent across the city on foot.
            // Callers that do not carry a person (skims, travel-time matrices) still get an
            // answer, because the car router works without one.
            if let Some(fallback) = &self.fallback {
                let vehicle = request.person.and_then(|person| {
                    let vehicle_id =
                        Id::try_get_from_ext(format!("{}_car", person.id().external()).as_str());
                    vehicle_id.and_then(|vehicle_id| self.garage.vehicles.get(&vehicle_id))
                });
                let car_request = RoutingRequestBuilder::default()
                    .from(request.from)
                    .to(request.to)
                    .departure_time(request.departure_time)
                    .person(request.person)
                    .vehicle(vehicle)
                    .attributes(request.attributes.clone())
                    .build()
                    .expect("required fallback routing request fields are set");
                return fallback.calc_route(car_request);
            }
            return Err(RoutingError::NoPath {
                from: request.from.link().external().to_string(),
                to: request.to.link().external().to_string(),
                mode: self.mode.external().to_string(),
            });
        };
        if self.stop_to_stop_legs {
            return Ok(self.stop_to_stop_trip(&request, &path));
        }

        let first = path.rides.first().unwrap();
        let last = path.rides.last().unwrap();
        let distance = (path.access_distance + path.egress_distance) * self.walk_distance_factor
            + path.rides.iter().map(|ride| ride.distance).sum::<f64>();
        let travel_time = path.arrival.duration_since(request.departure_time);
        let generic_route = InternalGenericRoute::new(
            request.from.link().clone(),
            request.to.link().clone(),
            Some(travel_time),
            Some(distance),
            None,
        );
        let route = InternalRoute::Pt(InternalPtRoute {
            generic_delegate: generic_route,
            description: InternalPtRouteDescription {
                transit_route_id: first.route.external().to_string(),
                boarding_time: Some(first.boarding_time),
                transit_line_id: first.line.external().to_string(),
                access_facility_id: first.board.external().to_string(),
                egress_facility_id: last.alight.external().to_string(),
            },
        });
        Ok(vec![InternalPlanElement::Leg(InternalLeg::new(
            route,
            self.mode.external(),
            self.mode.external(),
            travel_time,
            Some(request.departure_time),
        ))])
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

    /// Routes PT trips as separate stop-to-stop legs. See `stop_to_stop_legs`.
    pub(crate) fn with_stop_to_stop_legs(mut self, enabled: bool) -> Self {
        self.stop_to_stop_legs = enabled;
        if enabled {
            // Replanning routes on several threads; create the ids the trips use up front.
            Id::<String>::create(Self::WALK_MODE);
            Id::<String>::create(Self::INTERACTION);
        }
        self
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
        for ride in &path.rides {
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
                self.mode.external(),
                self.mode.external(),
                travel_time,
                Some(time),
            )));
            elements.push(interaction(&ride.alight));
            time = ride.alighting_time;
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
            stop_to_stop_legs: false,
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
        candidates.sort_by(|a, b| a.1.total_cmp(&b.1));
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

    fn find_transfer_path(
        &self,
        destination: &Coordinate,
        departure_time: SimTime,
        access_stops: &[(Id<TransitStopFacility>, f64)],
        egress_stops: &HashSet<Id<TransitStopFacility>>,
    ) -> Option<TransitPath> {
        let mut states: HashMap<Id<TransitStopFacility>, TransitPathState> = HashMap::new();
        let mut queue = BinaryHeap::new();
        for (stop_id, access_distance) in access_stops {
            let state = TransitPathState {
                arrival: departure_time.saturating_add(self.walk_time(*access_distance)),
                access_distance: *access_distance,
                rides: Vec::new(),
            };
            if states
                .get(stop_id)
                .is_none_or(|old| state.arrival < old.arrival)
            {
                queue.push(Reverse((state.arrival, stop_id.clone())));
                states.insert(stop_id.clone(), state);
            }
        }

        let mut best: Option<TransitPath> = None;
        while let Some(Reverse((arrival, stop_id))) = queue.pop() {
            if best.as_ref().is_some_and(|best| arrival >= best.arrival) {
                break;
            }
            let Some(current) = states.get(&stop_id).cloned() else {
                continue;
            };
            if current.arrival != arrival {
                continue;
            }

            if egress_stops.contains(&stop_id) && !current.rides.is_empty() {
                let facility = self.schedule.get_facility(&stop_id);
                let egress_distance = Coordinate::euclidean_distance(&facility.coord, destination);
                best = Some(TransitPath {
                    arrival: arrival.saturating_add(self.walk_time(egress_distance)),
                    access_distance: current.access_distance,
                    egress_distance,
                    rides: current.rides.clone(),
                });
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
                    let mut next: Option<(SimTime, SimTime)> = None;
                    for departure in &route.departures {
                        let boarding_time = departure.departure_time.saturating_add(board_offset);
                        if boarding_time < arrival {
                            continue;
                        }
                        let stop_arrival = departure.departure_time.saturating_add(arrival_offset);
                        if stop_arrival < boarding_time {
                            continue;
                        }
                        if next.is_none_or(|(old_arrival, _)| stop_arrival < old_arrival) {
                            next = Some((stop_arrival, boarding_time));
                        }
                    }
                    let Some((stop_arrival, boarding_time)) = next else {
                        continue;
                    };
                    if states
                        .get(&alight_stop.facility_id)
                        .is_some_and(|old| stop_arrival >= old.arrival)
                    {
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
                    });
                    let next_stop = alight_stop.facility_id.clone();
                    queue.push(Reverse((stop_arrival, next_stop.clone())));
                    states.insert(
                        next_stop,
                        TransitPathState {
                            arrival: stop_arrival,
                            access_distance: current.access_distance,
                            rides,
                        },
                    );
                }
            }
        }
        best
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
        RouteFrequencyProposalBackend, RouteProposal, RouteProposalKey, RouteProposalSeed,
        RouteProposalTable, RoutingError, RoutingModule, RoutingRequest, RoutingRequestBuilder,
        TransitRoutingModule,
    };
    use crate::simulation::id::Id;
    use crate::simulation::scenario::network::Link;
    use crate::simulation::scenario::population::{
        InternalGenericRoute, InternalLeg, InternalPlanElement, InternalRoute,
    };
    use crate::simulation::scenario::transit::TransitSchedule;
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

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
    }

    impl RoutingModule for FallbackSpy {
        fn calc_route(
            &self,
            request: RoutingRequest,
        ) -> Result<Vec<InternalPlanElement>, RoutingError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
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

    /// SILO asks for travel times between zones without a person, so the pt module has to
    /// fall back to a car trip there too. Bangkok's earlier Java runs got the same behaviour
    /// from BangkokPtFallbackModule, which was installed as a controler-wide override.
    #[deterministic_id_test]
    fn pt_without_a_person_falls_back_to_the_car_router() {
        use crate::simulation::scenario::Coordinate;
        use super::Facility;
        use crate::simulation::scenario::network::Link;
        use crate::simulation::scenario::vehicles::Garage;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // An empty schedule leaves the pt module with no transit path to find.
        let calls = Arc::new(AtomicUsize::new(0));
        let module = TransitRoutingModule::new(
            Arc::new(TransitSchedule::default()),
            1.0,
            1.0,
            Arc::new(Garage::default()),
            Some(Arc::new(FallbackSpy {
                mode: Id::create("car"),
                calls: calls.clone(),
            })),
        );
        let from =
            Facility::new_link_wrapper(Coordinate::new_2d(0.0, 0.0), Id::<Link>::create("1"));
        let to =
            Facility::new_link_wrapper(Coordinate::new_2d(10.0, 10.0), Id::<Link>::create("5"));
        let request = RoutingRequestBuilder::default()
            .from(&from)
            .to(&to)
            .departure_time(SimTime::from_duration(Duration::ZERO))
            .build()
            .unwrap();

        // No person on the request: the answer has to come from the car fallback.
        let elements = module.calc_route(request).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(elements.iter().any(|element| element.as_leg().is_some()));
    }

    /// Without a fallback router there is nothing to answer with, so the caller gets the
    /// no-path error rather than a silently wrong travel time.
    #[deterministic_id_test]
    fn pt_without_transit_or_fallback_reports_no_path() {
        use crate::simulation::scenario::Coordinate;
        use super::Facility;
        use crate::simulation::scenario::network::Link;
        use crate::simulation::scenario::vehicles::Garage;

        let module = TransitRoutingModule::new(
            Arc::new(TransitSchedule::default()),
            1.0,
            1.0,
            Arc::new(Garage::default()),
            None,
        );
        let from =
            Facility::new_link_wrapper(Coordinate::new_2d(0.0, 0.0), Id::<Link>::create("1"));
        let to =
            Facility::new_link_wrapper(Coordinate::new_2d(10.0, 10.0), Id::<Link>::create("5"));
        let request = RoutingRequestBuilder::default()
            .from(&from)
            .to(&to)
            .departure_time(SimTime::from_duration(Duration::ZERO))
            .build()
            .unwrap();

        assert!(matches!(
            module.calc_route(request),
            Err(RoutingError::NoPath { .. })
        ));
    }
}
