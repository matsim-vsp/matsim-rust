use crate::simulation::InternalAttributes;
use crate::simulation::id::Id;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::facilities::Facility;
use crate::simulation::scenario::population::{
    InternalGenericRoute, InternalLeg, InternalPerson, InternalPlanElement, InternalPtRoute,
    InternalPtRouteDescription, InternalRoute, Population,
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
    candidate_path: Option<Vec<Id<crate::simulation::scenario::network::Link>>>,
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

    pub fn candidate_path(&self) -> Option<&[Id<crate::simulation::scenario::network::Link>]> {
        self.candidate_path.as_deref()
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
}

#[derive(Clone)]
struct RouteStopRef {
    line_id: Id<crate::simulation::scenario::transit::TransitLine>,
    route_id: Id<crate::simulation::scenario::transit::TransitRoute>,
    stop_index: usize,
}

#[derive(Clone)]
struct TransitPathState {
    arrival: SimTime,
    distance: f64,
    first_boarding_time: Option<SimTime>,
    first_route_id: Option<String>,
    first_line_id: Option<String>,
    first_access_id: Option<String>,
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
        let mut best: Option<(SimTime, SimTime, f64, String, String, String, String)> = None;

        for (access_id, access_distance) in &access_stops {
            let Some(routes) = self.routes_by_stop.get(access_id) else {
                continue;
            };
            let access_facility = self.schedule.get_facility(access_id);
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
                            .is_some_and(|(best_arrival, ..)| final_arrival >= *best_arrival)
                        {
                            continue;
                        }
                        let distance = access_distance * self.walk_distance_factor
                            + ride_distance
                            + egress_distance * self.walk_distance_factor;
                        best = Some((
                            final_arrival,
                            boarding_time,
                            distance,
                            route.id.external().to_string(),
                            line.id.external().to_string(),
                            access_facility.id.external().to_string(),
                            egress_facility.id.external().to_string(),
                        ));
                    }
                }
            }
        }

        let best = best.or_else(|| {
            self.find_transfer_path(to, request.departure_time, &access_stops, &egress_stops)
        });
        let Some((arrival_time, boarding_time, distance, route_id, line_id, access_id, egress_id)) =
            best
        else {
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
        let travel_time = arrival_time.duration_since(request.departure_time);
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
                transit_route_id: route_id,
                boarding_time: Some(boarding_time),
                transit_line_id: line_id,
                access_facility_id: access_id,
                egress_facility_id: egress_id,
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
    ) -> Option<(SimTime, SimTime, f64, String, String, String, String)> {
        let mut states: HashMap<Id<TransitStopFacility>, TransitPathState> = HashMap::new();
        let mut queue = BinaryHeap::new();
        for (stop_id, access_distance) in access_stops {
            let state = TransitPathState {
                arrival: departure_time.saturating_add(self.walk_time(*access_distance)),
                distance: *access_distance * self.walk_distance_factor,
                first_boarding_time: None,
                first_route_id: None,
                first_line_id: None,
                first_access_id: None,
            };
            if states
                .get(stop_id)
                .is_none_or(|old| state.arrival < old.arrival)
            {
                states.insert(stop_id.clone(), state.clone());
                queue.push(Reverse((state.arrival, stop_id.clone())));
            }
        }

        let mut best: Option<(SimTime, SimTime, f64, String, String, String, String)> = None;
        while let Some(Reverse((arrival, stop_id))) = queue.pop() {
            if best
                .as_ref()
                .is_some_and(|(best_arrival, ..)| arrival >= *best_arrival)
            {
                break;
            }
            let Some(current) = states.get(&stop_id).cloned() else {
                continue;
            };
            if current.arrival != arrival {
                continue;
            }

            if egress_stops.contains(&stop_id)
                && let (Some(boarding_time), Some(route_id), Some(line_id), Some(access_id)) = (
                    current.first_boarding_time,
                    current.first_route_id.clone(),
                    current.first_line_id.clone(),
                    current.first_access_id.clone(),
                )
            {
                let facility = self.schedule.get_facility(&stop_id);
                let egress_distance = Coordinate::euclidean_distance(&facility.coord, destination);
                let final_arrival = arrival.saturating_add(self.walk_time(egress_distance));
                best = Some((
                    final_arrival,
                    boarding_time,
                    current.distance + egress_distance * self.walk_distance_factor,
                    route_id,
                    line_id,
                    access_id,
                    stop_id.external().to_string(),
                ));
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
                    let ride_distance = route.stops[route_ref.stop_index..=alight_index]
                        .windows(2)
                        .map(|pair| {
                            let a = self.schedule.get_facility(&pair[0].facility_id);
                            let b = self.schedule.get_facility(&pair[1].facility_id);
                            Coordinate::euclidean_distance(&a.coord, &b.coord)
                        })
                        .sum::<f64>();
                    let state = TransitPathState {
                        arrival: stop_arrival,
                        distance: current.distance + ride_distance,
                        first_boarding_time: current.first_boarding_time.or(Some(boarding_time)),
                        first_route_id: current
                            .first_route_id
                            .clone()
                            .or_else(|| Some(route.id.external().to_string())),
                        first_line_id: current
                            .first_line_id
                            .clone()
                            .or_else(|| Some(line.id.external().to_string())),
                        first_access_id: current
                            .first_access_id
                            .clone()
                            .or_else(|| Some(stop_id.external().to_string())),
                    };
                    if states
                        .get(&alight_stop.facility_id)
                        .is_none_or(|old| state.arrival < old.arrival)
                    {
                        let next_stop = alight_stop.facility_id.clone();
                        states.insert(next_stop.clone(), state.clone());
                        queue.push(Reverse((state.arrival, next_stop)));
                    }
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
        use crate::simulation::scenario::facilities::Facility;
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
        use crate::simulation::scenario::facilities::Facility;
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
