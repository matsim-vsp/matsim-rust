use crate::simulation::InternalAttributes;
use crate::simulation::config::{
    IntermodalAccessEgress, IntermodalLegOnlyHandling, IntermodalModeSelection, ModalLinkSelection,
    TransferConstruction, TransitRangeQuerySettings, TransitRouteSelectorSettings,
    TransitTransferPenalty,
};
use crate::simulation::id::Id;
use crate::simulation::pt::feedback::{TransitSegment, TransitSegmentObservation};
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::facilities::ActivityFacility;
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::population::{
    InternalActivity, InternalGenericRoute, InternalLeg, InternalPerson, InternalPlanElement,
    InternalPtRoute, InternalPtRouteDescription, InternalRoute, Population,
};
use crate::simulation::scenario::transit::{ChainedDeparture, TransitDeparture};
use crate::simulation::scenario::transit::{TransitSchedule, TransitStopFacility};
use crate::simulation::scenario::vehicles::{Garage, InternalVehicle};
use crate::simulation::time::SimTime;
use arc_swap::ArcSwap;
use derive_builder::Builder;
use nohash_hasher::IntMap;
use rand::RngExt;
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::{Debug, Formatter};
use std::mem::size_of;
use std::sync::{Arc, RwLock};
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
    max_walk_duration: Option<Duration>,
}

const ROUTE_PROPOSAL_MAX_ENTRIES: usize = 4096;
const ROUTE_PROPOSAL_MAX_BYTES: usize = 4 * 1024 * 1024;

impl Default for TripRouter {
    fn default() -> Self {
        Self {
            modules: IntMap::default(),
            route_proposals: Arc::new(ArcSwap::from_pointee(RouteProposalTable::default())),
            max_walk_duration: None,
        }
    }
}

impl TripRouter {
    pub fn new(modules: IntMap<Id<String>, Arc<dyn RoutingModule>>) -> Self {
        Self {
            modules,
            route_proposals: Arc::new(ArcSwap::from_pointee(RouteProposalTable::default())),
            max_walk_duration: None,
        }
    }

    pub(crate) fn with_max_walk_duration(mut self, maximum: Option<Duration>) -> Self {
        self.max_walk_duration = maximum;
        self
    }

    pub fn has_module(&self, mode: &Id<String>) -> bool {
        self.modules.contains_key(mode)
    }

    #[tracing::instrument(
        level = "trace",
        target = "matsim_rust::simulation::agents::agent_logic",
        skip(self, request),
        fields(
            person_id = request.person().map_or("", |person| person.id().external()),
            mode = mode.external()
        )
    )]
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

        if let Some(maximum) = self.max_walk_duration {
            let walking = walking_travel_time(&elements)?;
            if walking > maximum {
                return Err(RoutingError::WalkingLimitExceeded {
                    mode: mode.external().to_owned(),
                    walking,
                    maximum,
                });
            }
        }

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
    #[error("No feasible {mode} route: total walking time {walking:?} exceeds limit {maximum:?}")]
    WalkingLimitExceeded {
        mode: String,
        walking: Duration,
        maximum: Duration,
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
    #[error("Transit stop {stop} attribute {key} is malformed: {reason}")]
    MalformedTransitStopAttribute {
        stop: String,
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
    stop_cell_bounds: Option<(i32, i32, i32, i32)>,
    routes_by_stop: HashMap<Id<TransitStopFacility>, Vec<RouteStopRef>>,
    departures_by_route: HashMap<
        (
            Id<crate::simulation::scenario::transit::TransitLine>,
            Id<crate::simulation::scenario::transit::TransitRoute>,
        ),
        Vec<usize>,
    >,
    distance_prefix_by_route: HashMap<
        (
            Id<crate::simulation::scenario::transit::TransitLine>,
            Id<crate::simulation::scenario::transit::TransitRoute>,
        ),
        Vec<f64>,
    >,
    has_chained_departures: bool,
    transfers_by_stop: HashMap<Id<TransitStopFacility>, Vec<(Id<TransitStopFacility>, f64)>>,
    cell_size: f64,
    walk_speed: f64,
    walk_distance_factor: f64,
    max_walk_duration: Option<Duration>,
    garage: Arc<Garage>,
    fallback: Option<Arc<dyn RoutingModule>>,
    transfer_construction: TransferConstruction,
    transfer_cache: RwLock<HashMap<Id<TransitStopFacility>, Vec<(Id<TransitStopFacility>, f64)>>>,
    /// Let a request that carries no person fall back to the car router. That is the legacy
    /// behaviour SILO's zone-to-zone queries rely on; it is an application policy, not a
    /// passenger one, so it stays off unless a config asks for it. See `with_personless_fallback`.
    personless_fallback: bool,
    intermodal_access_egress: Vec<IntermodalAccessEgress>,
    feeder_routers: IntMap<Id<String>, Arc<dyn RoutingModule>>,
    feeder_mode_utilities: BTreeMap<String, f64>,
    mode_selection: IntermodalModeSelection,
    leg_only_handling: IntermodalLegOnlyHandling,
    passenger_modes: std::collections::BTreeMap<String, String>,
    use_passenger_mode_mapping: bool,
    /// Resolved per-subpopulation routing parameters, keyed by subpopulation. An empty string
    /// entry stores the default that applies to every subpopulation without an explicit override.
    /// Built once from the configured `mode_params` / `agent_params` so each request resolves
    /// costs with a single `BTreeMap` lookup.
    routing_params_by_subpopulation: std::collections::BTreeMap<String, ResolvedRoutingParams>,
    range_query_settings: Vec<TransitRangeQuerySettings>,
    route_selector_settings: Vec<TransitRouteSelectorSettings>,
    transfer_penalty: TransitTransferPenalty,
    random_seed: u64,
    capacity_feedback: Arc<ArcSwap<BTreeMap<TransitSegment, TransitSegmentObservation>>>,
}

/// The three numbers MATSim's travel-time cost formula needs: the agent's performing utility, the
/// baseline pt utility, and a per-mode utility map that lets bus and rail legs disagree about how
/// fast time passes. Reference semantics are intentional: the routing module hands the same
/// resolved struct to every leg-cost call within one request, so a passenger never pays different
/// costs for two halves of the same trip.
#[derive(Clone, Debug)]
pub struct ResolvedRoutingParams {
    /// `agent_params[person].performing` for the request's subpopulation, or the config default
    /// (6.0) when no agent parameter overrides it. Always finite: the controller rejects a config
    /// whose `performing` is non-finite.
    pub performing_utility_per_hour: f64,
    /// `mode_params[pt].marginal_utility_of_traveling` for the request's subpopulation, falling
    /// back to the empty-subpopulation entry, then -6.0. Always finite by the same controller check.
    pub pt_utility_per_hour: f64,
    /// Per-mode marginal utilities of traveling (utils/hour) for the request's subpopulation.
    /// Resolution order: matching subpopulation, then empty subpopulation, then -6.0 for `pt`.
    pub mode_utilities: std::collections::BTreeMap<String, f64>,
}

#[derive(Clone)]
struct RouteStopRef {
    line_id: Id<crate::simulation::scenario::transit::TransitLine>,
    route_id: Id<crate::simulation::scenario::transit::TransitRoute>,
    stop_index: usize,
}

struct BoardingCandidate {
    line_id: Id<crate::simulation::scenario::transit::TransitLine>,
    route_id: Id<crate::simulation::scenario::transit::TransitRoute>,
    stop_index: usize,
    state_id: u64,
    transfer_distance: f64,
    transfer_time: Duration,
}

#[derive(Clone, Copy)]
struct ActiveBoarding {
    state_id: u64,
    board_index: usize,
    departure_index: usize,
    transfer_distance: f64,
    transfer_time: Duration,
    chained_continuation: bool,
    base_departure: SimTime,
}

/// One ride in a transit vehicle from boarding to alighting.
#[derive(Clone)]
struct Ride {
    line: Id<crate::simulation::scenario::transit::TransitLine>,
    route: Id<crate::simulation::scenario::transit::TransitRoute>,
    departure: Id<TransitDeparture>,
    board_index: usize,
    alight_index: usize,
    board: Id<TransitStopFacility>,
    alight: Id<TransitStopFacility>,
    boarding_time: SimTime,
    /// When the vehicle reached the boarding stop, which is earlier than `boarding_time` by any
    /// dwell there. MATSim starts the clock it prices a transfer penalty against at
    /// `max(agent arrival, vehicle arrival)`, so this is needed to reproduce that origin.
    vehicle_arrival_at_board: SimTime,
    alighting_time: SimTime,
    distance: f64,
    /// The route's transport mode, which MATSim's mode-to-mode transfer penalties are keyed on.
    /// Distinct from `passenger_mode`, the mapped mode the leg is reported under.
    transport_mode: String,
    passenger_mode: String,
    transfer_before: Option<(f64, f64, Duration)>,
    chained_from_previous: bool,
}

/// A door-to-door transit connection.
#[derive(Clone)]
struct TransitPath {
    stop: Id<TransitStopFacility>,
    departure: SimTime,
    arrival: SimTime,
    access_distance: f64,
    egress_distance: f64,
    rides: Vec<Ride>,
    access_route: Option<Arc<Vec<InternalPlanElement>>>,
    egress_route: Option<Arc<Vec<InternalPlanElement>>>,
    access_time: Duration,
}

#[derive(Clone)]
struct TransitPathState {
    state_id: u64,
    stop: Id<TransitStopFacility>,
    parent_state_id: Option<u64>,
    ride: Option<Ride>,
    ride_count: usize,
    arrival: SimTime,
    cost: f64,
    transfer_penalty_seconds: f64,
    transfer_count: usize,
    first_vehicle_arrival_at_board: Option<SimTime>,
    access_distance: f64,
    access_route: Option<Arc<Vec<InternalPlanElement>>>,
    access_time: Duration,
    walking_time: Duration,
}

fn reconstruct_transit_rides(state_id: u64, states: &[TransitPathState]) -> Vec<Ride> {
    let mut rides = Vec::new();
    let mut current = Some(state_id);
    while let Some(state_id) = current {
        let state = &states[state_id as usize];
        if let Some(ride) = &state.ride {
            rides.push(ride.clone());
        }
        current = state.parent_state_id;
    }
    rides.reverse();
    rides
}

#[derive(Clone)]
struct FeederStop {
    stop: Id<TransitStopFacility>,
    distance: f64,
    time: Duration,
    disutility: f64,
    route: Option<Arc<Vec<InternalPlanElement>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitSkimOutcome {
    Transit,
    Walking,
    /// At least one endpoint has no nearby stop candidate; direct walking may still be possible.
    NoPath,
}

impl TransitSkimOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transit => "pt",
            Self::Walking => "walk",
            Self::NoPath => "no_path",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransitSkimResult {
    pub outcome: TransitSkimOutcome,
    pub travel_time: Option<Duration>,
}

trait TransitSubpopulations {
    fn subpopulations(&self) -> &[String];
}

impl TransitSubpopulations for TransitRangeQuerySettings {
    fn subpopulations(&self) -> &[String] {
        &self.subpopulations
    }
}

impl TransitSubpopulations for TransitRouteSelectorSettings {
    fn subpopulations(&self) -> &[String] {
        &self.subpopulations
    }
}

fn matching_transit_settings<'a, T: TransitSubpopulations>(
    settings: &'a [T],
    subpopulation: &str,
) -> Option<&'a T> {
    settings
        .iter()
        .find(|setting| {
            setting
                .subpopulations()
                .iter()
                .any(|candidate| candidate == subpopulation)
        })
        .or_else(|| {
            settings
                .iter()
                .find(|setting| setting.subpopulations().is_empty())
        })
}

/// Build a per-subpopulation map of resolved routing parameters from the configured scoring
/// inputs. Each entry bundles the performing utility for its subpopulation with the matching mode
/// utilities; a subpopulation absent from the input gets no entry here, so resolution falls back
/// to the empty-subpopulation entry (the global default) and finally to the built-in constants.
///
/// The output is keyed by `subpopulation` (verbatim from the config); empty string is the global
/// default entry.
fn build_routing_params_by_subpopulation(
    mode_params: &[crate::simulation::config::ModeParameter],
    agent_params: &[crate::simulation::config::AgentParameter],
) -> std::collections::BTreeMap<String, ResolvedRoutingParams> {
    let mut mode_by_subpopulation: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, f64>,
    > = std::collections::BTreeMap::new();
    for params in mode_params {
        mode_by_subpopulation
            .entry(params.subpopulation.clone())
            .or_default()
            .insert(params.mode.clone(), params.marginal_utility_of_traveling);
    }
    let mut by_subpopulation: std::collections::BTreeMap<String, ResolvedRoutingParams> =
        std::collections::BTreeMap::new();
    let subpopulations: std::collections::BTreeSet<String> = mode_by_subpopulation
        .keys()
        .cloned()
        .chain(agent_params.iter().map(|p| p.subpopulation.clone()))
        .collect();
    for subpopulation in subpopulations {
        let mode_utilities = mode_by_subpopulation
            .get(&subpopulation)
            .cloned()
            .unwrap_or_else(|| mode_by_subpopulation.get("").cloned().unwrap_or_default());
        let pt_utility = mode_utilities.get("pt").copied().unwrap_or(-6.0);
        // The performing utility is per-subpopulation; an absent agent_params entry for the
        // requested subpopulation inherits the global default ("person" subpopulation) at
        // 6.0 utils/hour — the same fallback MATSim uses when no `person` override is set.
        let performing = agent_params
            .iter()
            .find(|params| params.subpopulation == subpopulation)
            .map_or(6.0, |params| params.performing);
        by_subpopulation.insert(
            subpopulation,
            ResolvedRoutingParams {
                performing_utility_per_hour: performing,
                pt_utility_per_hour: pt_utility,
                mode_utilities,
            },
        );
    }
    by_subpopulation
}

// ponytail: This search cap is fixed at 20 until MATSim's configurable transfer limit is ported.
const RAPTOR_MAX_TRANSFERS: usize = 20;
const RAPTOR_MIN_TRANSFER_TIME: Duration = Duration::from_secs(60);
const RAPTOR_TRANSFER_WALK_MARGIN: Duration = Duration::from_secs(5);
const RAPTOR_MAX_WALK_TRANSFER_DISTANCE: f64 = 200.0;

impl RoutingModule for TransitRoutingModule {
    fn calc_route(
        &self,
        request: RoutingRequest,
    ) -> Result<Vec<InternalPlanElement>, RoutingError> {
        let from = request.from.coord();
        let to = request.to.coord();
        let (access_stops, egress_stops) = if self.intermodal_access_egress.is_empty() {
            let access = self
                .nearest_stops(from)
                .into_iter()
                .map(|(stop, distance)| FeederStop {
                    stop,
                    distance,
                    time: self.walk_time(distance),
                    disutility: 0.0,
                    route: None,
                })
                .collect();
            let egress = self
                .nearest_stops(to)
                .into_iter()
                .map(|(stop, distance)| FeederStop {
                    stop,
                    distance,
                    time: self.walk_time(distance),
                    disutility: 0.0,
                    route: None,
                })
                .collect();
            (access, egress)
        } else {
            (
                self.intermodal_stops(&request, request.from, true)?,
                self.intermodal_stops(&request, request.to, false)?,
            )
        };
        let direct_walk_distance = Coordinate::euclidean_distance(from, to);
        let direct_walk_time = self.walk_time(direct_walk_distance);
        let direct_walk = || {
            InternalPlanElement::Leg(InternalLeg::new(
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
            ))
        };
        let subpopulation = request
            .person()
            .map_or("", |person| person.subpopulation().external());
        // Resolve per-subpopulation routing costs once. The TripRouter builds the itinerary
        // here and downstream scoring picks up the same passenger mode, so they have to agree
        // on the cost formula. Resolving once and threading the same struct through every
        // leg-cost call inside this request is what keeps that agreement exact.
        let routing_params = self.resolve_routing_params(subpopulation);
        let capacity_feedback = self.capacity_feedback.load_full();
        let best = if let Some(settings) =
            matching_transit_settings(&self.range_query_settings, subpopulation)
        {
            self.select_range_query_path(
                to,
                request.departure_time,
                &access_stops,
                &egress_stops,
                settings,
                request.person(),
                &routing_params,
                &capacity_feedback,
            )
            .or_else(|| {
                self.find_best_path_with_feeders(
                    to,
                    request.departure_time,
                    &access_stops,
                    &egress_stops,
                    &routing_params,
                    &capacity_feedback,
                )
            })
        } else {
            self.find_best_path_with_feeders(
                to,
                request.departure_time,
                &access_stops,
                &egress_stops,
                &routing_params,
                &capacity_feedback,
            )
        };
        let Some(path) = best else {
            if !access_stops.is_empty()
                && !egress_stops.is_empty()
                && self.walking_allowed(direct_walk_time)
            {
                return Ok(vec![direct_walk()]);
            }
            // When transit and permitted walking cannot connect the endpoints, try a car trip.
            // But a car trip is only a legitimate answer for an agent that declares owning one
            // and has a car to drive. Everyone else gets the no-path outcome, which the caller
            // must report as an infeasible trip.
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
        if self.walking_allowed(direct_walk_time)
            && direct_walk_time.as_secs_f64()
                < self.path_cost_equivalent_seconds_with_feedback(
                    &path,
                    request.departure_time,
                    &routing_params,
                    &capacity_feedback,
                )
        {
            return Ok(vec![direct_walk()]);
        }
        Ok(self.stop_to_stop_trip(&request, &path))
    }

    fn mode(&self) -> &Id<String> {
        &self.mode
    }
}

impl TransitRoutingModule {
    pub(crate) fn with_max_walk_duration(mut self, maximum: Option<Duration>) -> Self {
        self.max_walk_duration = maximum;
        self
    }

    fn walking_allowed(&self, duration: Duration) -> bool {
        self.max_walk_duration
            .is_none_or(|maximum| duration <= maximum)
    }

    fn walking_after_boarding(
        &self,
        state: &TransitPathState,
        transfer_time: Duration,
        chained: bool,
        walking_budget: Option<Duration>,
    ) -> Duration {
        if walking_budget.is_none() {
            return Duration::ZERO;
        }
        if state.ride_count == 0 || chained {
            state.walking_time
        } else {
            state
                .walking_time
                .saturating_add(transfer_time.saturating_sub(RAPTOR_TRANSFER_WALK_MARGIN))
        }
    }

    fn feeder_walking_time(&self, feeder: &FeederStop) -> Duration {
        if self.max_walk_duration.is_none() {
            return Duration::ZERO;
        }
        feeder.route.as_ref().map_or_else(
            || self.walk_time(feeder.distance),
            |route| walking_travel_time(route).expect("feeder walking times were validated"),
        )
    }

    fn path_walking_time(&self, path: &TransitPath) -> Duration {
        let feeder_time = |route: &Option<Arc<Vec<InternalPlanElement>>>, distance| {
            route.as_ref().map_or_else(
                || self.walk_time(distance),
                |route| walking_travel_time(route).expect("feeder walking times were validated"),
            )
        };
        let mut walking = feeder_time(&path.access_route, path.access_distance)
            .saturating_add(feeder_time(&path.egress_route, path.egress_distance));
        for ride in &path.rides {
            if let Some((_, _, duration)) = ride.transfer_before {
                walking =
                    walking.saturating_add(duration.saturating_sub(RAPTOR_TRANSFER_WALK_MARGIN));
            }
        }
        walking
    }

    pub(crate) fn with_capacity_feedback_snapshot(
        mut self,
        snapshot: Arc<ArcSwap<BTreeMap<TransitSegment, TransitSegmentObservation>>>,
    ) -> Self {
        self.capacity_feedback = snapshot;
        self
    }

    const CELL_SIZE: f64 = 1_000.0;
    const STOP_SEARCH_RADIUS: f64 = 1_000.0;
    const STOP_SEARCH_EXTENSION_RADIUS: f64 = 200.0;
    const WALK_MODE: &'static str = "walk";
    const INTERACTION: &'static str = "pt interaction";

    pub(crate) fn with_intermodal_access_egress(
        mut self,
        settings: Vec<IntermodalAccessEgress>,
        routers: IntMap<Id<String>, Arc<dyn RoutingModule>>,
        utilities: BTreeMap<String, f64>,
        selection: IntermodalModeSelection,
        leg_only_handling: IntermodalLegOnlyHandling,
        random_seed: u64,
    ) -> Self {
        self.intermodal_access_egress = settings;
        self.feeder_routers = routers;
        self.feeder_mode_utilities = utilities;
        self.mode_selection = selection;
        self.leg_only_handling = leg_only_handling;
        self.random_seed = random_seed;
        self
    }

    fn intermodal_stops(
        &self,
        request: &RoutingRequest,
        endpoint: &Facility<'_>,
        access: bool,
    ) -> Result<Vec<FeederStop>, RoutingError> {
        let mut settings: Vec<_> = self.intermodal_access_egress.iter().collect();
        if self.mode_selection == IntermodalModeSelection::RandomPerDirection {
            let person_id = request
                .person()
                .map_or("personless", |person| person.id().external());
            let direction = if access { "access" } else { "egress" };
            let stream = format!(
                "{person_id}:{}:{}:{}:{direction}",
                request.from.link(),
                request.to.link(),
                request.departure_time
            );
            let mut rng = crate::simulation::random::get_rng(
                self.random_seed,
                "routing.pt.intermodal_mode",
                &stream,
            );
            let start = rng.random_range(0..settings.len());
            settings.rotate_left(start);
        }

        let trip_radius = Coordinate::euclidean_distance(request.from.coord(), request.to.coord());
        let mut selected = Vec::new();
        for setting in settings {
            if self.mode_selection == IntermodalModeSelection::RandomPerDirection
                && !selected.is_empty()
            {
                break;
            }
            if let (Some(attribute), Some(value)) = (
                setting.person_filter_attribute.as_deref(),
                setting.person_filter_value.as_deref(),
            ) && !request.person().is_some_and(|person| {
                person.attributes().get::<String>(attribute).as_deref() == Some(value)
            }) {
                continue;
            }
            let Some(router) = self.feeder_routers.get(&Id::create(&setting.mode)) else {
                continue;
            };
            let initial_radius = setting
                .initial_search_radius
                .min(setting.max_radius)
                .min(trip_radius * setting.share_trip_search_radius);
            let stop_filter = setting
                .stop_filter_attribute
                .as_deref()
                .zip(setting.stop_filter_value.as_deref());
            let initial_stops =
                self.stops_within_radius_filtered(endpoint.coord(), initial_radius, stop_filter);
            let search_radius = if initial_stops.len() < 2 {
                self.nearest_stop_filtered(endpoint.coord(), stop_filter)
                    .map_or(initial_radius, |(_, nearest)| {
                        (nearest + setting.search_extension_radius).min(setting.max_radius)
                    })
            } else {
                initial_radius
            };

            let stops = if search_radius == initial_radius {
                initial_stops
            } else {
                self.stops_within_radius_filtered(endpoint.coord(), search_radius, stop_filter)
            };
            for (stop_id, distance) in stops {
                let stop = self.schedule.get_facility(&stop_id);
                let actual_link = self.stop_link(&stop_id).clone();
                let feeder_link = setting
                    .link_id_attribute
                    .as_deref()
                    .and_then(|attribute| stop.attributes.get::<String>(attribute))
                    .map(|link| Id::<Link>::create(&link))
                    .unwrap_or_else(|| actual_link.clone());
                let connector_time =
                    transit_stop_time(stop, if access { "accessTime" } else { "egressTime" })?;
                let connector_time = Duration::try_from_secs_f64(connector_time).map_err(|_| {
                    RoutingError::MalformedTransitStopAttribute {
                        stop: stop.id.external().to_owned(),
                        key: if access { "accessTime" } else { "egressTime" }.to_owned(),
                        reason: "seconds exceed the supported duration range".to_owned(),
                    }
                })?;
                let stop_endpoint =
                    Facility::new_link_wrapper(stop.coord.clone(), feeder_link.clone());
                let (from, to) = if access {
                    (endpoint, &stop_endpoint)
                } else {
                    (&stop_endpoint, endpoint)
                };
                let route_request = RoutingRequestBuilder::default()
                    .from(from)
                    .to(to)
                    .departure_time(request.departure_time)
                    .person(request.person())
                    .attributes(request.attributes.clone())
                    .build()
                    .expect("required feeder routing request fields are set");
                let mut route = match router.calc_route(route_request) {
                    Ok(route) => route,
                    Err(RoutingError::NoPath { .. }) => continue,
                    Err(error) => return Err(error),
                };
                if self.max_walk_duration.is_some() {
                    walking_travel_time(&route)?;
                }
                let feeder_time = plan_travel_time(&route);
                if feeder_link != actual_link || !connector_time.is_zero() {
                    let connector = InternalPlanElement::Leg(InternalLeg::new(
                        InternalRoute::Generic(InternalGenericRoute::new(
                            if access {
                                feeder_link.clone()
                            } else {
                                actual_link.clone()
                            },
                            if access {
                                actual_link.clone()
                            } else {
                                feeder_link.clone()
                            },
                            Some(connector_time),
                            Some(0.0),
                            None,
                        )),
                        Self::WALK_MODE,
                        self.mode.external(),
                        connector_time,
                        access.then(|| request.departure_time.saturating_add(feeder_time)),
                    ));
                    if access {
                        route.push(connector);
                    } else {
                        route.insert(0, connector);
                    }
                }
                if !access {
                    for element in &mut route {
                        if let InternalPlanElement::Leg(leg) = element {
                            leg.dep_time = None;
                        }
                    }
                }
                let time = plan_travel_time(&route);
                let disutility = route_disutility(&route, &self.feeder_mode_utilities);
                let candidate = FeederStop {
                    stop: stop_id,
                    distance,
                    time,
                    disutility,
                    route: Some(Arc::new(route)),
                };
                if !self.walking_allowed(self.feeder_walking_time(&candidate)) {
                    continue;
                }
                // With a walking budget, retain feeder modes for joint access/egress selection.
                if self.mode_selection == IntermodalModeSelection::LeastCostPerStop
                    && self.max_walk_duration.is_none()
                {
                    if let Some(index) = selected
                        .iter()
                        .position(|old: &FeederStop| old.stop == candidate.stop)
                    {
                        if candidate.disutility < selected[index].disutility {
                            selected[index] = candidate;
                        }
                        continue;
                    }
                }
                selected.push(candidate);
            }
        }
        Ok(selected)
    }

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
        self.routing_params_by_subpopulation =
            build_routing_params_by_subpopulation(scoring, agent_scoring);
        self
    }

    /// Resolves the routing parameters for a single subpopulation, applying the documented
    /// precedence: explicit subpopulation match, then the empty-subpopulation entry, then -6.0
    /// for the `pt` baseline. Reference semantics matter: the returned struct is read-only here
    /// and the routing module hands it to every leg-cost call inside one request.
    fn resolve_routing_params(&self, subpopulation: &str) -> ResolvedRoutingParams {
        if let Some(params) = self.routing_params_by_subpopulation.get(subpopulation) {
            return params.clone();
        }
        if let Some(params) = self.routing_params_by_subpopulation.get("") {
            return params.clone();
        }
        // No subpopulation entry and no empty-subpopulation default — fall back to the built-in
        // defaults so a request without a configured subpopulation still gets finite costs.
        ResolvedRoutingParams {
            performing_utility_per_hour: 6.0,
            pt_utility_per_hour: -6.0,
            mode_utilities: std::collections::BTreeMap::new(),
        }
    }

    pub(crate) fn with_range_queries(
        mut self,
        range_query_settings: Vec<TransitRangeQuerySettings>,
        route_selector_settings: Vec<TransitRouteSelectorSettings>,
        random_seed: u64,
    ) -> Self {
        self.range_query_settings = range_query_settings;
        if !route_selector_settings.is_empty() {
            self.route_selector_settings = route_selector_settings;
        }
        self.random_seed = random_seed;
        self
    }

    /// Sets the transfer penalties a PT itinerary pays. With MATSim's defaults this reproduces
    /// the previous fixed cost of one utility, i.e. 300 seconds, per transfer.
    pub(crate) fn with_transfer_penalty(
        mut self,
        transfer_penalty: TransitTransferPenalty,
    ) -> Self {
        self.transfer_penalty = transfer_penalty;
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
        if path.rides.is_empty() {
            let mut access: Vec<_> = path
                .access_route
                .as_deref()
                .into_iter()
                .flatten()
                .cloned()
                .collect();
            let mut egress = path
                .egress_route
                .as_deref()
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>();
            if let (Some(InternalPlanElement::Leg(leg)), Some(InternalPlanElement::Leg(_))) =
                (access.last(), egress.first())
            {
                access.push(InternalPlanElement::Activity(InternalActivity::new(
                    Some(self.schedule.get_facility(&path.stop).coord.clone()),
                    Self::INTERACTION,
                    leg.route.as_ref().unwrap().end_link().clone(),
                    None,
                    None,
                    Some(Duration::ZERO),
                )));
            }
            access.append(&mut egress);
            return access;
        }
        let walk_leg = |from: &Id<Link>,
                        to: &Id<Link>,
                        route_distance: f64,
                        travel_time: Duration,
                        departure: SimTime| {
            InternalPlanElement::Leg(InternalLeg::new(
                InternalRoute::Generic(InternalGenericRoute::new(
                    from.clone(),
                    to.clone(),
                    Some(travel_time),
                    Some(route_distance),
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

        let rides = collapse_chained_rides(&path.rides);
        let first_stop = &rides[0].board;
        let mut elements = path.access_route.as_ref().map_or_else(
            || {
                vec![walk_leg(
                    request.from.link(),
                    self.stop_link(first_stop),
                    path.access_distance * self.walk_distance_factor,
                    self.walk_time(path.access_distance),
                    path.departure,
                )]
            },
            |route| route.as_ref().clone(),
        );
        elements.push(interaction(first_stop));
        let mut time = path.departure.saturating_add(path.access_time);
        for (ride_index, ride) in rides.iter().enumerate() {
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
            if let Some(next_ride) = rides.get(ride_index + 1) {
                let (_, transfer_distance, transfer_time) = next_ride
                    .transfer_before
                    .expect("every ride after the first has a transfer");
                elements.push(walk_leg(
                    self.stop_link(&ride.alight),
                    self.stop_link(&next_ride.board),
                    transfer_distance,
                    transfer_time.saturating_sub(RAPTOR_TRANSFER_WALK_MARGIN),
                    time,
                ));
                time = time.saturating_add(transfer_time);
                elements.push(interaction(&next_ride.board));
            }
        }
        let last_stop = &path.rides.last().unwrap().alight;
        if let Some(route) = &path.egress_route {
            elements.extend(route.iter().cloned());
        } else {
            elements.push(walk_leg(
                self.stop_link(last_stop),
                request.to.link(),
                path.egress_distance * self.walk_distance_factor,
                self.walk_time(path.egress_distance),
                time,
            ));
        }
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
        Self::new_with_transfer_construction(
            schedule,
            walk_speed,
            walk_distance_factor,
            garage,
            fallback,
            TransferConstruction::default(),
        )
    }

    pub(crate) fn new_with_transfer_construction(
        schedule: Arc<TransitSchedule>,
        walk_speed: f64,
        walk_distance_factor: f64,
        garage: Arc<Garage>,
        fallback: Option<Arc<dyn RoutingModule>>,
        transfer_construction: TransferConstruction,
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
        let stop_cell_bounds: Option<(i32, i32, i32, i32)> =
            stops_by_cell.keys().fold(None, |bounds, &(x, y)| {
                Some(match bounds {
                    Some((min_x, min_y, max_x, max_y)) => {
                        (min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y))
                    }
                    None => (x, y, x, y),
                })
            });
        let mut routes_by_stop: HashMap<Id<TransitStopFacility>, Vec<RouteStopRef>> =
            HashMap::new();
        let mut departures_by_route = HashMap::new();
        let mut distance_prefix_by_route = HashMap::new();
        let mut has_chained_departures = false;
        let mut transfers_by_stop: HashMap<_, Vec<_>> = HashMap::new();
        for transfer in schedule.minimal_transfer_times() {
            transfers_by_stop
                .entry(transfer.from_stop.clone())
                .or_default()
                .push((transfer.to_stop.clone(), transfer.transfer_time));
        }
        for line in schedule.lines().values() {
            for route in line.routes.values() {
                has_chained_departures |= route
                    .departures
                    .iter()
                    .any(|departure| !departure.chained_departures.is_empty());
                let mut distance_prefix = Vec::with_capacity(route.stops.len());
                distance_prefix.push(0.0);
                for pair in route.stops.windows(2) {
                    let from = schedule.get_facility(&pair[0].facility_id);
                    let to = schedule.get_facility(&pair[1].facility_id);
                    distance_prefix.push(
                        distance_prefix.last().copied().unwrap_or_default()
                            + Coordinate::euclidean_distance(&from.coord, &to.coord),
                    );
                }
                distance_prefix_by_route
                    .insert((line.id.clone(), route.id.clone()), distance_prefix);
                let mut departure_indices = (0..route.departures.len()).collect::<Vec<_>>();
                departure_indices.sort_by(|left, right| {
                    route.departures[*left]
                        .departure_time
                        .cmp(&route.departures[*right].departure_time)
                        .then_with(|| {
                            route.departures[*left]
                                .id
                                .external()
                                .cmp(route.departures[*right].id.external())
                        })
                });
                departures_by_route.insert((line.id.clone(), route.id.clone()), departure_indices);
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
        let router = Self {
            mode: Id::create("pt"),
            schedule,
            stops_by_cell,
            stop_cell_bounds,
            routes_by_stop,
            departures_by_route,
            distance_prefix_by_route,
            has_chained_departures,
            transfers_by_stop,
            cell_size: Self::CELL_SIZE,
            walk_speed: walk_speed.max(0.1),
            walk_distance_factor,
            max_walk_duration: None,
            garage,
            fallback,
            transfer_construction,
            transfer_cache: RwLock::new(HashMap::new()),
            personless_fallback: false,
            intermodal_access_egress: Vec::new(),
            feeder_routers: IntMap::default(),
            feeder_mode_utilities: BTreeMap::new(),
            mode_selection: IntermodalModeSelection::LeastCostPerStop,
            leg_only_handling: IntermodalLegOnlyHandling::Forbid,
            passenger_modes: std::collections::BTreeMap::new(),
            use_passenger_mode_mapping: false,
            routing_params_by_subpopulation: std::collections::BTreeMap::new(),
            range_query_settings: Vec::new(),
            route_selector_settings: vec![TransitRouteSelectorSettings::default()],
            transfer_penalty: TransitTransferPenalty::default(),
            random_seed: crate::simulation::config::DEFAULT_RANDOM_SEED,
            capacity_feedback: Arc::new(ArcSwap::from_pointee(BTreeMap::new())),
        };
        if transfer_construction == TransferConstruction::Initial {
            let mut cache = router.transfer_cache.write().unwrap();
            for stop_id in router.routes_by_stop.keys() {
                cache.insert(
                    stop_id.clone(),
                    router.calculate_nearby_transfer_stops(stop_id),
                );
            }
        }
        router
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
        self.skim_results_from_origin(origin, destinations, departure_time)
            .into_iter()
            .zip(destinations)
            .map(|(result, destination)| {
                result.travel_time.unwrap_or_else(|| {
                    self.walk_time(Coordinate::euclidean_distance(origin, destination))
                })
            })
            .collect()
    }

    pub fn skim_results_from_origin(
        &self,
        origin: &Coordinate,
        destinations: &[Coordinate],
        departure_time: SimTime,
    ) -> Vec<TransitSkimResult> {
        let access_stops = self.nearest_stops(origin);
        let capacity_feedback = self.capacity_feedback.load_full();
        // Skims have no per-person context — fall back to the empty-subpopulation default so
        // cost-equivalent travel time still uses passenger-mode utilities when configured.
        let routing_params = self.resolve_routing_params("");
        let egress_stops_by_destination = destinations
            .iter()
            .map(|destination| {
                let mut stops = self
                    .nearest_stops(destination)
                    .into_iter()
                    .map(|(stop, distance)| FeederStop {
                        stop,
                        distance,
                        time: self.walk_time(distance),
                        disutility: 0.0,
                        route: None,
                    })
                    .collect::<Vec<_>>();
                stops.sort_by(|left, right| left.stop.external().cmp(right.stop.external()));
                stops
            })
            .collect::<Vec<_>>();
        let mut candidates_by_destination = vec![Vec::new(); destinations.len()];
        for (destination_index, path) in self.find_paths_for_destinations(
            destinations,
            &egress_stops_by_destination,
            departure_time,
            &access_stops
                .iter()
                .map(|(stop, distance)| FeederStop {
                    stop: stop.clone(),
                    distance: *distance,
                    time: self.walk_time(*distance),
                    disutility: 0.0,
                    route: None,
                })
                .collect::<Vec<_>>(),
            &routing_params,
            &capacity_feedback,
        ) {
            candidates_by_destination[destination_index].push(path);
        }

        destinations
            .iter()
            .zip(&egress_stops_by_destination)
            .zip(candidates_by_destination)
            .map(|((destination, egress_stops), candidates)| {
                let direct_walk =
                    self.walk_time(Coordinate::euclidean_distance(origin, destination));
                match self.select_best_path_from_candidates(
                    &candidates,
                    departure_time,
                    &routing_params,
                    &capacity_feedback,
                ) {
                    Some(path)
                        if direct_walk.as_secs_f64()
                            >= self.path_cost_equivalent_seconds_with_feedback(
                                &path,
                                departure_time,
                                &routing_params,
                                &capacity_feedback,
                            ) =>
                    {
                        TransitSkimResult {
                            outcome: TransitSkimOutcome::Transit,
                            travel_time: Some(path.arrival.duration_since(departure_time)),
                        }
                    }
                    Some(_) => TransitSkimResult {
                        outcome: TransitSkimOutcome::Walking,
                        travel_time: Some(direct_walk),
                    },
                    None if !access_stops.is_empty() && !egress_stops.is_empty() => {
                        TransitSkimResult {
                            outcome: TransitSkimOutcome::Walking,
                            travel_time: Some(direct_walk),
                        }
                    }
                    None => TransitSkimResult {
                        outcome: TransitSkimOutcome::NoPath,
                        travel_time: None,
                    },
                }
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
        let stops = self.stops_within_radius_filtered(coordinate, Self::STOP_SEARCH_RADIUS, None);
        if stops.len() >= 2 {
            return stops;
        }
        let Some((_, nearest_distance)) = self.nearest_stop_filtered(coordinate, None) else {
            return stops;
        };
        self.stops_within_radius_filtered(
            coordinate,
            nearest_distance + Self::STOP_SEARCH_EXTENSION_RADIUS,
            None,
        )
    }

    fn nearest_stop_filtered(
        &self,
        coordinate: &Coordinate,
        stop_filter: Option<(&str, &str)>,
    ) -> Option<(Id<TransitStopFacility>, f64)> {
        let (center_x, center_y) = self.cell(coordinate);
        let (min_x, min_y, max_x, max_y) = self.stop_cell_bounds?;
        let max_radius = (center_x - min_x)
            .abs()
            .max((center_x - max_x).abs())
            .max((center_y - min_y).abs())
            .max((center_y - max_y).abs());
        let mut nearest: Option<(Id<TransitStopFacility>, f64)> = None;
        for radius in 0..=max_radius {
            for offset in -radius..=radius {
                self.consider_cell_stop(
                    (center_x + offset, center_y - radius),
                    coordinate,
                    stop_filter,
                    &mut nearest,
                );
                if radius > 0 {
                    self.consider_cell_stop(
                        (center_x + offset, center_y + radius),
                        coordinate,
                        stop_filter,
                        &mut nearest,
                    );
                }
                if offset != -radius && offset != radius {
                    self.consider_cell_stop(
                        (center_x - radius, center_y + offset),
                        coordinate,
                        stop_filter,
                        &mut nearest,
                    );
                    if radius > 0 {
                        self.consider_cell_stop(
                            (center_x + radius, center_y + offset),
                            coordinate,
                            stop_filter,
                            &mut nearest,
                        );
                    }
                }
            }
            if nearest
                .as_ref()
                .is_some_and(|(_, distance)| *distance < radius as f64 * self.cell_size)
            {
                break;
            }
        }
        nearest
    }

    fn consider_cell_stop(
        &self,
        cell: (i32, i32),
        coordinate: &Coordinate,
        stop_filter: Option<(&str, &str)>,
        nearest: &mut Option<(Id<TransitStopFacility>, f64)>,
    ) {
        let Some(stop_ids) = self.stops_by_cell.get(&cell) else {
            return;
        };
        for stop_id in stop_ids {
            let stop = self.schedule.get_facility(stop_id);
            if stop_filter.is_some_and(|(attribute, value)| {
                stop.attributes.get::<String>(attribute).as_deref() != Some(value)
            }) {
                continue;
            }
            let distance = Coordinate::euclidean_distance(coordinate, &stop.coord);
            if nearest
                .as_ref()
                .is_none_or(|(current_id, current_distance)| {
                    distance < *current_distance
                        || (distance == *current_distance
                            && stop_id.external() < current_id.external())
                })
            {
                *nearest = Some((stop_id.clone(), distance));
            }
        }
    }

    fn stops_within_radius_filtered(
        &self,
        coordinate: &Coordinate,
        radius: f64,
        stop_filter: Option<(&str, &str)>,
    ) -> Vec<(Id<TransitStopFacility>, f64)> {
        let min_x = ((coordinate.x - radius) / self.cell_size).floor() as i32;
        let max_x = ((coordinate.x + radius) / self.cell_size).floor() as i32;
        let min_y = ((coordinate.y - radius) / self.cell_size).floor() as i32;
        let max_y = ((coordinate.y + radius) / self.cell_size).floor() as i32;
        let mut stops = Vec::new();
        for x in min_x..=max_x {
            for y in min_y..=max_y {
                let Some(ids) = self.stops_by_cell.get(&(x, y)) else {
                    continue;
                };
                for id in ids {
                    let facility = self.schedule.get_facility(id);
                    if stop_filter.is_some_and(|(attribute, value)| {
                        facility.attributes.get::<String>(attribute).as_deref() != Some(value)
                    }) {
                        continue;
                    }
                    let distance = Coordinate::euclidean_distance(coordinate, &facility.coord);
                    if distance <= radius {
                        stops.push((id.clone(), distance));
                    }
                }
            }
        }
        stops.sort_by(|a, b| {
            a.1.total_cmp(&b.1)
                .then_with(|| a.0.external().cmp(b.0.external()))
        });
        stops
    }

    fn walk_time(&self, distance: f64) -> Duration {
        Duration::from_secs_f64(distance * self.walk_distance_factor / self.walk_speed)
    }

    fn transfer_time(
        &self,
        from: &Id<TransitStopFacility>,
        to: &Id<TransitStopFacility>,
        distance: f64,
    ) -> Duration {
        let seconds = self
            .transfers_by_stop
            .get(from)
            .and_then(|transfers| transfers.iter().find(|(stop, _)| stop == to))
            .map_or_else(
                || {
                    Duration::from_secs_f64(
                        self.walk_time(distance)
                            .as_secs_f64()
                            .max(RAPTOR_MIN_TRANSFER_TIME.as_secs_f64())
                            .ceil(),
                    )
                },
                |(_, time)| Duration::from_secs_f64(time.ceil()),
            );
        seconds.max(Duration::ZERO)
    }

    fn nearby_transfer_stops(
        &self,
        stop_id: &Id<TransitStopFacility>,
    ) -> Vec<(Id<TransitStopFacility>, f64)> {
        match self.transfer_construction {
            TransferConstruction::Initial => self.transfer_cache.read().unwrap()[stop_id].clone(),
            TransferConstruction::Adaptive => {
                if let Some(candidates) = self.transfer_cache.read().unwrap().get(stop_id) {
                    return candidates.clone();
                }
                let candidates = self.calculate_nearby_transfer_stops(stop_id);
                self.transfer_cache
                    .write()
                    .unwrap()
                    .entry(stop_id.clone())
                    .or_insert(candidates)
                    .clone()
            }
            TransferConstruction::Online => self.calculate_nearby_transfer_stops(stop_id),
        }
    }

    fn calculate_nearby_transfer_stops(
        &self,
        stop_id: &Id<TransitStopFacility>,
    ) -> Vec<(Id<TransitStopFacility>, f64)> {
        let from = &self.schedule.get_facility(stop_id).coord;
        let (center_x, center_y) = self.cell(from);
        let mut stops = Vec::new();
        for x_offset in -1..=1 {
            for y_offset in -1..=1 {
                if let Some(candidates) = self.stops_by_cell.get(&(
                    center_x.saturating_add(x_offset),
                    center_y.saturating_add(y_offset),
                )) {
                    for candidate in candidates {
                        if candidate == stop_id || !self.routes_by_stop.contains_key(candidate) {
                            continue;
                        }
                        let distance = Coordinate::euclidean_distance(
                            from,
                            &self.schedule.get_facility(candidate).coord,
                        );
                        if distance <= RAPTOR_MAX_WALK_TRANSFER_DISTANCE {
                            stops.push((candidate.clone(), distance));
                        }
                    }
                }
            }
        }
        for (to_stop, _) in self.transfers_by_stop.get(stop_id).into_iter().flatten() {
            if !stops.iter().any(|(candidate, _)| candidate == to_stop) {
                let distance = Coordinate::euclidean_distance(
                    from,
                    &self.schedule.get_facility(to_stop).coord,
                );
                stops.push((to_stop.clone(), distance));
            }
        }
        stops.sort_by(|left, right| {
            left.1
                .total_cmp(&right.1)
                .then_with(|| left.0.external().cmp(right.0.external()))
        });
        stops
    }

    #[cfg(test)]
    fn path_cost(&self, path: &TransitPath, departure_time: SimTime) -> Duration {
        let routing_params = self.resolve_routing_params("");
        path.arrival.duration_since(departure_time)
            + Duration::from_secs_f64(self.transfer_penalty_seconds(&path.rides, &routing_params))
    }

    fn path_cost_equivalent_seconds_with_feedback(
        &self,
        path: &TransitPath,
        departure_time: SimTime,
        routing_params: &ResolvedRoutingParams,
        feedback: &BTreeMap<TransitSegment, TransitSegmentObservation>,
    ) -> f64 {
        self.cost_equivalent_seconds_with_feedback(
            path.arrival,
            &path.rides,
            departure_time,
            routing_params,
            feedback,
        )
    }

    fn cost_equivalent_seconds_with_feedback(
        &self,
        arrival: SimTime,
        rides: &[Ride],
        departure_time: SimTime,
        routing_params: &ResolvedRoutingParams,
        feedback: &BTreeMap<TransitSegment, TransitSegmentObservation>,
    ) -> f64 {
        let base = arrival.duration_since(departure_time).as_secs_f64()
            - chained_dwell_seconds(rides)
            + self.transfer_penalty_seconds(rides, routing_params);
        let capacity_cost = rides
            .iter()
            .map(|ride| self.capacity_feedback_cost_seconds(ride, feedback))
            .sum::<f64>();
        if !self.use_passenger_mode_mapping {
            return base + capacity_cost;
        }
        let baseline =
            routing_params.performing_utility_per_hour - routing_params.pt_utility_per_hour;
        base + rides
            .iter()
            .map(|ride| {
                let utility = routing_params
                    .mode_utilities
                    .get(&ride.passenger_mode)
                    .copied()
                    .unwrap_or(routing_params.pt_utility_per_hour);
                let mode_cost_factor =
                    (routing_params.performing_utility_per_hour - utility) / baseline;
                // Transfer walking and waiting stay at the baseline PT cost, not the next mode's.
                ride.alighting_time
                    .duration_since(ride.boarding_time)
                    .as_secs_f64()
                    * (mode_cost_factor - 1.0)
            })
            .sum::<f64>()
            + capacity_cost
    }

    fn cost_after_ride(
        &self,
        current: &TransitPathState,
        ride: &Ride,
        arrival: SimTime,
        routing_params: &ResolvedRoutingParams,
        feedback: &BTreeMap<TransitSegment, TransitSegmentObservation>,
    ) -> (f64, f64, usize) {
        let chained = ride.chained_from_previous;
        let transfer_count = current.transfer_count
            + usize::from(current.ride_count > 0 && !chained && ride.transfer_before.is_some());
        let seconds_per_utility = 3600.0
            / (routing_params.performing_utility_per_hour - routing_params.pt_utility_per_hour);
        let transfer_penalty = if transfer_count == 0 {
            0.0
        } else if self.transfer_penalty.is_mode_specific() {
            if !chained && ride.transfer_before.is_some() {
                let previous = current.ride.as_ref().unwrap();
                self.transfer_penalty.clip(
                    self.transfer_penalty.base_cost()
                        + self
                            .transfer_penalty
                            .mode_penalty(&previous.transport_mode, &ride.transport_mode),
                ) * seconds_per_utility
            } else {
                current.transfer_penalty_seconds
            }
        } else {
            let origin = current
                .first_vehicle_arrival_at_board
                .unwrap_or(ride.vehicle_arrival_at_board);
            let elapsed = arrival.saturating_sub(origin).as_secs() as f64;
            self.transfer_penalty.clip(
                self.transfer_penalty.base_cost()
                    + self.transfer_penalty.per_travel_time_hour / 3600.0 * elapsed,
            ) * transfer_count as f64
                * seconds_per_utility
        };
        let chain_dwell = if chained {
            ride.boarding_time
                .duration_since(current.arrival)
                .as_secs_f64()
        } else {
            0.0
        };
        let in_vehicle_cost = if self.use_passenger_mode_mapping {
            let baseline =
                routing_params.performing_utility_per_hour - routing_params.pt_utility_per_hour;
            let utility = routing_params
                .mode_utilities
                .get(&ride.passenger_mode)
                .copied()
                .unwrap_or(routing_params.pt_utility_per_hour);
            let factor = (routing_params.performing_utility_per_hour - utility) / baseline;
            ride.alighting_time
                .duration_since(ride.boarding_time)
                .as_secs_f64()
                * (factor - 1.0)
        } else {
            0.0
        };
        let incremental_cost = arrival.duration_since(current.arrival).as_secs_f64() - chain_dwell
            + transfer_penalty
            - current.transfer_penalty_seconds
            + in_vehicle_cost
            + self.capacity_feedback_cost_seconds(ride, feedback);
        (
            current.cost + incremental_cost,
            transfer_penalty,
            transfer_count,
        )
    }

    fn supports_monotone_objective(
        &self,
        capacity_feedback: &BTreeMap<TransitSegment, TransitSegmentObservation>,
    ) -> bool {
        capacity_feedback.is_empty()
            && !self.use_passenger_mode_mapping
            && self.transfer_penalty.by_transport_mode.is_empty()
            && self.transfer_penalty.per_travel_time_hour == 0.0
            && self
                .transfer_penalty
                .clip(self.transfer_penalty.base_cost())
                >= 0.0
    }

    fn capacity_feedback_cost_seconds(
        &self,
        ride: &Ride,
        feedback: &BTreeMap<TransitSegment, TransitSegmentObservation>,
    ) -> f64 {
        if feedback.is_empty() {
            return 0.0;
        }
        let line = self.schedule.get_line(&ride.line);
        let route = &line.routes[&ride.route];
        let from = ride.board_index;
        let to = ride.alight_index;
        if from >= to {
            return 0.0;
        }

        let mut penalty = 0.0;
        for index in from..to {
            let from_stop = &route.stops[index];
            let to_stop = &route.stops[index + 1];
            let segment_seconds = to_stop
                .arrival_offset
                .or(to_stop.departure_offset)
                .unwrap_or_default()
                .saturating_sub(from_stop.departure_offset.unwrap_or_default())
                .as_secs_f64();
            let segment = TransitSegment {
                line: ride.line.clone(),
                route: ride.route.clone(),
                departure: ride.departure.clone(),
                from: from_stop.facility_id.clone(),
                to: to_stop.facility_id.clone(),
            };
            let Some(observation) = feedback.get(&segment) else {
                continue;
            };
            if observation.capacity > 0 {
                let occupancy = observation.passengers as f64 / observation.capacity as f64;
                penalty += segment_seconds * occupancy;
            }
            let boarding_attempts = observation
                .failed_boardings
                .saturating_add(observation.boarded);
            if index == from && boarding_attempts > 0 {
                let boarding_offset = route.stops[from].departure_offset.unwrap_or_default();
                let departure_indices =
                    &self.departures_by_route[&(ride.line.clone(), ride.route.clone())];
                let next = departure_indices.partition_point(|departure_index| {
                    route.departures[*departure_index]
                        .departure_time
                        .saturating_add(boarding_offset)
                        <= ride.boarding_time
                });
                let headway = departure_indices
                    .get(next)
                    .map(|departure_index| {
                        route.departures[*departure_index]
                            .departure_time
                            .saturating_add(boarding_offset)
                            .duration_since(ride.boarding_time)
                            .as_secs_f64()
                    })
                    .unwrap_or(0.0);
                penalty += headway * observation.failed_boardings as f64 / boarding_attempts as f64;
            }
        }
        penalty
    }

    /// Transfer penalties for a whole itinerary, priced into the seconds-based cost.
    ///
    /// MATSim keeps the penalty in utils and lets the search compare it against utils; this router
    /// compares seconds, so utils are converted at the module's seconds-per-utility rate.
    ///
    /// The two shapes mirror MATSim's two calculators. `DefaultRaptorTransferCostCalculator` gives
    /// every transfer one clipped cost that grows with the journey's elapsed travel time once a
    /// per-travel-time-hour cost is configured; because it is recomputed from scratch at each path
    /// element, the elapsed time runs from when riding began to the last alighting, which is
    /// MATSim's `newArrivalTime - firstDepartureTime`. `ModeSpecificTransferCostCalculator` instead
    /// adds the configured transport-mode offset per transfer and ignores travel time entirely; it
    /// is selected by configuring any mode pair, and configuration rejects combining the two.
    fn transfer_penalty_seconds(
        &self,
        rides: &[Ride],
        routing_params: &ResolvedRoutingParams,
    ) -> f64 {
        let penalty = &self.transfer_penalty;
        let transfers = transfer_count(rides);
        if transfers == 0 {
            return 0.0;
        }
        // Transfer penalties are denominated in utils but this router compares seconds. One utility
        // is worth `3600 / (performing - pt)` seconds of travel time, which is the same rate the
        // mode factors below already use, so a config that changes the pt time weight reprices the
        // penalty consistently instead of leaving it stale. At MATSim's pinned defaults this is
        // 3600 / 12 = 300 seconds per utility.
        let seconds_per_utility = 3600.0
            / (routing_params.performing_utility_per_hour - routing_params.pt_utility_per_hour);
        let utils = if penalty.is_mode_specific() {
            // The first ride has no transfer before it, so each following ride contributes the
            // cost of switching from the mode it arrived on.
            rides
                .iter()
                .enumerate()
                .skip(1)
                .filter(|(_, ride)| ride.transfer_before.is_some())
                .map(|(index, ride)| {
                    let previous = &rides[index - 1];
                    penalty.clip(
                        penalty.base_cost()
                            + penalty.mode_penalty(&previous.transport_mode, &ride.transport_mode),
                    )
                })
                .sum::<f64>()
        } else {
            let (Some(first), Some(last)) = (rides.first(), rides.last()) else {
                return 0.0;
            };
            // MATSim prices the hourly term against the time riding actually began:
            // `max(agent arrival at the first stop, vehicle arrival there)`
            // (SwissRailRaptorCore:700). `boarding_time` is the vehicle's *departure* from that
            // stop, which is later by any dwell there and so would under-count the penalty. The
            // agent reached the stop no later than the departure, since a departure the agent would
            // miss is skipped above, which leaves the vehicle's arrival as the origin.
            let origin = first.vehicle_arrival_at_board;
            let travel_seconds = last.alighting_time.saturating_sub(origin).as_secs();
            penalty.clip(
                penalty.base_cost() + penalty.per_travel_time_hour / 3600.0 * travel_seconds as f64,
            ) * transfers as f64
        };
        utils * seconds_per_utility
    }

    fn select_range_query_path(
        &self,
        destination: &Coordinate,
        desired_departure: SimTime,
        access_stops: &[FeederStop],
        egress_stops: &[FeederStop],
        window: &TransitRangeQuerySettings,
        person: Option<&InternalPerson>,
        routing_params: &ResolvedRoutingParams,
        capacity_feedback: &BTreeMap<TransitSegment, TransitSegmentObservation>,
    ) -> Option<TransitPath> {
        let earlier = Duration::from_secs(window.max_earlier_departure_sec);
        let later = Duration::from_secs(window.max_later_departure_sec);
        let earliest = desired_departure.saturating_sub(earlier);
        let latest = desired_departure.saturating_add(later);
        let mut query_times = vec![desired_departure, earliest, latest];
        for feeder in access_stops {
            if let Some(route_refs) = self.routes_by_stop.get(&feeder.stop) {
                for route_ref in route_refs {
                    let route =
                        &self.schedule.get_line(&route_ref.line_id).routes[&route_ref.route_id];
                    if !route.stops[route_ref.stop_index].allow_boarding {
                        continue;
                    }
                    let board_offset = route.stops[route_ref.stop_index]
                        .departure_offset
                        .unwrap_or_default();
                    let departure_indices = &self.departures_by_route
                        [&(route_ref.line_id.clone(), route_ref.route_id.clone())];
                    let first_departure_time = earliest
                        .saturating_add(feeder.time)
                        .saturating_sub(board_offset);
                    let last_departure_time = latest
                        .saturating_add(feeder.time)
                        .saturating_sub(board_offset);
                    let first = departure_indices.partition_point(|index| {
                        route.departures[*index].departure_time < first_departure_time
                    });
                    query_times.extend(
                        departure_indices[first..]
                            .iter()
                            .map(|index| &route.departures[*index])
                            .take_while(|departure| departure.departure_time <= last_departure_time)
                            .map(|departure| {
                                departure
                                    .departure_time
                                    .saturating_add(board_offset)
                                    .saturating_sub(feeder.time)
                            }),
                    );
                }
            }
        }
        query_times.sort_unstable();
        query_times.dedup();
        let selector = matching_transit_settings(
            &self.route_selector_settings,
            person.map_or("", |person| person.subpopulation().external()),
        )
        .map(|settings| settings.clone())
        .unwrap_or_default();
        let mut candidates = Vec::new();
        for departure in query_times {
            let mut paths = self.find_paths_with_feeders(
                destination,
                departure,
                access_stops,
                egress_stops,
                routing_params,
                capacity_feedback,
            );
            for path in &mut paths {
                if let Some(first_ride) = path.rides.first() {
                    path.departure = first_ride.boarding_time.saturating_sub(path.access_time);
                }
            }
            candidates.extend(
                paths
                    .into_iter()
                    .filter(|path| path.departure >= earliest && path.departure <= latest),
            );
        }
        let score = |path: &TransitPath| {
            selector.beta_departure_time
                * (path
                    .departure
                    .as_nanos()
                    .abs_diff(desired_departure.as_nanos()) as f64
                    / 1_000_000_000.0)
                + selector.beta_travel_time
                    * (path.arrival.duration_since(path.departure).as_secs_f64()
                        - chained_dwell_seconds(&path.rides))
                + selector.beta_transfer_count * transfer_count(&path.rides) as f64
        };
        let Some(best_score) = candidates.iter().map(&score).reduce(f64::min) else {
            return None;
        };
        let mut best: Vec<_> = candidates
            .into_iter()
            .filter(|path| score(path) == best_score)
            .collect();
        best.sort_by(|a, b| {
            a.departure
                .cmp(&b.departure)
                .then_with(|| transit_path_tiebreak(a, b))
        });
        best.dedup_by(|a, b| a.departure == b.departure && transit_path_tiebreak(a, b).is_eq());
        let mut stream_id = format!(
            "{}:{}",
            person.map_or("", |p| p.id().external()),
            desired_departure.as_nanos()
        );
        for feeder in access_stops {
            stream_id.push(':');
            stream_id.push_str(feeder.stop.external());
        }
        let mut egress_ids: Vec<_> = egress_stops.iter().map(|feeder| &feeder.stop).collect();
        egress_ids.sort_by_key(|stop| stop.external());
        for stop in egress_ids {
            stream_id.push(':');
            stream_id.push_str(stop.external());
        }
        let mut rng = crate::simulation::random::get_rng(
            self.random_seed,
            "transit.route_selection",
            &stream_id,
        );
        Some(best.swap_remove(rng.random_range(0..best.len())))
    }

    /// Plain access/egress pairs as walk feeders, so the range-query tests can call the routing
    /// entry points without spelling out the feeder struct.
    #[cfg(test)]
    fn walk_feeders(&self, stops: &[(Id<TransitStopFacility>, f64)]) -> Vec<FeederStop> {
        stops
            .iter()
            .map(|(stop, distance)| FeederStop {
                stop: stop.clone(),
                distance: *distance,
                time: self.walk_time(*distance),
                disutility: 0.0,
                route: None,
            })
            .collect()
    }

    /// Walk egress feeders for a destination, ordered by external stop ID so candidate ordering
    /// is reproducible.
    #[cfg(test)]
    fn walk_egress_feeders(
        &self,
        destination: &Coordinate,
        stops: &HashSet<Id<TransitStopFacility>>,
    ) -> Vec<FeederStop> {
        let mut feeders = stops
            .iter()
            .map(|stop| {
                let distance = Coordinate::euclidean_distance(
                    &self.schedule.get_facility(stop).coord,
                    destination,
                );
                FeederStop {
                    stop: stop.clone(),
                    distance,
                    time: self.walk_time(distance),
                    disutility: 0.0,
                    route: None,
                }
            })
            .collect::<Vec<_>>();
        feeders.sort_by(|a, b| a.stop.external().cmp(b.stop.external()));
        feeders
    }

    /// Cheapest path for a plain access/egress request that carries no feeder route. Used by the
    /// skim helper and the tests; real requests go through `find_best_path_with_feeders`.
    #[cfg(test)]
    fn find_best_path(
        &self,
        destination: &Coordinate,
        departure_time: SimTime,
        access_stops: &[(Id<TransitStopFacility>, f64)],
        egress_stops: &HashSet<Id<TransitStopFacility>>,
        routing_params: &ResolvedRoutingParams,
    ) -> Option<TransitPath> {
        let capacity_feedback = self.capacity_feedback.load_full();
        self.find_best_path_with_feedback(
            destination,
            departure_time,
            access_stops,
            egress_stops,
            routing_params,
            &capacity_feedback,
        )
    }

    #[cfg(test)]
    fn find_best_path_with_feedback(
        &self,
        destination: &Coordinate,
        departure_time: SimTime,
        access_stops: &[(Id<TransitStopFacility>, f64)],
        egress_stops: &HashSet<Id<TransitStopFacility>>,
        routing_params: &ResolvedRoutingParams,
        capacity_feedback: &BTreeMap<TransitSegment, TransitSegmentObservation>,
    ) -> Option<TransitPath> {
        let access = access_stops
            .iter()
            .map(|(stop, distance)| FeederStop {
                stop: stop.clone(),
                distance: *distance,
                time: self.walk_time(*distance),
                disutility: 0.0,
                route: None,
            })
            .collect::<Vec<_>>();
        let mut egress = egress_stops
            .iter()
            .map(|stop| {
                let distance = Coordinate::euclidean_distance(
                    &self.schedule.get_facility(stop).coord,
                    destination,
                );
                FeederStop {
                    stop: stop.clone(),
                    distance,
                    time: self.walk_time(distance),
                    disutility: 0.0,
                    route: None,
                }
            })
            .collect::<Vec<_>>();
        egress.sort_by(|a, b| a.stop.external().cmp(b.stop.external()));
        self.find_best_path_with_feeders(
            destination,
            departure_time,
            &access,
            &egress,
            routing_params,
            capacity_feedback,
        )
    }

    /// Applies the configured leg-only policy to the paths the search produced. Leg-only paths are
    /// candidates without any ride, so they are compared separately from real transit itineraries.
    fn find_best_path_with_feeders(
        &self,
        destination: &Coordinate,
        departure_time: SimTime,
        access_stops: &[FeederStop],
        egress_stops: &[FeederStop],
        routing_params: &ResolvedRoutingParams,
        capacity_feedback: &BTreeMap<TransitSegment, TransitSegmentObservation>,
    ) -> Option<TransitPath> {
        // A feasible unconstrained optimum is already optimal under the walking limit.
        // Avoid carrying a third search resource for ordinary trips on large timetables.
        let candidates = self
            .search_paths_for_destinations(
                std::slice::from_ref(destination),
                &[egress_stops.to_vec()],
                departure_time,
                access_stops,
                routing_params,
                capacity_feedback,
                None,
            )
            .into_iter()
            .map(|(_, path)| path)
            .collect::<Vec<_>>();
        let best = self.select_best_path_from_candidates(
            &candidates,
            departure_time,
            routing_params,
            capacity_feedback,
        )?;
        if self.max_walk_duration.is_none() || self.walking_allowed(self.path_walking_time(&best)) {
            return Some(best);
        }
        let candidates = self.find_paths_with_feeders(
            destination,
            departure_time,
            access_stops,
            egress_stops,
            routing_params,
            capacity_feedback,
        );
        self.select_best_path_from_candidates(
            &candidates,
            departure_time,
            routing_params,
            capacity_feedback,
        )
    }

    fn select_best_path_from_candidates(
        &self,
        candidates: &[TransitPath],
        departure_time: SimTime,
        routing_params: &ResolvedRoutingParams,
        capacity_feedback: &BTreeMap<TransitSegment, TransitSegmentObservation>,
    ) -> Option<TransitPath> {
        let cheapest = |paths: &[&TransitPath]| {
            paths
                .iter()
                .copied()
                .min_by(|left, right| {
                    self.path_cost_equivalent_seconds_with_feedback(
                        left,
                        departure_time,
                        routing_params,
                        capacity_feedback,
                    )
                    .total_cmp(&self.path_cost_equivalent_seconds_with_feedback(
                        right,
                        departure_time,
                        routing_params,
                        capacity_feedback,
                    ))
                    .then_with(|| transit_path_tiebreak(left, right))
                })
                .cloned()
        };
        let leg_only: Vec<_> = candidates
            .iter()
            .filter(|candidate| candidate.rides.is_empty())
            .collect();
        let transit: Vec<_> = candidates
            .iter()
            .filter(|candidate| !candidate.rides.is_empty())
            .collect();
        match self.leg_only_handling {
            IntermodalLegOnlyHandling::Forbid => cheapest(&transit),
            IntermodalLegOnlyHandling::Allow => match (cheapest(&transit), cheapest(&leg_only)) {
                (Some(transit), Some(leg_only))
                    if self.path_cost_equivalent_seconds_with_feedback(
                        &leg_only,
                        departure_time,
                        routing_params,
                        capacity_feedback,
                    ) < self.path_cost_equivalent_seconds_with_feedback(
                        &transit,
                        departure_time,
                        routing_params,
                        capacity_feedback,
                    ) =>
                {
                    Some(leg_only)
                }
                (Some(transit), _) => Some(transit),
                (None, leg_only) => leg_only,
            },
            IntermodalLegOnlyHandling::Avoid => match cheapest(&transit) {
                Some(transit) => Some(transit),
                None => cheapest(&leg_only),
            },
        }
    }

    /// Every non-dominated path reachable from the given access and egress feeders. Callers choose
    /// among them: the plain router takes the cheapest, the range-query selector scores them.
    fn find_paths_with_feeders(
        &self,
        destination: &Coordinate,
        departure_time: SimTime,
        access_stops: &[FeederStop],
        egress_stops: &[FeederStop],
        routing_params: &ResolvedRoutingParams,
        capacity_feedback: &BTreeMap<TransitSegment, TransitSegmentObservation>,
    ) -> Vec<TransitPath> {
        self.find_paths_for_destinations(
            std::slice::from_ref(destination),
            &[egress_stops.to_vec()],
            departure_time,
            access_stops,
            routing_params,
            capacity_feedback,
        )
        .into_iter()
        .map(|(_, path)| path)
        .collect()
    }

    fn find_paths_for_destinations(
        &self,
        destinations: &[Coordinate],
        egress_stops_by_destination: &[Vec<FeederStop>],
        departure_time: SimTime,
        access_stops: &[FeederStop],
        routing_params: &ResolvedRoutingParams,
        capacity_feedback: &BTreeMap<TransitSegment, TransitSegmentObservation>,
    ) -> Vec<(usize, TransitPath)> {
        self.search_paths_for_destinations(
            destinations,
            egress_stops_by_destination,
            departure_time,
            access_stops,
            routing_params,
            capacity_feedback,
            self.max_walk_duration,
        )
    }

    fn search_paths_for_destinations(
        &self,
        destinations: &[Coordinate],
        egress_stops_by_destination: &[Vec<FeederStop>],
        departure_time: SimTime,
        access_stops: &[FeederStop],
        routing_params: &ResolvedRoutingParams,
        capacity_feedback: &BTreeMap<TransitSegment, TransitSegmentObservation>,
        walking_budget: Option<Duration>,
    ) -> Vec<(usize, TransitPath)> {
        if self.leg_only_handling == IntermodalLegOnlyHandling::Forbid
            && !self.may_reach_any_egress(access_stops, egress_stops_by_destination)
        {
            return Vec::new();
        }
        debug_assert_eq!(destinations.len(), egress_stops_by_destination.len());

        let mut egress_by_stop: HashMap<Id<TransitStopFacility>, Vec<(usize, usize)>> =
            HashMap::new();
        for (destination_index, egress_stops) in egress_stops_by_destination.iter().enumerate() {
            for (egress_index, egress) in egress_stops.iter().enumerate() {
                egress_by_stop
                    .entry(egress.stop.clone())
                    .or_default()
                    .push((destination_index, egress_index));
            }
        }
        let targets_with_egress = egress_stops_by_destination
            .iter()
            .enumerate()
            .filter_map(|(index, egress_stops)| (!egress_stops.is_empty()).then_some(index))
            .collect::<Vec<_>>();
        let mut states: HashMap<(Id<TransitStopFacility>, usize), Vec<u64>> = HashMap::new();
        let mut state_arena = Vec::<TransitPathState>::new();
        let mut rounds = vec![Vec::<u64>::new(); RAPTOR_MAX_TRANSFERS + 2];
        let mut next_state_id = 0;
        for access in access_stops {
            let walking_time = if walking_budget.is_some() {
                self.feeder_walking_time(access)
            } else {
                Duration::ZERO
            };
            if walking_budget.is_some_and(|maximum| walking_time > maximum) {
                continue;
            }
            let arrival = departure_time.saturating_add(access.time);
            let state = TransitPathState {
                state_id: next_state_id,
                stop: access.stop.clone(),
                parent_state_id: None,
                ride: None,
                ride_count: 0,
                arrival,
                cost: self.cost_equivalent_seconds_with_feedback(
                    arrival,
                    &[],
                    departure_time,
                    routing_params,
                    capacity_feedback,
                ),
                transfer_penalty_seconds: 0.0,
                transfer_count: 0,
                first_vehicle_arrival_at_board: None,
                access_distance: access.distance,
                access_route: access.route.clone(),
                access_time: access.time,
                walking_time,
            };
            let labels = states.entry((access.stop.clone(), 0)).or_default();
            let keep = if walking_budget.is_some() {
                !labels.iter().any(|old| {
                    let old = &state_arena[*old as usize];
                    old.arrival <= state.arrival
                        && old.cost <= state.cost
                        && old.walking_time <= state.walking_time
                })
            } else {
                labels
                    .iter()
                    .all(|old| state.arrival < state_arena[*old as usize].arrival)
            };
            if keep {
                if walking_budget.is_some() {
                    labels.retain(|old| {
                        let old = &state_arena[*old as usize];
                        !(state.arrival <= old.arrival
                            && state.cost <= old.cost
                            && state.walking_time <= old.walking_time)
                    });
                } else {
                    labels.clear();
                }
                rounds[0].push(next_state_id);
                state_arena.push(state);
                labels.push(next_state_id);
                next_state_id += 1;
            }
        }

        let mut candidates = Vec::new();
        let mut best_destination_cost = vec![f64::INFINITY; destinations.len()];
        let mut first_destination_round = vec![None; destinations.len()];
        let monotone_bound =
            walking_budget.is_none() && self.supports_monotone_objective(capacity_feedback);
        for rides_used in 0..rounds.len() {
            let mut current_round_states = std::mem::take(&mut rounds[rides_used]);
            while !current_round_states.is_empty() {
                let mut marked = Vec::new();
                for state_id in current_round_states.drain(..) {
                    let Some(current) = state_arena.get(state_id as usize) else {
                        continue;
                    };
                    if !states
                        .get(&(current.stop.clone(), rides_used))
                        .is_some_and(|labels| labels.contains(&state_id))
                    {
                        continue;
                    }
                    let stop_id = current.stop.clone();
                    if (current.ride_count > 0
                        || self.leg_only_handling != IntermodalLegOnlyHandling::Forbid)
                        && let Some(egresses) = egress_by_stop.get(&stop_id)
                    {
                        let facility = self.schedule.get_facility(&stop_id);
                        for (destination_index, egress_index) in egresses {
                            if monotone_bound && current.ride_count > 0 {
                                let first_round = first_destination_round[*destination_index]
                                    .get_or_insert(rides_used);
                                if rides_used > *first_round + 2 {
                                    continue;
                                }
                            }
                            let egress =
                                &egress_stops_by_destination[*destination_index][*egress_index];
                            if walking_budget.is_some_and(|maximum| {
                                current
                                    .walking_time
                                    .saturating_add(self.feeder_walking_time(egress))
                                    > maximum
                            }) {
                                continue;
                            }
                            let egress_distance = Coordinate::euclidean_distance(
                                &facility.coord,
                                &destinations[*destination_index],
                            );
                            let full_cost = current.cost + egress.time.as_secs_f64();
                            if monotone_bound && current.ride_count > 0 {
                                best_destination_cost[*destination_index] =
                                    best_destination_cost[*destination_index].min(full_cost);
                            }
                            candidates.push((
                                *destination_index,
                                TransitPath {
                                    stop: stop_id.clone(),
                                    departure: departure_time,
                                    arrival: current.arrival.saturating_add(egress.time),
                                    access_distance: current.access_distance,
                                    egress_distance,
                                    rides: reconstruct_transit_rides(
                                        current.state_id,
                                        &state_arena,
                                    ),
                                    access_route: current.access_route.clone(),
                                    egress_route: egress.route.clone(),
                                    access_time: current.access_time,
                                },
                            ));
                        }
                    }

                    let bounds_exhausted = monotone_bound
                        && targets_with_egress.iter().all(|index| {
                            best_destination_cost[*index].is_finite()
                                && (current.cost > best_destination_cost[*index]
                                    || first_destination_round[*index]
                                        .is_some_and(|first| rides_used >= first + 2))
                        });
                    if bounds_exhausted || rides_used >= RAPTOR_MAX_TRANSFERS + 1 {
                        continue;
                    }

                    let mut boarding_stops = vec![(stop_id.clone(), 0.0)];
                    if current.ride_count > 0 {
                        boarding_stops.extend(self.nearby_transfer_stops(&stop_id));
                    }
                    for (boarding_stop, transfer_distance) in boarding_stops {
                        let Some(route_refs) = self.routes_by_stop.get(&boarding_stop) else {
                            continue;
                        };
                        let transfer_time = if current.ride_count > 0 {
                            self.transfer_time(&stop_id, &boarding_stop, transfer_distance)
                        } else {
                            Duration::ZERO
                        };
                        for route_ref in route_refs {
                            let route = self
                                .schedule
                                .get_line(&route_ref.line_id)
                                .routes
                                .get(&route_ref.route_id)
                                .expect("route index references a scheduled route");
                            let chained_targets = chained_departures_from(
                                &self.schedule,
                                current.ride.as_ref(),
                                &boarding_stop,
                            );
                            let can_continue_route = chained_targets.is_some_and(|targets| {
                                targets.iter().any(|target| {
                                    target.transit_line_id == route_ref.line_id
                                        && target.transit_route_id == route_ref.route_id
                                })
                            });
                            if (!route.stops[route_ref.stop_index].allow_boarding
                                && !can_continue_route)
                                || route_ref.stop_index + 1 >= route.stops.len()
                            {
                                continue;
                            }
                            marked.push(BoardingCandidate {
                                line_id: route_ref.line_id.clone(),
                                route_id: route_ref.route_id.clone(),
                                stop_index: route_ref.stop_index,
                                state_id,
                                transfer_distance,
                                transfer_time,
                            });
                        }
                    }
                }

                if marked.is_empty() {
                    break;
                }

                marked.sort_by(|left, right| {
                    (
                        left.line_id.external(),
                        left.route_id.external(),
                        left.stop_index,
                        left.state_id,
                    )
                        .cmp(&(
                            right.line_id.external(),
                            right.route_id.external(),
                            right.stop_index,
                            right.state_id,
                        ))
                });
                let mut marked_start = 0;
                while marked_start < marked.len() {
                    let line_id = marked[marked_start].line_id.clone();
                    let route_id = marked[marked_start].route_id.clone();
                    let mut marked_end = marked_start + 1;
                    while marked_end < marked.len()
                        && marked[marked_end].line_id == line_id
                        && marked[marked_end].route_id == route_id
                    {
                        marked_end += 1;
                    }
                    let line = self.schedule.get_line(&line_id);
                    let route = &line.routes[&route_id];
                    let departure_indices =
                        &self.departures_by_route[&(line_id.clone(), route_id.clone())];
                    let distance_prefix =
                        &self.distance_prefix_by_route[&(line_id.clone(), route_id.clone())];
                    if !departure_indices.is_empty() {
                        let mut candidate_index = marked_start;
                        let mut active = Vec::<ActiveBoarding>::new();
                        for stop_index in 0..route.stops.len() {
                            if stop_index > 0 {
                                let alight_stop = &route.stops[stop_index];
                                for candidate in active.iter().copied() {
                                    if alight_stop.allow_alighting {
                                        let current = &state_arena[candidate.state_id as usize];
                                        let departure =
                                            &route.departures[candidate.departure_index];
                                        let board_stop = &route.stops[candidate.board_index];
                                        let board_offset =
                                            board_stop.departure_offset.unwrap_or_default();
                                        let board_arrival_offset = board_stop
                                            .arrival_offset
                                            .or(board_stop.departure_offset)
                                            .unwrap_or_default();
                                        let arrival_offset = alight_stop
                                            .arrival_offset
                                            .or(alight_stop.departure_offset)
                                            .unwrap_or_default();
                                        let boarding_time =
                                            departure.departure_time.saturating_add(board_offset);
                                        let stop_arrival =
                                            departure.departure_time.saturating_add(arrival_offset);
                                        if stop_arrival >= boarding_time {
                                            let ride = Ride {
                                                line: line.id.clone(),
                                                route: route.id.clone(),
                                                departure: departure.id.clone(),
                                                board_index: candidate.board_index,
                                                alight_index: stop_index,
                                                board: board_stop.facility_id.clone(),
                                                alight: alight_stop.facility_id.clone(),
                                                boarding_time,
                                                vehicle_arrival_at_board: departure
                                                    .departure_time
                                                    .saturating_add(board_arrival_offset),
                                                alighting_time: stop_arrival,
                                                distance: distance_prefix[stop_index]
                                                    - distance_prefix[candidate.board_index],
                                                transport_mode: route
                                                    .transport_mode
                                                    .external()
                                                    .to_owned(),
                                                passenger_mode: if self.use_passenger_mode_mapping {
                                                    self.passenger_modes
                                                        .get(route.transport_mode.external())
                                                        .cloned()
                                                        .unwrap_or_else(|| {
                                                            self.mode.external().to_owned()
                                                        })
                                                } else {
                                                    self.mode.external().to_owned()
                                                },
                                                transfer_before: (!candidate.chained_continuation)
                                                    .then(|| {
                                                        (current.ride_count > 0).then_some((
                                                            candidate.transfer_distance,
                                                            (candidate.transfer_distance
                                                                * self.walk_distance_factor)
                                                                .ceil(),
                                                            candidate.transfer_time,
                                                        ))
                                                    })
                                                    .flatten(),
                                                chained_from_previous: candidate
                                                    .chained_continuation,
                                            };
                                            let (cost, transfer_penalty_seconds, transfer_count) =
                                                self.cost_after_ride(
                                                    current,
                                                    &ride,
                                                    stop_arrival,
                                                    routing_params,
                                                    capacity_feedback,
                                                );
                                            let walking_time = self.walking_after_boarding(
                                                current,
                                                candidate.transfer_time,
                                                candidate.chained_continuation,
                                                walking_budget,
                                            );
                                            if walking_budget
                                                .is_some_and(|maximum| walking_time > maximum)
                                            {
                                                continue;
                                            }
                                            #[cfg(test)]
                                            {
                                                let mut rides = reconstruct_transit_rides(
                                                    current.state_id,
                                                    &state_arena,
                                                );
                                                rides.push(ride.clone());
                                                let expected = self
                                                    .cost_equivalent_seconds_with_feedback(
                                                        stop_arrival,
                                                        &rides,
                                                        departure_time,
                                                        routing_params,
                                                        capacity_feedback,
                                                    );
                                                assert!(
                                                    (cost - expected).abs() < 1e-8,
                                                    "incremental transit cost {cost} differs from full cost {expected}"
                                                );
                                            }
                                            let next_stop = alight_stop.facility_id.clone();
                                            let next_rides_used = rides_used
                                                + usize::from(
                                                    current.ride_count == 0
                                                        || !candidate.chained_continuation,
                                                );
                                            if monotone_bound
                                                && (1..next_rides_used).any(|previous_round| {
                                                    states
                                                        .get(&(next_stop.clone(), previous_round))
                                                        .is_some_and(|labels| {
                                                            labels.iter().any(|old| {
                                                                let old =
                                                                    &state_arena[*old as usize];
                                                                old.arrival <= stop_arrival
                                                                    && old.cost
                                                                        + stop_arrival
                                                                            .duration_since(
                                                                                old.arrival,
                                                                            )
                                                                            .as_secs_f64()
                                                                        <= cost
                                                            })
                                                        })
                                                })
                                            {
                                                continue;
                                            }
                                            let labels = states
                                                .entry((next_stop.clone(), next_rides_used))
                                                .or_default();
                                            if !labels.iter().any(|old| {
                                                let old = &state_arena[*old as usize];
                                                old.arrival <= stop_arrival
                                                    && old.cost <= cost
                                                    && old.walking_time <= walking_time
                                            }) {
                                                labels.retain(|old| {
                                                    let old = &state_arena[*old as usize];
                                                    !(stop_arrival <= old.arrival
                                                        && cost <= old.cost
                                                        && walking_time <= old.walking_time)
                                                });
                                                let first_vehicle_arrival_at_board = current
                                                    .first_vehicle_arrival_at_board
                                                    .or(Some(ride.vehicle_arrival_at_board));
                                                let new_state_id = next_state_id;
                                                state_arena.push(TransitPathState {
                                                    state_id: new_state_id,
                                                    stop: next_stop.clone(),
                                                    parent_state_id: Some(current.state_id),
                                                    ride: Some(ride),
                                                    ride_count: current.ride_count + 1,
                                                    arrival: stop_arrival,
                                                    cost,
                                                    transfer_penalty_seconds,
                                                    transfer_count,
                                                    first_vehicle_arrival_at_board,
                                                    access_distance: current.access_distance,
                                                    access_route: current.access_route.clone(),
                                                    access_time: current.access_time,
                                                    walking_time,
                                                });
                                                labels.push(new_state_id);
                                                next_state_id += 1;
                                                if next_rides_used == rides_used {
                                                    current_round_states.push(new_state_id);
                                                } else if next_rides_used < rounds.len() {
                                                    rounds[next_rides_used].push(new_state_id);
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            while candidate_index < marked_end
                                && marked[candidate_index].stop_index == stop_index
                            {
                                let candidate = &marked[candidate_index];
                                let current = &state_arena[candidate.state_id as usize];
                                let board_stop = &route.stops[stop_index];
                                let board_offset = board_stop.departure_offset.unwrap_or_default();
                                let chained_targets = chained_departures_from(
                                    &self.schedule,
                                    current.ride.as_ref(),
                                    &board_stop.facility_id,
                                );
                                let can_continue_route = chained_targets.is_some_and(|targets| {
                                    targets.iter().any(|target| {
                                        target.transit_line_id == line_id
                                            && target.transit_route_id == route_id
                                    })
                                });
                                if !board_stop.allow_boarding && !can_continue_route {
                                    candidate_index += 1;
                                    continue;
                                }
                                let can_skip_dominated_departures = capacity_feedback.is_empty()
                                    && route
                                        .departures
                                        .iter()
                                        .all(|departure| departure.chained_departures.is_empty())
                                    && (current.ride_count == 0
                                        || self.transfer_penalty.per_travel_time_hour >= 0.0);
                                let first_boardable_time =
                                    current.arrival.saturating_add(if can_continue_route {
                                        Duration::ZERO
                                    } else {
                                        candidate.transfer_time
                                    });
                                let first_departure = departure_indices.partition_point(|index| {
                                    route.departures[*index]
                                        .departure_time
                                        .saturating_add(board_offset)
                                        < first_boardable_time
                                });
                                let mut earliest_selected_time = None;
                                for &dep_idx in &departure_indices[first_departure..] {
                                    let departure = &route.departures[dep_idx];
                                    let chained_continuation =
                                        chained_targets.is_some_and(|targets| {
                                            targets.iter().any(|target| {
                                                target.transit_line_id == line_id
                                                    && target.transit_route_id == route_id
                                                    && target.departure_id == departure.id
                                            })
                                        });
                                    if !board_stop.allow_boarding && !chained_continuation {
                                        continue;
                                    }
                                    let connection_time = if chained_continuation {
                                        Duration::ZERO
                                    } else {
                                        candidate.transfer_time
                                    };
                                    let boarding_time =
                                        departure.departure_time.saturating_add(board_offset);
                                    if boarding_time
                                        < current.arrival.saturating_add(connection_time)
                                    {
                                        continue;
                                    }
                                    if can_skip_dominated_departures {
                                        match earliest_selected_time {
                                            Some(time) if boarding_time > time => break,
                                            Some(time) if boarding_time < time => {
                                                earliest_selected_time = Some(boarding_time);
                                            }
                                            None => earliest_selected_time = Some(boarding_time),
                                            _ => {}
                                        }
                                    }
                                    let walking_time = self.walking_after_boarding(
                                        current,
                                        candidate.transfer_time,
                                        chained_continuation,
                                        walking_budget,
                                    );
                                    let dominated = can_skip_dominated_departures
                                        && active.iter().any(|existing| {
                                            existing.base_departure <= departure.departure_time
                                                && route.stops[existing.board_index]
                                                    .departure_offset
                                                    .unwrap_or_default()
                                                    <= board_offset
                                                && self.walking_after_boarding(
                                                    &state_arena[existing.state_id as usize],
                                                    existing.transfer_time,
                                                    existing.chained_continuation,
                                                    walking_budget,
                                                ) <= walking_time
                                        });
                                    if !dominated {
                                        active.push(ActiveBoarding {
                                            state_id: candidate.state_id,
                                            board_index: stop_index,
                                            departure_index: dep_idx,
                                            transfer_distance: candidate.transfer_distance,
                                            transfer_time: candidate.transfer_time,
                                            chained_continuation,
                                            base_departure: departure.departure_time,
                                        });
                                    }
                                }
                                candidate_index += 1;
                            }
                        }
                    }
                    marked_start = marked_end;
                }
            }
        }
        candidates
    }

    fn may_reach_any_egress(
        &self,
        access_stops: &[FeederStop],
        egress_stops_by_destination: &[Vec<FeederStop>],
    ) -> bool {
        if self.has_chained_departures {
            return true;
        }
        let targets = egress_stops_by_destination
            .iter()
            .flatten()
            .map(|stop| stop.stop.clone())
            .collect::<HashSet<_>>();
        if targets.is_empty() || access_stops.is_empty() {
            return false;
        }

        let mut reachable = HashSet::new();
        let mut pending = VecDeque::new();
        for access in access_stops {
            if reachable.insert(access.stop.clone()) {
                pending.push_back(access.stop.clone());
            }
        }
        while let Some(stop_id) = pending.pop_front() {
            if targets.contains(&stop_id) {
                return true;
            }
            if self.routes_by_stop.contains_key(&stop_id) {
                for (transfer_stop, _) in self.nearby_transfer_stops(&stop_id) {
                    if reachable.insert(transfer_stop.clone()) {
                        pending.push_back(transfer_stop);
                    }
                }
            }
            let Some(route_refs) = self.routes_by_stop.get(&stop_id) else {
                continue;
            };
            for route_ref in route_refs {
                let line = self.schedule.get_line(&route_ref.line_id);
                let route = &line.routes[&route_ref.route_id];
                if route.departures.is_empty() || !route.stops[route_ref.stop_index].allow_boarding
                {
                    continue;
                }
                for alight in route
                    .stops
                    .iter()
                    .skip(route_ref.stop_index + 1)
                    .filter(|stop| stop.allow_alighting)
                {
                    if reachable.insert(alight.facility_id.clone()) {
                        pending.push_back(alight.facility_id.clone());
                    }
                }
            }
        }
        false
    }
}
fn chained_departures_from<'a>(
    schedule: &'a TransitSchedule,
    previous: Option<&Ride>,
    boarding_stop: &Id<TransitStopFacility>,
) -> Option<&'a [ChainedDeparture]> {
    let Some(previous) = previous else {
        return None;
    };
    if &previous.alight != boarding_stop {
        return None;
    }
    schedule
        .get_line(&previous.line)
        .routes
        .get(&previous.route)
        .and_then(|route| {
            route
                .departures
                .iter()
                .find(|candidate| candidate.id == previous.departure)
        })
        .map(|departure| departure.chained_departures.as_slice())
}

fn collapse_chained_rides(rides: &[Ride]) -> Vec<Ride> {
    let mut collapsed: Vec<Ride> = Vec::with_capacity(rides.len());
    for ride in rides {
        if ride.chained_from_previous
            && let Some(previous) = collapsed.last_mut()
        {
            previous.alight = ride.alight.clone();
            previous.alight_index = ride.alight_index;
            previous.alighting_time = ride.alighting_time;
            previous.distance += ride.distance;
        } else {
            collapsed.push(ride.clone());
        }
    }
    collapsed
}

fn transfer_count(rides: &[Ride]) -> usize {
    rides
        .iter()
        .skip(1)
        .filter(|ride| !ride.chained_from_previous)
        .count()
}

fn chained_dwell_seconds(rides: &[Ride]) -> f64 {
    rides
        .windows(2)
        .filter(|pair| pair[1].chained_from_previous)
        .map(|pair| {
            pair[1]
                .boarding_time
                .duration_since(pair[0].alighting_time)
                .as_secs_f64()
        })
        .sum()
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

fn plan_travel_time(elements: &[InternalPlanElement]) -> Duration {
    elements
        .iter()
        .map(|element| match element {
            InternalPlanElement::Leg(leg) => leg.trav_time.unwrap_or_default(),
            InternalPlanElement::Activity(activity) => activity.max_dur.unwrap_or_default(),
        })
        .sum()
}

pub(crate) fn walking_travel_time(
    elements: &[InternalPlanElement],
) -> Result<Duration, RoutingError> {
    elements.iter().try_fold(Duration::ZERO, |total, element| {
        let Some(leg) = element.as_leg().filter(|leg| leg.mode.external() == "walk") else {
            return Ok(total);
        };
        let duration = leg
            .trav_time
            .or_else(|| {
                leg.route
                    .as_ref()
                    .and_then(|route| route.as_generic().trav_time())
            })
            .ok_or_else(|| RoutingError::MissingEndTime {
                mode: "walk".to_owned(),
            })?;
        Ok(total.saturating_add(duration))
    })
}

fn route_disutility(elements: &[InternalPlanElement], utilities: &BTreeMap<String, f64>) -> f64 {
    elements
        .iter()
        .filter_map(|element| match element {
            InternalPlanElement::Leg(leg) => Some(
                leg.trav_time.unwrap_or_default().as_secs_f64()
                    * -utilities.get(leg.mode.external()).copied().unwrap_or(-6.0)
                    / 3_600.0,
            ),
            InternalPlanElement::Activity(_) => None,
        })
        .sum()
}

fn transit_stop_time(stop: &TransitStopFacility, key: &str) -> Result<f64, RoutingError> {
    let Some((_, raw)) = stop
        .attributes
        .iter()
        .find(|(name, _)| name.as_str() == key)
    else {
        return Ok(0.0);
    };
    let value = raw
        .as_f64()
        .or_else(|| raw.as_str().and_then(|s| s.parse().ok()))
        .filter(|value: &f64| value.is_finite() && *value >= 0.0)
        .ok_or_else(|| RoutingError::MalformedTransitStopAttribute {
            stop: stop.id.external().to_owned(),
            key: key.to_owned(),
            reason: "expected finite non-negative seconds".to_owned(),
        })?;
    Ok(value)
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
        Facility, FeederStop, OWNS_CAR, Ride, RouteFrequencyProposalBackend, RouteProposal,
        RouteProposalKey, RouteProposalSeed, RouteProposalTable, RoutingError, RoutingModule,
        RoutingRequest, RoutingRequestBuilder, TransitRoutingModule, TransitSegment,
        TransitSegmentObservation, TransitSkimOutcome, TripRouter, matching_transit_settings,
        transit_path_tiebreak,
    };
    use crate::simulation::InternalAttributes;
    use crate::simulation::config::Config;
    use crate::simulation::config::{
        IntermodalAccessEgress, IntermodalLegOnlyHandling, IntermodalModeSelection,
        TransferConstruction, TransitRangeQuerySettings, TransitRouteSelectorSettings,
        TransitTransferPenalty,
    };
    use crate::simulation::id::Id;
    use crate::simulation::replanning::routing::teleportation::TeleportationRoutingModule;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::Scenario;
    use crate::simulation::scenario::network::Link;
    use crate::simulation::scenario::population::{
        InternalActivity, InternalGenericRoute, InternalLeg, InternalPerson, InternalPlan,
        InternalPlanElement, InternalRoute, Population,
    };
    use crate::simulation::scenario::transit::{
        TransitDeparture, TransitLine, TransitRoute, TransitRouteStop, TransitSchedule,
    };
    use crate::simulation::scenario::vehicles::{Garage, InternalVehicle};
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use nohash_hasher::IntMap;
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::collections::HashSet;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn reference_schedule() -> TransitSchedule {
        TransitSchedule::from_file(
            "./tests/resources/pt_reference/routing_direct_vs_transfer/transit_schedule.xml"
                .as_ref(),
        )
    }

    fn reference_trip_endpoints() -> (Facility<'static>, Facility<'static>) {
        (
            Facility::new_link_wrapper(Coordinate::new_2d(1050.0, 1050.0), Id::create("11")),
            Facility::new_link_wrapper(Coordinate::new_2d(3950.0, 1050.0), Id::create("33")),
        )
    }

    fn separate_platform_schedule(platform_x: u32) -> TransitSchedule {
        let fixture = std::fs::read_to_string(
            "./tests/resources/pt_reference/routing_direct_vs_transfer/transit_schedule.xml",
        )
        .unwrap();
        let platform = format!(
            "<stopFacility id=\"rb_platform\" x=\"{platform_x}\" y=\"2940\" linkRefId=\"23\" />"
        );
        let fixture = fixture
            .replace(
                "<stopFacility id=\"rb\" x=\"2050\" y=\"2940\" linkRefId=\"12\" />",
                &format!(
                    "<stopFacility id=\"rb\" x=\"2050\" y=\"2940\" linkRefId=\"12\" />\n\t\t{platform}"
                ),
            )
            .replace("refId=\"rb\" departureOffset", "refId=\"rb_platform\" departureOffset");
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("schedule.xml");
        std::fs::write(&path, fixture).unwrap();
        TransitSchedule::from_file(&path)
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

    fn transfer_construction_modes() -> [TransferConstruction; 3] {
        [
            TransferConstruction::Initial,
            TransferConstruction::Adaptive,
            TransferConstruction::Online,
        ]
    }

    fn reference_router_with_construction(
        schedule: TransitSchedule,
        walk_speed: f64,
        construction: TransferConstruction,
    ) -> TransitRoutingModule {
        TransitRoutingModule::new_with_transfer_construction(
            Arc::new(schedule),
            walk_speed,
            1.3,
            Arc::new(Garage::default()),
            None,
            construction,
        )
    }

    #[deterministic_id_test]
    fn one_to_all_skim_matches_passenger_cost_and_finds_the_nearest_stop() {
        let router = reference_router(reference_schedule(), 0.8333333333333334);
        let origin = Coordinate::new_2d(1050.0, 1050.0);
        let destinations = [
            Coordinate::new_2d(3950.0, 1050.0),
            Coordinate::new_2d(50_000.0, 50_000.0),
        ];

        let results =
            router.skim_results_from_origin(&origin, &destinations, SimTime::from_secs(8 * 3600));

        assert_eq!(results[0].outcome, TransitSkimOutcome::Transit);
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/resources/pt_reference/java/routing_direct_vs_transfer.json"
        ))
        .unwrap();
        let reference_travel_time = reference["itineraries"][0]["arrival_time"]
            .as_f64()
            .unwrap()
            - (8 * 3600) as f64;
        assert_eq!(
            results[0].travel_time,
            Some(Duration::from_secs_f64(reference_travel_time))
        );
        assert_eq!(results[1].outcome, TransitSkimOutcome::Transit);
        assert!(results[1].travel_time.is_some());
        for (destination, expected) in destinations.iter().zip(results.iter().copied()) {
            assert_eq!(
                router.skim_results_from_origin(
                    &origin,
                    std::slice::from_ref(destination),
                    SimTime::from_secs(8 * 3600),
                ),
                [expected],
            );
        }
        assert_eq!(
            router.skim_times_from_origin(
                &origin,
                &destinations[1..],
                SimTime::from_secs(8 * 3600)
            ),
            [results[1].travel_time.unwrap()]
        );
    }

    #[deterministic_id_test]
    fn search_skips_a_destination_outside_transit_connectivity() {
        let router = reference_router(reference_schedule(), 0.8333333333333334);
        let destination = Coordinate::new_2d(1051.0, 1050.0);
        let access = [FeederStop {
            stop: Id::create("ra"),
            distance: 0.0,
            time: Duration::ZERO,
            disutility: 0.0,
            route: None,
        }];
        let egress = vec![vec![FeederStop {
            stop: Id::create("rd"),
            distance: 0.0,
            time: Duration::ZERO,
            disutility: 0.0,
            route: None,
        }]];
        let paths = router.find_paths_for_destinations(
            &[destination],
            &egress,
            SimTime::from_secs(8 * 3600),
            &access,
            &router.resolve_routing_params(""),
            &BTreeMap::new(),
        );

        assert!(paths.is_empty());
    }

    #[deterministic_id_test]
    #[ignore = "requires local Bangkok inputs via MATSIM_RUST_BKK_CONFIG"]
    fn raw_bangkok_pt_journey_stays_within_one_second() {
        let config_path = std::env::var("MATSIM_RUST_BKK_CONFIG")
            .expect("set MATSIM_RUST_BKK_CONFIG to the local raw 0.1% config");
        let config = Arc::new(Config::from_path(config_path));
        let scenario = Scenario::load(config.clone());
        let person_id =
            std::env::var("MATSIM_RUST_BKK_PERSON").unwrap_or_else(|_| "11460713".to_owned());
        let person = scenario
            .population
            .persons
            .values()
            .find(|person| person.id().external() == person_id)
            .unwrap_or_else(|| panic!("raw Bangkok sample does not contain person {person_id}"))
            .clone();
        let plan = person.selected_plan().unwrap();
        let (from, leg, to) = plan
            .elements
            .windows(3)
            .find_map(|elements| match elements {
                [
                    InternalPlanElement::Activity(from),
                    InternalPlanElement::Leg(leg),
                    InternalPlanElement::Activity(to),
                ] if leg.mode.external() == "pt" => Some((from, leg, to)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("person {person_id} has no raw PT journey"));
        let departure = leg
            .dep_time
            .or(from.end_time)
            .expect("raw PT journey has a departure time");
        let walk = config
            .routing()
            .teleported_mode_params
            .iter()
            .find(|params| params.mode == "walk")
            .expect("config has a walk router");
        assert!(
            !config.transit().use_intermodal_access_egress,
            "this local performance fixture expects ordinary walk access and egress"
        );
        let module = TransitRoutingModule::new_with_transfer_construction(
            Arc::new(scenario.transit_schedule),
            walk.teleported_mode_speed,
            walk.beeline_distance_factor,
            Arc::new(scenario.garage),
            None,
            config.transit().transfer_construction,
        )
        .with_passenger_mode_mapping(
            config.transit().use_mode_mapping_for_passengers,
            config.transit().mode_mapping_for_passengers.clone(),
            &config.scoring().mode_params,
            &config.scoring().agent_params,
        )
        .with_range_queries(
            config.transit().range_query_settings.clone(),
            config.transit().route_selector_settings.clone(),
            config.computational_setup().random_seed,
        )
        .with_transfer_penalty(config.transit().transfer_penalty.clone());
        let mut modules: IntMap<Id<String>, Arc<dyn RoutingModule>> = IntMap::default();
        modules.insert(Id::create("pt"), Arc::new(module));
        let trip_router = TripRouter::new(modules);
        let from = Facility::new_link_wrapper(from.coord().clone(), from.link_id().clone());
        let to = Facility::new_link_wrapper(to.coord().clone(), to.link_id().clone());
        let route_once = || {
            trip_router
                .calc_route(
                    &Id::create("pt"),
                    RoutingRequestBuilder::default()
                        .from(&from)
                        .to(&to)
                        .departure_time(departure)
                        .person(Some(&person))
                        .build()
                        .unwrap(),
                )
                .unwrap()
        };

        let itinerary = route_once();
        assert!(itinerary.iter().any(|element| matches!(
            element,
            InternalPlanElement::Leg(leg) if leg.trav_time.is_some()
        )));
        if person_id == "11460713" {
            assert!(itinerary.iter().any(|element| matches!(
                element,
                InternalPlanElement::Leg(leg) if leg.mode.external() == "pt"
            )));
        }
        let pt_segments = itinerary
            .iter()
            .filter_map(|element| {
                let InternalPlanElement::Leg(leg) = element else {
                    return None;
                };
                let Some(InternalRoute::Pt(route)) = leg.route.as_ref() else {
                    return None;
                };
                let description = &route.description;
                Some((
                    description.transit_line_id.as_str(),
                    description.transit_route_id.as_str(),
                    description.access_facility_id.as_str(),
                    description.egress_facility_id.as_str(),
                    description.boarding_time,
                ))
            })
            .collect::<Vec<_>>();
        println!("raw person {person_id} PT segments: {pt_segments:?}");
        let times = (0..5)
            .map(|_| {
                let started = Instant::now();
                let result = route_once();
                let elapsed = started.elapsed().as_secs_f64();
                assert_eq!(result, itinerary);
                elapsed
            })
            .collect::<Vec<_>>();
        let worst = times.iter().copied().fold(0.0, f64::max);
        println!("raw PT TripRouter timings: {times:?}; worst={worst:.6}s");
        assert!(worst <= 1.0, "raw PT journey took {worst:.6}s (limit 1s)");
    }

    #[deterministic_id_test]
    fn route_scan_keeps_an_earlier_service_boarded_further_downstream() {
        let mut schedule = reference_schedule();
        let line = schedule
            .lines_mut()
            .get_mut(&Id::<TransitLine>::create("Reference Line"))
            .unwrap();
        line.routes
            .get_mut(&Id::<TransitRoute>::create("a_to_b"))
            .unwrap()
            .departures
            .clear();
        let mut middle = line.routes[&Id::<TransitRoute>::create("b_to_c")].stops[0].clone();
        middle.arrival_offset = Some(Duration::from_secs(600));
        middle.departure_offset = Some(Duration::from_secs(600));
        line.routes
            .get_mut(&Id::<TransitRoute>::create("b_to_c"))
            .unwrap()
            .departures
            .clear();
        let route = line
            .routes
            .get_mut(&Id::<TransitRoute>::create("direct"))
            .unwrap();
        route.stops[1].arrival_offset = Some(Duration::from_secs(1200));
        route.stops.insert(1, middle);
        route
            .departures
            .retain(|departure| ["d_0800", "d_0830"].contains(&departure.id.external()));
        let router = reference_router(schedule, 1.0);
        for (access_a, access_b, service, board, arrival) in [
            (900.0, 600.0, "d_0800", "rb", 8 * 3600 + 1200),
            (900.0, 2700.0, "d_0830", "ra", 8 * 3600 + 3000),
            (0.0, 600.0, "d_0800", "ra", 8 * 3600 + 1200),
        ] {
            let path = router
                .find_best_path(
                    &Coordinate::new_2d(3950.0, 1050.0),
                    SimTime::from_secs(8 * 3600),
                    &[(Id::create("ra"), access_a), (Id::create("rb"), access_b)],
                    &HashSet::from([Id::create("rc")]),
                    &router.resolve_routing_params(""),
                )
                .unwrap();
            assert_eq!(path.arrival, SimTime::from_secs(arrival));
            assert_eq!(path.rides.len(), 1);
            assert_eq!(path.rides[0].departure.external(), service);
            assert_eq!(path.rides[0].board.external(), board);
            assert_eq!(path.rides[0].alight.external(), "rc");
        }
    }

    #[deterministic_id_test]
    fn marked_search_discards_repeated_cycles_but_keeps_useful_transfers() {
        let mut schedule = reference_schedule();
        let line = schedule
            .lines_mut()
            .get_mut(&Id::<TransitLine>::create("Reference Line"))
            .unwrap();
        line.routes
            .get_mut(&Id::<TransitRoute>::create("a_to_b"))
            .unwrap()
            .departures
            .clear();
        let mut middle = line.routes[&Id::<TransitRoute>::create("b_to_c")].stops[0].clone();
        middle.arrival_offset = Some(Duration::from_secs(600));
        middle.departure_offset = Some(Duration::from_secs(600));
        let loop_route = line
            .routes
            .get_mut(&Id::<TransitRoute>::create("direct"))
            .unwrap();
        loop_route.stops[1].facility_id = Id::create("ra");
        loop_route.stops[1].arrival_offset = Some(Duration::from_secs(1200));
        loop_route.stops.insert(1, middle);
        let router = reference_router(schedule, 0.8333333333333334);
        let departure = SimTime::from_secs(8 * 3600);
        let access = router.walk_feeders(&[(Id::create("ra"), 0.0)]);
        let destinations = [
            Coordinate::new_2d(1050.0, 1050.0),
            Coordinate::new_2d(3950.0, 1050.0),
        ];
        let egress = vec![
            router.walk_feeders(&[(Id::create("ra"), 0.0)]),
            router.walk_feeders(&[(Id::create("rc"), 0.0)]),
        ];
        let params = router.resolve_routing_params("");
        let paths = router.find_paths_for_destinations(
            &destinations,
            &egress,
            departure,
            &access,
            &params,
            &BTreeMap::new(),
        );
        for destination in 0..destinations.len() {
            let best = |paths: &[(usize, super::TransitPath)]| {
                let paths = paths
                    .iter()
                    .filter(|(index, _)| *index == destination)
                    .map(|(_, path)| path.clone())
                    .collect::<Vec<_>>();
                router
                    .select_best_path_from_candidates(&paths, departure, &params, &BTreeMap::new())
                    .unwrap()
            };
            let actual = best(&paths);
            assert_eq!(actual.rides.len(), destination + 1);
        }
        assert!(
            paths
                .iter()
                .all(|(destination, path)| path.rides.len() == destination + 1)
        );
    }

    #[deterministic_id_test]
    fn shared_search_applies_destination_round_bounds_independently() {
        let mut schedule = reference_schedule();
        let line = schedule
            .lines_mut()
            .get_mut(&Id::<TransitLine>::create("Reference Line"))
            .unwrap();
        for route in line.routes.values_mut() {
            route.departures.clear();
        }
        line.routes
            .get_mut(&Id::<TransitRoute>::create("a_to_b"))
            .unwrap()
            .departures = vec![TransitDeparture {
            id: Id::create("a_to_b_0800"),
            departure_time: SimTime::from_secs(8 * 3600),
            vehicle_ref_id: None,
            chained_departures: Vec::new(),
            attributes: InternalAttributes::default(),
        }];
        line.routes
            .get_mut(&Id::<TransitRoute>::create("b_to_c"))
            .unwrap()
            .departures = vec![TransitDeparture {
            id: Id::create("b_to_c_0815"),
            departure_time: SimTime::from_secs(8 * 3600 + 15 * 60),
            vehicle_ref_id: None,
            chained_departures: Vec::new(),
            attributes: InternalAttributes::default(),
        }];
        let direct = line
            .routes
            .get_mut(&Id::<TransitRoute>::create("direct"))
            .unwrap();
        direct.stops[1].facility_id = Id::create("rd");
        direct.departures = vec![TransitDeparture {
            id: Id::create("direct_late"),
            departure_time: SimTime::from_secs(12 * 3600),
            vehicle_ref_id: None,
            chained_departures: Vec::new(),
            attributes: InternalAttributes::default(),
        }];
        let mut final_route = line.routes[&Id::<TransitRoute>::create("b_to_c")].clone();
        final_route.id = Id::create("c_to_rd");
        final_route.stops[0].facility_id = Id::create("rc");
        final_route.stops[1].facility_id = Id::create("rd");
        final_route.stops[1].arrival_offset = Some(Duration::from_secs(5 * 60));
        final_route.departures = vec![TransitDeparture {
            id: Id::create("c_to_rd_0826"),
            departure_time: SimTime::from_secs(8 * 3600 + 26 * 60),
            vehicle_ref_id: None,
            chained_departures: Vec::new(),
            attributes: InternalAttributes::default(),
        }];
        line.routes.insert(final_route.id.clone(), final_route);

        let router = reference_router(schedule.clone(), 0.8333333333333334);
        let departure = SimTime::from_secs(8 * 3600);
        let access = [FeederStop {
            stop: Id::create("ra"),
            distance: 0.0,
            time: Duration::ZERO,
            disutility: 0.0,
            route: None,
        }];
        let destinations = [
            Coordinate::new_2d(1051.0, 1050.0),
            Coordinate::new_2d(50_000.0, 50_000.0),
        ];
        let egress = vec![
            vec![FeederStop {
                stop: Id::create("rd"),
                distance: 0.0,
                time: Duration::ZERO,
                disutility: 0.0,
                route: None,
            }],
            vec![FeederStop {
                stop: Id::create("ra"),
                distance: 50_000.0,
                time: router.walk_time(50_000.0),
                disutility: 0.0,
                route: None,
            }],
        ];
        let params = router.resolve_routing_params("");
        let feedback = BTreeMap::new();
        let shared = router.find_paths_for_destinations(
            &destinations,
            &egress,
            departure,
            &access,
            &params,
            &feedback,
        );
        let selected_signature = |paths: &[(usize, super::TransitPath)], destination| {
            let candidates = paths
                .iter()
                .filter(|(index, _)| *index == destination)
                .map(|(_, path)| path.clone())
                .collect::<Vec<_>>();
            let path = router
                .select_best_path_from_candidates(&candidates, departure, &params, &feedback)
                .unwrap();
            path.rides
                .iter()
                .map(|ride| {
                    (
                        ride.line.external().to_owned(),
                        ride.route.external().to_owned(),
                        ride.departure.external().to_owned(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let single = router.find_paths_with_feeders(
            &destinations[0],
            departure,
            &access,
            &egress[0],
            &params,
            &feedback,
        );
        let single_paths = single
            .iter()
            .cloned()
            .map(|path| (0, path))
            .collect::<Vec<_>>();
        assert_eq!(
            selected_signature(&shared, 0),
            selected_signature(&single_paths, 0)
        );

        assert!(
            shared
                .iter()
                .any(|(destination, path)| { *destination == 0 && path.rides.len() == 3 })
        );
        assert!(single.iter().any(|path| path.rides.len() == 3));
        assert!(
            !shared
                .iter()
                .any(|(destination, path)| { *destination == 0 && path.rides.len() > 3 })
        );
        assert!(!single.iter().any(|path| path.rides.len() > 3));

        let mut mapped_router = reference_router(schedule, 0.8333333333333334);
        mapped_router.use_passenger_mode_mapping = true;
        mapped_router
            .passenger_modes
            .insert("train".to_string(), "rail".to_string());
        let mut mapped_params = mapped_router.resolve_routing_params("");
        mapped_params
            .mode_utilities
            .insert("rail".to_string(), 12.0);
        let mapped = mapped_router.find_paths_for_destinations(
            &destinations,
            &egress,
            departure,
            &access,
            &mapped_params,
            &feedback,
        );
        assert!(
            mapped
                .iter()
                .any(|(destination, path)| { *destination == 0 && path.rides.len() > 3 })
        );
    }

    #[deterministic_id_test]
    fn one_to_all_skim_reports_walking_when_it_beats_a_late_transit_departure() {
        let mut schedule = reference_schedule();
        let line = schedule
            .lines_mut()
            .get_mut(&Id::<TransitLine>::create("Reference Line"))
            .unwrap();
        for route in line.routes.values_mut() {
            route.departures.clear();
        }
        line.routes
            .get_mut(&Id::<TransitRoute>::create("a_to_b"))
            .unwrap()
            .departures = vec![TransitDeparture {
            id: Id::create("late"),
            departure_time: SimTime::from_secs(9 * 3600),
            vehicle_ref_id: None,
            chained_departures: Vec::new(),
            attributes: InternalAttributes::default(),
        }];
        let router = reference_router(schedule, 0.8333333333333334);
        let results = router.skim_results_from_origin(
            &Coordinate::new_2d(1050.0, 2940.0),
            &[Coordinate::new_2d(2050.0, 2940.0)],
            SimTime::from_secs(8 * 3600),
        );

        assert_eq!(results[0].outcome, TransitSkimOutcome::Walking);
        assert_eq!(results[0].travel_time, Some(Duration::from_secs(1200)));
    }

    #[deterministic_id_test]
    fn one_to_all_skim_reports_walking_when_stops_have_no_departures() {
        let mut schedule = reference_schedule();
        for route in schedule
            .lines_mut()
            .values_mut()
            .flat_map(|line| line.routes.values_mut())
        {
            route.departures.clear();
        }
        let router = reference_router(schedule, 0.8333333333333334);
        let results = router.skim_results_from_origin(
            &Coordinate::new_2d(1050.0, 1050.0),
            &[Coordinate::new_2d(3950.0, 1050.0)],
            SimTime::from_secs(8 * 3600),
        );

        assert_eq!(results[0].outcome, TransitSkimOutcome::Walking);
        assert_eq!(results[0].travel_time, Some(Duration::from_secs(58 * 60)));
    }

    /// The hourly penalty is priced against when riding began, not when the vehicle leaves the
    /// first stop. MATSim uses `max(agent arrival, vehicle arrival)` (SwissRailRaptorCore:700),
    /// so a vehicle dwelling 120 s at its first stop starts that clock 120 s earlier and the same
    /// ride is charged 120 s / 3600 more per transfer. Asserted on one fixed pair of rides so the
    /// comparison is the origin alone rather than two different journeys.
    #[deterministic_id_test]
    fn an_hourly_penalty_starts_at_the_vehicle_arrival_not_its_departure() {
        let hourly = TransitTransferPenalty {
            // A non-zero hourly cost is what makes `base_cost` take effect at all.
            per_travel_time_hour: 6.0,
            base_cost: 0.0,
            ..TransitTransferPenalty::default()
        };
        let ride =
            |boarding: SimTime, vehicle_arrival_at_board: SimTime, alighting: SimTime| Ride {
                line: Id::create("line"),
                route: Id::create("route"),
                departure: Id::create("departure"),
                board_index: 0,
                alight_index: 1,
                board: Id::create("ra"),
                alight: Id::create("rc"),
                boarding_time: boarding,
                vehicle_arrival_at_board,
                alighting_time: alighting,
                distance: 0.0,
                transport_mode: "train".to_string(),
                passenger_mode: "pt".to_string(),
                transfer_before: None,
                chained_from_previous: false,
            };
        let cost_in_utils = |rides: &[Ride]| {
            let router = reference_router(reference_schedule(), 0.8333333333333334)
                .with_transfer_penalty(hourly.clone());
            let params = router.resolve_routing_params("");
            router.transfer_penalty_seconds(rides, &params) / 300.0
        };

        // Two rides. The first dwells 120 s at its boarding stop: the vehicle reaches the stop at
        // 08:00 but departs at 08:02, so every later stop is reached 120 s later too. Pricing the
        // penalty from boarding instead of arrival would cancel that 120 s out entirely.
        let dwelling = [
            ride(
                SimTime::from_secs(8 * 3600 + 120),
                SimTime::from_secs(8 * 3600),
                SimTime::from_secs(8 * 3600 + 720),
            ),
            ride(
                SimTime::from_secs(8 * 3600 + 1020),
                SimTime::from_secs(8 * 3600 + 1020),
                SimTime::from_secs(8 * 3600 + 1620),
            ),
        ];
        let no_dwell = [
            ride(
                SimTime::from_secs(8 * 3600),
                SimTime::from_secs(8 * 3600),
                SimTime::from_secs(8 * 3600 + 600),
            ),
            ride(
                SimTime::from_secs(8 * 3600 + 900),
                SimTime::from_secs(8 * 3600 + 900),
                SimTime::from_secs(8 * 3600 + 1500),
            ),
        ];

        let expected = 120.0 / 3600.0 * hourly.per_travel_time_hour;
        assert!(
            (cost_in_utils(&dwelling) - cost_in_utils(&no_dwell) - expected).abs() < 1e-9,
            "boarding 120 s after the vehicle arrived should cost {expected} utils more per \
             transfer, but the penalty moved by {}",
            cost_in_utils(&dwelling) - cost_in_utils(&no_dwell)
        );
    }

    #[deterministic_id_test]
    fn a_penalty_worth_more_than_the_time_it_saves_rejects_the_transfer() {
        let destination = Coordinate::new_2d(3950.0, 1050.0);
        let access = [(Id::create("ra"), 0.0)];
        let egress = HashSet::from([Id::create("rc")]);
        let departure = SimTime::from_secs(8 * 3600);
        let chosen = |router: &TransitRoutingModule| {
            let params = router.resolve_routing_params("");
            router
                .find_best_path(&destination, departure, &access, &egress, &params)
                .unwrap()
                .rides
                .iter()
                .map(|ride| ride.route.external().to_owned())
                .collect::<Vec<_>>()
        };

        let default = reference_router(reference_schedule(), 0.8333333333333334);
        assert_eq!(chosen(&default), ["a_to_b", "b_to_c"]);

        // 25 utils is 7500 equivalent seconds, more than the 1500 s the transfer saves, so the
        // direct service wins. 20 utils would not: the boundary is checked below, not asserted.
        let penalized = reference_router(reference_schedule(), 0.8333333333333334)
            .with_transfer_penalty(TransitTransferPenalty {
                base_cost: 25.0,
                per_travel_time_hour: 1.0,
                ..TransitTransferPenalty::default()
            });
        assert_eq!(chosen(&penalized), ["direct"]);
    }

    /// The mode-to-mode penalty is keyed on the route's transport mode, not on the mapped
    /// passenger mode: penalizing train -> bus leaves the reverse direction free.
    #[deterministic_id_test]
    fn a_mode_to_mode_penalty_only_costs_its_own_direction() {
        let destination = Coordinate::new_2d(3950.0, 1050.0);
        let access = [(Id::create("ra"), 0.0)];
        let egress = HashSet::from([Id::create("rc")]);
        let departure = SimTime::from_secs(8 * 3600);
        let schedule = || {
            let mut schedule = reference_schedule();
            schedule
                .lines_mut()
                .get_mut(&Id::<TransitLine>::create("Reference Line"))
                .unwrap()
                .routes
                .get_mut(&Id::<TransitRoute>::create("b_to_c"))
                .unwrap()
                .transport_mode = Id::create("bus");
            schedule
        };
        let chosen = |router: &TransitRoutingModule| {
            let params = router.resolve_routing_params("");
            router
                .find_best_path(&destination, departure, &access, &egress, &params)
                .unwrap()
                .rides
                .iter()
                .map(|ride| ride.route.external().to_owned())
                .collect::<Vec<_>>()
        };

        let penalty = TransitTransferPenalty {
            by_transport_mode: vec![
                crate::simulation::config::TransitModeToModeTransferPenalty {
                    from_mode: "train".to_string(),
                    to_mode: "bus".to_string(),
                    transfer_penalty: 25.0,
                },
            ],
            ..TransitTransferPenalty::default()
        };

        assert_eq!(
            chosen(&reference_router(schedule(), 0.8333333333333334)),
            ["a_to_b", "b_to_c"]
        );
        assert_eq!(
            chosen(
                &reference_router(schedule(), 0.8333333333333334)
                    .with_transfer_penalty(penalty.clone())
            ),
            ["direct"]
        );
        // The same 25 utils on the unconfigured direction changes nothing, which shows the penalty
        // was applied by mode pair rather than as an untargeted cost per transfer.
        let reverse = TransitTransferPenalty {
            by_transport_mode: vec![
                crate::simulation::config::TransitModeToModeTransferPenalty {
                    from_mode: "bus".to_string(),
                    to_mode: "train".to_string(),
                    transfer_penalty: 25.0,
                },
            ],
            ..penalty
        };
        assert_eq!(
            chosen(
                &reference_router(schedule(), 0.8333333333333334).with_transfer_penalty(reverse)
            ),
            ["a_to_b", "b_to_c"]
        );
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
        let params = router.resolve_routing_params("");
        let path = router
            .find_best_path(
                &destination,
                SimTime::from_secs(8 * 3600),
                &access,
                &egress,
                &params,
            )
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
    fn person_specific_routing_costs_let_two_passengers_choose_different_services() {
        // Reuse the reference schedule and relabel the routes so the two-leg option (bus) and
        // the direct option (rail) are visibly distinct services. With the same travel times and
        // a flat per-mode utility the direct route wins; biasing each passenger's per-mode
        // utility flips the answer for one but not the other.
        let mut schedule = reference_schedule();
        let line = schedule
            .lines_mut()
            .get_mut(&Id::<TransitLine>::create("Reference Line"))
            .unwrap();
        line.routes
            .get_mut(&Id::<TransitRoute>::create("b_to_c"))
            .unwrap()
            .transport_mode = Id::create("bus");
        line.routes
            .get_mut(&Id::<TransitRoute>::create("direct"))
            .unwrap()
            .transport_mode = Id::create("rail");

        // Both passengers share one router; the per-subpopulation `mode_params` are what differ.
        // The `person` subpopulation keeps the rail-friendly utilities; the `freight` subpopulation
        // ships rail-averse utilities that prefer the bus transfer.
        let scoring = vec![
            crate::simulation::config::ModeParameter {
                subpopulation: "person".to_owned(),
                mode: "rail".to_owned(),
                marginal_utility_of_traveling: -1.0,
                ..Default::default()
            },
            crate::simulation::config::ModeParameter {
                subpopulation: "person".to_owned(),
                mode: "road".to_owned(),
                marginal_utility_of_traveling: -24.0,
                ..Default::default()
            },
            crate::simulation::config::ModeParameter {
                subpopulation: "freight".to_owned(),
                mode: "rail".to_owned(),
                marginal_utility_of_traveling: -24.0,
                ..Default::default()
            },
            crate::simulation::config::ModeParameter {
                subpopulation: "freight".to_owned(),
                mode: "road".to_owned(),
                marginal_utility_of_traveling: -1.0,
                ..Default::default()
            },
        ];
        let agent_scoring = vec![
            crate::simulation::config::AgentParameter::default(),
            crate::simulation::config::AgentParameter {
                subpopulation: "freight".to_owned(),
                performing: 6.0,
                ..Default::default()
            },
        ];
        let router = reference_router(schedule, 0.8333333333333334).with_passenger_mode_mapping(
            true,
            [
                ("bus".to_owned(), "road".to_owned()),
                ("rail".to_owned(), "rail".to_owned()),
            ]
            .into(),
            &scoring,
            &agent_scoring,
        );

        let departure = SimTime::from_secs(8 * 3600);

        let passenger = InternalPerson::new(
            Id::create("passenger"),
            InternalPlan {
                score: None,
                selected: true,
                elements: Vec::new(),
                attributes: InternalAttributes::default(),
            },
        );
        let freight = InternalPerson::new(
            Id::create("freight-hauler"),
            InternalPlan {
                score: None,
                selected: true,
                elements: Vec::new(),
                attributes: InternalAttributes::default(),
            },
        )
        .with_subpopulation("freight");

        let from = Facility::new_link_wrapper(
            Coordinate::new_2d(1050.0, 1050.0),
            Id::<Link>::create("11"),
        );
        let to = Facility::new_link_wrapper(
            Coordinate::new_2d(3950.0, 1050.0),
            Id::<Link>::create("33"),
        );

        let passenger_path = router
            .calc_route(
                RoutingRequestBuilder::default()
                    .from(&from)
                    .to(&to)
                    .departure_time(departure)
                    .person(Some(&passenger))
                    .build()
                    .unwrap(),
            )
            .unwrap();
        let freight_path = router
            .calc_route(
                RoutingRequestBuilder::default()
                    .from(&from)
                    .to(&to)
                    .departure_time(departure)
                    .person(Some(&freight))
                    .build()
                    .unwrap(),
            )
            .unwrap();

        // Inspect the pt legs only: the walks wrap them, so filter out the walk legs and read
        // the route id from the embedded PT description. The route id identifies the service the
        // passenger picked.
        let pt_route = |elements: &[InternalPlanElement]| -> String {
            elements
                .iter()
                .find_map(|element| {
                    let leg = element.as_leg()?;
                    leg.route
                        .as_ref()?
                        .as_pt()
                        .map(|pt| pt.description.transit_route_id.clone())
                })
                .expect("the passenger picks a transit service")
        };
        let passenger_route = pt_route(&passenger_path);
        let freight_route = pt_route(&freight_path);

        // The passenger subpopulation prefers rail; freight prefers road. The competing
        // candidates are the direct rail service and the bus transfer (a_to_b -> b_to_c), so the
        // passengers must end up on different services. The freight itinerary contains two rides,
        // which is enough to disambiguate it from the single-ride rail itinerary.
        assert_eq!(passenger_route, "direct");
        assert_eq!(freight_route, "a_to_b");
        assert_ne!(passenger_route, freight_route);

        // Repeating the request for the same passenger is deterministic: the same cost resolves
        // to the same service.
        let passenger_again = router
            .calc_route(
                RoutingRequestBuilder::default()
                    .from(&from)
                    .to(&to)
                    .departure_time(departure)
                    .person(Some(&passenger))
                    .build()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(pt_route(&passenger_again), passenger_route);
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
        let access_stops = [(Id::create("ra"), 0.0)];
        let egress_stops = HashSet::from([Id::create("rc")]);
        let access = access_stops;
        let egress = egress_stops;
        let departure = SimTime::from_secs(8 * 3600);
        let params = router.resolve_routing_params("");
        let path = router
            .find_best_path(&destination, departure, &access, &egress, &params)
            .unwrap();
        let repeated = router
            .find_best_path(&destination, departure, &access, &egress, &params)
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
    fn previous_iteration_crowding_and_failed_boarding_change_the_next_route_choice() {
        let mut schedule = reference_schedule();
        let direct = schedule
            .lines_mut()
            .get_mut(&Id::<TransitLine>::create("Reference Line"))
            .unwrap()
            .routes
            .get_mut(&Id::<TransitRoute>::create("direct"))
            .unwrap();
        direct.stops[1].arrival_offset = Some(Duration::from_secs(5 * 60));
        direct.departures = [("d_0800", 8 * 3600), ("d_0830", 8 * 3600 + 30 * 60)]
            .into_iter()
            .map(|(id, time)| TransitDeparture {
                id: Id::create(id),
                departure_time: SimTime::from_secs(time),
                vehicle_ref_id: None,
                chained_departures: Vec::new(),
                attributes: InternalAttributes::default(),
            })
            .collect();

        let router = reference_router(schedule, 0.8333333333333334);
        let destination = Coordinate::new_2d(3950.0, 1050.0);
        let access = [(Id::create("ra"), 0.0)];
        let egress = HashSet::from([Id::create("rc")]);
        let departure = SimTime::from_secs(8 * 3600);
        let params = router.resolve_routing_params("");
        let baseline = router
            .find_best_path(&destination, departure, &access, &egress, &params)
            .unwrap();
        assert_eq!("direct", baseline.rides[0].route.external());
        let empty_feedback = router
            .find_best_path(&destination, departure, &access, &egress, &params)
            .unwrap();
        assert_eq!(baseline.rides[0].route, empty_feedback.rides[0].route);

        let segment = TransitSegment {
            line: Id::create("Reference Line"),
            route: Id::create("direct"),
            departure: Id::create("d_0800"),
            from: Id::create("ra"),
            to: Id::create("rc"),
        };
        let observations =
            crate::simulation::pt::feedback::TransitCapacityFeedbackCollector::default();
        observations.record(
            segment,
            TransitSegmentObservation {
                passengers: 1,
                capacity: 1,
                boarded: 0,
                failed_boardings: 1,
            },
        );
        router
            .capacity_feedback
            .store(Arc::new(observations.take()));
        let next_iteration = router
            .find_best_path(&destination, departure, &access, &egress, &params)
            .unwrap();
        assert_eq!(
            vec!["a_to_b", "b_to_c"],
            next_iteration
                .rides
                .iter()
                .map(|ride| ride.route.external())
                .collect::<Vec<_>>()
        );
        let repeated = router
            .find_best_path(&destination, departure, &access, &egress, &params)
            .unwrap();
        assert_eq!(
            next_iteration
                .rides
                .iter()
                .map(|ride| ride.route.external())
                .collect::<Vec<_>>(),
            repeated
                .rides
                .iter()
                .map(|ride| ride.route.external())
                .collect::<Vec<_>>()
        );
    }

    #[deterministic_id_test]
    fn occupancy_penalty_uses_each_scheduled_segment_duration() {
        let mut schedule = reference_schedule();
        schedule
            .lines_mut()
            .get_mut(&Id::<TransitLine>::create("Reference Line"))
            .unwrap()
            .routes
            .get_mut(&Id::<TransitRoute>::create("direct"))
            .unwrap()
            .stops
            .insert(
                1,
                TransitRouteStop {
                    facility_id: Id::create("rb"),
                    arrival_offset: Some(Duration::from_secs(5 * 60)),
                    departure_offset: Some(Duration::from_secs(10 * 60)),
                    await_departure: None,
                    allow_boarding: true,
                    allow_alighting: true,
                    minimum_stop_duration: Duration::ZERO,
                },
            );
        let router = reference_router(schedule, 0.8333333333333334);
        let ride = Ride {
            line: Id::create("Reference Line"),
            route: Id::create("direct"),
            departure: Id::create("d_0800"),
            board_index: 0,
            alight_index: 2,
            board: Id::create("ra"),
            alight: Id::create("rc"),
            boarding_time: SimTime::from_secs(8 * 3600),
            vehicle_arrival_at_board: SimTime::from_secs(8 * 3600),
            alighting_time: SimTime::from_secs(8 * 3600 + 50 * 60),
            distance: 0.0,
            transport_mode: "bus".to_string(),
            passenger_mode: "pt".to_string(),
            transfer_before: None,
            chained_from_previous: false,
        };
        let crowded_first_segment = BTreeMap::from([(
            TransitSegment {
                line: Id::create("Reference Line"),
                route: Id::create("direct"),
                departure: Id::create("d_0800"),
                from: Id::create("ra"),
                to: Id::create("rb"),
            },
            TransitSegmentObservation {
                passengers: 1,
                capacity: 1,
                ..TransitSegmentObservation::default()
            },
        )]);

        assert_eq!(
            5.0 * 60.0,
            router.capacity_feedback_cost_seconds(&ride, &crowded_first_segment)
        );
    }

    #[deterministic_id_test]
    fn configured_transfer_times_keep_direction_rounding_and_first_match() {
        let fixture = std::fs::read_to_string(
            "./tests/resources/pt_reference/routing_direct_vs_transfer/transit_schedule.xml",
        )
        .unwrap()
        .replace(
            "</transitStops>",
            r#"</transitStops>
            <minimalTransferTimes>
                <relation fromStop="ra" toStop="rc" transferTime="250.2" />
                <relation fromStop="ra" toStop="rc" transferTime="900" />
                <relation fromStop="rb" toStop="rb" transferTime="0" />
            </minimalTransferTimes>"#,
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("schedule.xml");
        std::fs::write(&path, fixture).unwrap();
        let schedule = TransitSchedule::from_file(&path);
        for construction in transfer_construction_modes() {
            let router = reference_router_with_construction(schedule.clone(), 1.0, construction);
            assert_eq!(
                router.transfer_time(&Id::create("ra"), &Id::create("rc"), 0.0),
                Duration::from_secs(251)
            );
            assert_eq!(
                router.transfer_time(&Id::create("rc"), &Id::create("ra"), 0.0),
                Duration::from_secs(60)
            );
            assert_eq!(
                router.transfer_time(&Id::create("rb"), &Id::create("rb"), 0.0),
                Duration::ZERO
            );
            assert!(
                router
                    .nearby_transfer_stops(&Id::create("ra"))
                    .iter()
                    .any(|(stop, _)| stop.external() == "rc")
            );
        }
    }

    #[deterministic_id_test]
    fn transfer_respects_minimum_time_at_just_catch_and_just_miss_boundaries() {
        let departure = SimTime::from_secs(8 * 3600);
        let destination = Coordinate::new_2d(3950.0, 1050.0);
        let access_stops = [(Id::create("ra"), 0.0)];
        let egress_stops = HashSet::from([Id::create("rc")]);
        let access = access_stops;
        let egress = egress_stops;
        let mut schedule = reference_schedule();
        let line = schedule
            .lines_mut()
            .get_mut(&Id::<TransitLine>::create("Reference Line"))
            .unwrap();
        line.routes
            .get_mut(&Id::<TransitRoute>::create("direct"))
            .unwrap()
            .departures
            .clear();

        let transfer_route = line
            .routes
            .get_mut(&Id::<TransitRoute>::create("b_to_c"))
            .unwrap();
        transfer_route.departures = vec![TransitDeparture {
            id: Id::create("boundary"),
            departure_time: SimTime::from_secs(8 * 3600 + 11 * 60),
            vehicle_ref_id: None,
            chained_departures: Vec::new(),
            attributes: InternalAttributes::default(),
        }];
        for construction in transfer_construction_modes() {
            let router = reference_router_with_construction(
                schedule.clone(),
                0.8333333333333334,
                construction,
            );
            assert!(router.nearby_transfer_stops(&Id::create("rb")).is_empty());
            assert_eq!(
                router.transfer_time(&Id::create("rb"), &Id::create("rb"), 0.0),
                Duration::from_secs(60)
            );
            let params = router.resolve_routing_params("");
            let just_catches = router
                .find_best_path(&destination, departure, &access, &egress, &params)
                .unwrap();
            assert_eq!(just_catches.rides.len(), 2);
        }

        schedule
            .lines_mut()
            .get_mut(&Id::<TransitLine>::create("Reference Line"))
            .unwrap()
            .routes
            .get_mut(&Id::<TransitRoute>::create("b_to_c"))
            .unwrap()
            .departures[0]
            .departure_time = SimTime::from_secs(8 * 3600 + 11 * 60 - 1);
        for construction in transfer_construction_modes() {
            let router = reference_router_with_construction(
                schedule.clone(),
                0.8333333333333334,
                construction,
            );
            let params = router.resolve_routing_params("");
            assert!(
                router
                    .find_best_path(&destination, departure, &access, &egress, &params)
                    .is_none()
            );
        }
    }

    #[deterministic_id_test]
    fn walking_transfer_is_unavailable_beyond_java_connection_radius() {
        let mut schedule = separate_platform_schedule(2251);
        schedule
            .lines_mut()
            .get_mut(&Id::<TransitLine>::create("Reference Line"))
            .unwrap()
            .routes
            .get_mut(&Id::<TransitRoute>::create("direct"))
            .unwrap()
            .departures
            .clear();
        let departure = SimTime::from_secs(8 * 3600);
        let access_stops = [(Id::create("ra"), 0.0)];
        let egress_stops = HashSet::from([Id::create("rc")]);
        let access = access_stops;
        let egress = egress_stops;
        for construction in transfer_construction_modes() {
            let router = reference_router_with_construction(
                schedule.clone(),
                0.8333333333333334,
                construction,
            );
            let params = router.resolve_routing_params("");
            assert!(router.nearby_transfer_stops(&Id::create("rb")).is_empty());
            assert!(
                router
                    .find_best_path(
                        &Coordinate::new_2d(3950.0, 1050.0),
                        departure,
                        &access,
                        &egress,
                        &params,
                    )
                    .is_none()
            );
        }
    }

    #[deterministic_id_test]
    fn transfer_construction_modes_select_the_same_unique_itinerary_on_repeated_queries() {
        let schedule = Arc::new(separate_platform_schedule(2150));
        let departure = SimTime::from_secs(8 * 3600);
        let destination = Coordinate::new_2d(3950.0, 1050.0);
        let access_stops = [(Id::create("ra"), 0.0)];
        let egress_stops = HashSet::from([Id::create("rc")]);
        let access = access_stops;
        let egress = egress_stops;
        let expected = vec![
            ("a_to_b".to_string(), "ra".to_string(), "rb".to_string()),
            (
                "b_to_c".to_string(),
                "rb_platform".to_string(),
                "rc".to_string(),
            ),
        ];
        let mut expected_arrival = None;

        for construction in transfer_construction_modes() {
            let router = TransitRoutingModule::new_with_transfer_construction(
                schedule.clone(),
                0.8333333333333334,
                1.0,
                Arc::new(Garage::default()),
                None,
                construction,
            );
            let params = router.resolve_routing_params("");
            let path = router
                .find_best_path(&destination, departure, &access, &egress, &params)
                .unwrap();
            assert_eq!(*expected_arrival.get_or_insert(path.arrival), path.arrival);
            let itinerary = path
                .rides
                .iter()
                .map(|ride| {
                    (
                        ride.route.external().to_string(),
                        ride.board.external().to_string(),
                        ride.alight.external().to_string(),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(itinerary, expected);
            assert_eq!(
                router
                    .find_best_path(&destination, departure, &access, &egress, &params)
                    .unwrap()
                    .arrival,
                path.arrival
            );
        }
    }

    #[deterministic_id_test]
    fn distinct_platform_transfer_emits_walk_leg_with_java_margin() {
        let departure = SimTime::from_secs(8 * 3600);
        let destination = Coordinate::new_2d(3950.0, 1050.0);
        let access_stops = [(Id::create("ra"), 0.0)];
        let egress_stops = HashSet::from([Id::create("rc")]);
        let access = access_stops;
        let egress = egress_stops;
        let mut schedule = separate_platform_schedule(2150);
        let line = schedule
            .lines_mut()
            .get_mut(&Id::<TransitLine>::create("Reference Line"))
            .unwrap();
        line.routes
            .get_mut(&Id::<TransitRoute>::create("direct"))
            .unwrap()
            .departures
            .clear();
        line.routes
            .get_mut(&Id::<TransitRoute>::create("b_to_c"))
            .unwrap()
            .departures = vec![TransitDeparture {
            id: Id::create("just-catch"),
            departure_time: SimTime::from_secs(8 * 3600 + 12 * 60 + 36),
            vehicle_ref_id: None,
            chained_departures: Vec::new(),
            attributes: InternalAttributes::default(),
        }];

        let router = TransitRoutingModule::new(
            Arc::new(schedule.clone()),
            0.8333333333333334,
            1.3,
            Arc::new(Garage::default()),
            None,
        );
        let params = router.resolve_routing_params("");
        let path = router
            .find_best_path(&destination, departure, &access, &egress, &params)
            .unwrap();
        assert_eq!(path.rides[1].board.external(), "rb_platform");
        assert_eq!(
            path.rides[1].transfer_before,
            Some((100.0, 130.0, Duration::from_secs(156)))
        );

        let from = Facility::new_link_wrapper(
            Coordinate::new_2d(1050.0, 1050.0),
            Id::<Link>::create("11"),
        );
        let to = Facility::new_link_wrapper(destination.clone(), Id::<Link>::create("33"));
        let request = RoutingRequestBuilder::default()
            .from(&from)
            .to(&to)
            .departure_time(departure)
            .build()
            .unwrap();
        let plan = router.calc_route(request).unwrap();
        let walk_legs: Vec<_> = plan
            .iter()
            .filter_map(InternalPlanElement::as_leg)
            .filter(|leg| leg.mode.external() == "walk")
            .collect();
        assert_eq!(walk_legs.len(), 3);
        assert_eq!(walk_legs[1].trav_time, Some(Duration::from_secs(151)));
        assert_eq!(
            walk_legs[1].route.as_ref().unwrap().as_generic().distance(),
            Some(130.0)
        );
        assert_eq!(
            plan.iter()
                .filter_map(InternalPlanElement::as_activity)
                .filter(|activity| activity.is_interaction())
                .count(),
            4
        );

        let mut plan_elements = vec![InternalPlanElement::Activity(InternalActivity::new(
            Some(from.coord().clone()),
            "home",
            Id::create("11"),
            None,
            None,
            None,
        ))];
        plan_elements.extend(plan);
        plan_elements.push(InternalPlanElement::Activity(InternalActivity::new(
            Some(to.coord().clone()),
            "work",
            Id::create("33"),
            None,
            None,
            None,
        )));
        let population = Population::from_persons(vec![InternalPerson::new(
            Id::create("transfer-person"),
            InternalPlan {
                score: None,
                selected: true,
                elements: plan_elements,
                attributes: InternalAttributes::default(),
            },
        )]);
        let output = tempfile::tempdir().unwrap();
        let itinerary = |population: &Population| {
            population.persons[&Id::create("transfer-person")].plans()[0]
                .elements
                .iter()
                .map(|element| match element {
                    InternalPlanElement::Activity(activity) => json!({
                        "activity": activity.act_type.external(),
                        "link": activity.link_id().external(),
                    }),
                    InternalPlanElement::Leg(leg) => {
                        let route = leg.route.as_ref().unwrap();
                        json!({
                            "mode": leg.mode.external(),
                            "time": leg.travel_time().as_secs_f64(),
                            "start_link": route.start_link().external(),
                            "end_link": route.end_link().external(),
                            "distance": route.as_generic().distance(),
                            "pt": route.as_pt().map(|pt| {
                                let description = pt.description();
                                json!({
                                    "line": description.transit_line_id,
                                    "route": description.transit_route_id,
                                    "board": description.access_facility_id,
                                    "alight": description.egress_facility_id,
                                    "boarding_time": description.boarding_time.map(|time| time.as_duration().as_secs_f64()),
                                })
                            }),
                        })
                    }
                })
                .collect::<Vec<_>>()
        };
        let expected_itinerary = itinerary(&population);
        for extension in ["xml", "binpb"] {
            let path = output.path().join(format!("itinerary.{extension}"));
            population.to_file(&path);
            let restored = Population::from_file(&path, &mut Garage::default());
            assert_eq!(
                itinerary(&restored),
                expected_itinerary,
                "{extension} changed the transfer itinerary"
            );
        }

        schedule
            .lines_mut()
            .get_mut(&Id::<TransitLine>::create("Reference Line"))
            .unwrap()
            .routes
            .get_mut(&Id::<TransitRoute>::create("b_to_c"))
            .unwrap()
            .departures[0]
            .departure_time = SimTime::from_secs(8 * 3600 + 12 * 60 + 35);
        let router = TransitRoutingModule::new(
            Arc::new(schedule),
            0.8333333333333334,
            1.3,
            Arc::new(Garage::default()),
            None,
        );
        let params = router.resolve_routing_params("");
        assert!(
            router
                .find_best_path(&destination, departure, &access, &egress, &params)
                .is_none()
        );
    }

    #[deterministic_id_test]
    fn xml_and_protobuf_schedule_inputs_produce_identical_routing_decisions() {
        let xml_schedule = reference_schedule();
        let directory = tempfile::tempdir().unwrap();
        let proto_path = directory.path().join("schedule.binpb");
        xml_schedule.to_file(&proto_path);
        let proto_schedule = TransitSchedule::from_file(&proto_path);

        let xml_router = reference_router(xml_schedule, 0.8333333333333334);
        let proto_router = reference_router(proto_schedule, 0.8333333333333334);

        let access = [(Id::create("ra"), 0.0)];
        let egress = HashSet::from([Id::create("rc")]);
        let destination = Coordinate::new_2d(3950.0, 1050.0);
        let departure = SimTime::from_secs(8 * 3600);

        let xml_params = xml_router.resolve_routing_params("");
        let proto_params = proto_router.resolve_routing_params("");

        let xml_path = xml_router
            .find_best_path(&destination, departure, &access, &egress, &xml_params)
            .unwrap();
        let proto_path = proto_router
            .find_best_path(&destination, departure, &access, &egress, &proto_params)
            .unwrap();

        assert_eq!(xml_path.departure, proto_path.departure);
        assert_eq!(xml_path.arrival, proto_path.arrival);
        assert_eq!(xml_path.rides.len(), proto_path.rides.len());
        for (xml_ride, proto_ride) in xml_path.rides.iter().zip(&proto_path.rides) {
            assert_eq!(xml_ride.line, proto_ride.line);
            assert_eq!(xml_ride.route, proto_ride.route);
            assert_eq!(xml_ride.board, proto_ride.board);
            assert_eq!(xml_ride.alight, proto_ride.alight);
            assert_eq!(xml_ride.boarding_time, proto_ride.boarding_time);
            assert_eq!(xml_ride.alighting_time, proto_ride.alighting_time);
        }
    }

    #[deterministic_id_test]
    fn final_departure_is_available_but_schedule_does_not_repeat_after_24_hours() {
        let router = reference_router(reference_schedule(), 0.8333333333333334);
        let access_stops = [(Id::create("ra"), 0.0)];
        let egress_stops = HashSet::from([Id::create("rc")]);
        let access = access_stops;
        let egress = egress_stops;
        let destination = Coordinate::new_2d(3950.0, 1050.0);

        let params = router.resolve_routing_params("");
        let final_departure = router
            .find_best_path(
                &destination,
                SimTime::from_secs(9 * 3600),
                &access,
                &egress,
                &params,
            )
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
                    &params,
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
        let extended_params = extended_router.resolve_routing_params("");
        let after_midnight = extended_router
            .find_best_path(
                &destination,
                SimTime::from_secs(25 * 3600),
                &access,
                &egress,
                &extended_params,
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
    fn walk_limit_accepts_one_hour_and_rejects_a_longer_direct_walk() {
        let walk = Id::create("walk");
        let module: Arc<dyn RoutingModule> =
            Arc::new(TeleportationRoutingModule::new(walk.clone(), 1.0, 1.0));
        let router = TripRouter::new(IntMap::from_iter([(walk.clone(), module)]));
        let from = Facility::new_link_wrapper(Coordinate::new_2d(0.0, 0.0), Id::create("11"));
        let at_limit =
            Facility::new_link_wrapper(Coordinate::new_2d(3600.0, 0.0), Id::create("33"));
        let beyond = Facility::new_link_wrapper(Coordinate::new_2d(3600.1, 0.0), Id::create("33"));
        assert!(
            router
                .calc_route(&walk, request(&from, &beyond, None))
                .is_ok()
        );
        let router = router.with_max_walk_duration(Some(Duration::from_secs(3600)));
        assert!(
            router
                .calc_route(&walk, request(&from, &at_limit, None))
                .is_ok()
        );
        assert!(matches!(
            router.calc_route(&walk, request(&from, &beyond, None)),
            Err(RoutingError::WalkingLimitExceeded { .. })
        ));
    }

    #[deterministic_id_test]
    fn walk_limit_rejects_long_pt_fallback_when_services_are_unavailable() {
        let mut schedule = reference_schedule();
        for line in schedule.lines_mut().values_mut() {
            for route in line.routes.values_mut() {
                route.departures.clear();
            }
        }
        let router = reference_router(schedule, 0.5);
        let (from, to) = reference_trip_endpoints();
        let legacy = router.calc_route(request(&from, &to, None)).unwrap();
        assert!(super::walking_travel_time(&legacy).unwrap() > Duration::from_secs(3600));
        let router = router.with_max_walk_duration(Some(Duration::from_secs(3600)));
        assert_no_path(router.calc_route(request(&from, &to, None)));
    }

    #[deterministic_id_test]
    fn walk_limit_keeps_transit_when_a_faster_direct_walk_is_too_long() {
        let router = reference_router(reference_schedule(), 0.5)
            .with_max_walk_duration(Some(Duration::from_secs(3600)));
        let (from, to) = reference_trip_endpoints();
        let elements = router.calc_route(request(&from, &to, None)).unwrap();
        assert!(elements.iter().any(|element| {
            element
                .as_leg()
                .is_some_and(|leg| matches!(leg.route, Some(InternalRoute::Pt(_))))
        }));
        assert!(super::walking_travel_time(&elements).unwrap() <= Duration::from_secs(3600));
    }

    #[deterministic_id_test]
    fn walk_limit_counts_transfer_walking_before_selecting_a_path() {
        let (from, to) = reference_trip_endpoints();
        for (limit, expected_rides) in [(54, 1), (55, 2)] {
            let router = reference_router(reference_schedule(), 1.0)
                .with_max_walk_duration(Some(Duration::from_secs(limit)));
            let request = RoutingRequestBuilder::default()
                .from(&from)
                .to(&to)
                .departure_time(SimTime::from_secs(8 * 3600))
                .build()
                .unwrap();
            let elements = router.calc_route(request).unwrap();
            let rides = elements
                .iter()
                .filter_map(InternalPlanElement::as_leg)
                .filter(|leg| matches!(leg.route, Some(InternalRoute::Pt(_))))
                .count();
            assert_eq!(rides, expected_rides);
            assert!(super::walking_travel_time(&elements).unwrap() <= Duration::from_secs(limit));
        }
    }

    #[deterministic_id_test]
    fn walk_limit_preserves_a_later_feeder_with_less_walking() {
        let router = reference_router(reference_schedule(), 1.0)
            .with_max_walk_duration(Some(Duration::from_secs(350)));
        let motorized_feeder = InternalPlanElement::Leg(InternalLeg::new(
            InternalRoute::Generic(InternalGenericRoute::new(
                Id::create("11"),
                Id::create("11"),
                Some(Duration::from_secs(400)),
                Some(300.0),
                None,
            )),
            "ride",
            "pt",
            Duration::from_secs(400),
            Some(SimTime::from_secs(0)),
        ));
        let access = [
            super::FeederStop {
                stop: Id::create("ra"),
                distance: 300.0,
                time: Duration::from_secs(300),
                disutility: 0.0,
                route: None,
            },
            super::FeederStop {
                stop: Id::create("ra"),
                distance: 300.0,
                time: Duration::from_secs(400),
                disutility: 0.0,
                route: Some(Arc::new(vec![motorized_feeder])),
            },
        ];
        let egress = [super::FeederStop {
            stop: Id::create("rc"),
            distance: 100.0,
            time: Duration::from_secs(100),
            disutility: 0.0,
            route: None,
        }];
        let path = router
            .find_best_path_with_feeders(
                &Coordinate::new_2d(4050.0, 1050.0),
                SimTime::from_secs(0),
                &access,
                &egress,
                &router.resolve_routing_params(""),
                &BTreeMap::new(),
            )
            .unwrap();
        assert!(
            path.access_route.is_some(),
            "the earlier walking feeder exceeds the combined budget"
        );
    }

    #[deterministic_id_test]
    fn walk_limit_sums_multiple_legs_and_excludes_motorized_time() {
        let make_leg = |mode, seconds| {
            InternalPlanElement::Leg(InternalLeg::new(
                InternalRoute::Generic(InternalGenericRoute::new(
                    Id::create("11"),
                    Id::create("33"),
                    Some(Duration::from_secs(seconds)),
                    Some(1.0),
                    None,
                )),
                mode,
                "pt",
                Duration::from_secs(seconds),
                None,
            ))
        };
        let elements = vec![
            make_leg("walk", 2000),
            make_leg("bus", 7200),
            make_leg("walk", 2000),
        ];
        assert_eq!(
            super::walking_travel_time(&elements).unwrap(),
            Duration::from_secs(4000)
        );
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

    #[deterministic_id_test]
    fn intermodal_modes_choose_lowest_cost_per_stop_and_obey_person_filters() {
        let make_setting = |mode: &str, person_filter_attribute| IntermodalAccessEgress {
            mode: mode.to_owned(),
            initial_search_radius: 1_000.0,
            max_radius: 1_000.0,
            search_extension_radius: 500.0,
            share_trip_search_radius: f64::INFINITY,
            person_filter_attribute,
            person_filter_value: Some("true".to_owned()),
            ..IntermodalAccessEgress::default()
        };
        let mut routers = IntMap::default();
        routers.insert(
            Id::create("walk"),
            Arc::new(TeleportationRoutingModule::new(
                Id::create("walk"),
                1.0,
                1.0,
            )) as Arc<dyn RoutingModule>,
        );
        routers.insert(
            Id::create("bike"),
            Arc::new(TeleportationRoutingModule::new(
                Id::create("bike"),
                1.0,
                10.0,
            )),
        );
        let mut utilities = BTreeMap::new();
        utilities.insert("walk".to_owned(), -6.0);
        utilities.insert("bike".to_owned(), -6.0);
        let module = TransitRoutingModule::new(
            Arc::new(reference_schedule()),
            1.0,
            1.0,
            Arc::new(Garage::default()),
            None,
        )
        .with_intermodal_access_egress(
            vec![
                make_setting("walk", None),
                make_setting("bike", Some("hasBike".to_owned())),
            ],
            routers,
            utilities,
            IntermodalModeSelection::LeastCostPerStop,
            IntermodalLegOnlyHandling::Forbid,
            4711,
        );
        let (mut person, _) = person_with_owns_car("feeder", Some(json!(true)));
        person.attributes_mut().insert("hasBike", "true");
        let from = Facility::new_link_wrapper(Coordinate::new_2d(1051.0, 1050.0), Id::create("11"));
        let to = Facility::new_link_wrapper(Coordinate::new_2d(3950.0, 1050.0), Id::create("33"));
        let eligible_request = request(&from, &to, Some(&person));

        let stops = module
            .intermodal_stops(&eligible_request, eligible_request.from, true)
            .unwrap();
        let ra = stops
            .iter()
            .find(|candidate| candidate.stop.external() == "ra")
            .unwrap();
        let route = ra.route.as_ref().unwrap();
        let InternalPlanElement::Leg(leg) = &route[0] else {
            panic!("feeder router should return a leg");
        };
        assert_eq!("bike", leg.mode.external());

        let (ineligible_person, _) = person_with_owns_car("no_bike", None);
        let ineligible_request = request(&from, &to, Some(&ineligible_person));
        let stops = module
            .intermodal_stops(&ineligible_request, ineligible_request.from, true)
            .unwrap();
        let ra = stops
            .iter()
            .find(|candidate| candidate.stop.external() == "ra")
            .unwrap();
        let InternalPlanElement::Leg(leg) = &ra.route.as_ref().unwrap()[0] else {
            panic!("feeder router should return a leg");
        };
        assert_eq!("walk", leg.mode.external());

        let random_module = TransitRoutingModule::new(
            Arc::new(reference_schedule()),
            1.0,
            1.0,
            Arc::new(Garage::default()),
            None,
        )
        .with_intermodal_access_egress(
            module.intermodal_access_egress.clone(),
            module.feeder_routers.clone(),
            module.feeder_mode_utilities.clone(),
            IntermodalModeSelection::RandomPerDirection,
            IntermodalLegOnlyHandling::Forbid,
            4711,
        );
        let selected_modes = |stops: Vec<super::FeederStop>| {
            stops
                .iter()
                .map(|stop| {
                    let InternalPlanElement::Leg(leg) = &stop.route.as_ref().unwrap()[0] else {
                        panic!("feeder router should return a leg");
                    };
                    (stop.stop.clone(), leg.mode.clone())
                })
                .collect::<Vec<_>>()
        };
        let selected_mode = |request: &RoutingRequest, endpoint, access| {
            selected_modes(
                random_module
                    .intermodal_stops(request, endpoint, access)
                    .unwrap(),
            )[0]
            .1
            .external()
            .to_owned()
        };
        let selected = selected_mode(&eligible_request, eligible_request.from, true);
        assert_eq!(
            selected,
            selected_mode(&eligible_request, eligible_request.from, true)
        );
        assert_ne!(
            selected,
            selected_mode(&eligible_request, eligible_request.to, false)
        );
    }

    #[deterministic_id_test]
    fn stop_filter_applies_before_nearest_stop_fallback() {
        let fixture = std::fs::read_to_string(
            "./tests/resources/pt_reference/routing_direct_vs_transfer/transit_schedule.xml",
        )
        .unwrap();
        let unfiltered_stops = (0..13)
            .map(|index| {
                format!(
                    "<stopFacility id=\"near-{index}\" x=\"1049.5\" y=\"1050\" linkRefId=\"11\" />"
                )
            })
            .collect::<String>();
        let fixture = fixture
            .replace(
                "<stopFacility id=\"ra\" x=\"1050\" y=\"1050\" linkRefId=\"11\" />",
                "<stopFacility id=\"ra\" x=\"1050\" y=\"1050\" linkRefId=\"11\"><attributes><attribute name=\"bikeAccess\" class=\"java.lang.String\">true</attribute><attribute name=\"bikeLink\" class=\"java.lang.String\">12</attribute></attributes></stopFacility>",
            )
            .replace("</transitStops>", &format!("{unfiltered_stops}</transitStops>"));
        let directory = tempfile::tempdir().unwrap();
        let xml_path = directory.path().join("schedule.xml");
        std::fs::write(&xml_path, fixture).unwrap();
        let xml_schedule = TransitSchedule::from_file(&xml_path);
        let proto_path = directory.path().join("schedule.binpb");
        xml_schedule.to_file(&proto_path);
        let proto_schedule = TransitSchedule::from_file(&proto_path);
        let make_module = |schedule| {
            TransitRoutingModule::new(
                Arc::new(schedule),
                1.0,
                1.0,
                Arc::new(Garage::default()),
                None,
            )
            .with_intermodal_access_egress(
                vec![IntermodalAccessEgress {
                    mode: "bike".to_owned(),
                    initial_search_radius: 0.1,
                    max_radius: 2.0,
                    search_extension_radius: 1.0,
                    share_trip_search_radius: f64::INFINITY,
                    stop_filter_attribute: Some("bikeAccess".to_owned()),
                    stop_filter_value: Some("true".to_owned()),
                    link_id_attribute: Some("bikeLink".to_owned()),
                    ..IntermodalAccessEgress::default()
                }],
                IntMap::from_iter([(
                    Id::create("bike"),
                    Arc::new(TeleportationRoutingModule::new(
                        Id::create("bike"),
                        1.0,
                        10.0,
                    )) as Arc<dyn RoutingModule>,
                )]),
                BTreeMap::new(),
                IntermodalModeSelection::LeastCostPerStop,
                IntermodalLegOnlyHandling::Forbid,
                4711,
            )
        };
        let xml_module = make_module(xml_schedule);
        let proto_module = make_module(proto_schedule);
        let from = Facility::new_link_wrapper(Coordinate::new_2d(1049.5, 1050.0), Id::create("11"));
        let to = Facility::new_link_wrapper(Coordinate::new_2d(3950.0, 1050.0), Id::create("33"));
        let request = request(&from, &to, None);

        let xml_stops = xml_module
            .intermodal_stops(&request, request.from, true)
            .unwrap();
        let proto_stops = proto_module
            .intermodal_stops(&request, request.from, true)
            .unwrap();

        let selected = |stops: &[super::FeederStop]| {
            stops
                .iter()
                .map(|stop| stop.stop.external().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(selected(&xml_stops), ["ra"]);
        assert_eq!(selected(&proto_stops), selected(&xml_stops));
        assert_eq!(xml_stops[0].route, proto_stops[0].route);
    }

    #[deterministic_id_test]
    fn stop_finder_keeps_every_stop_inside_the_reference_radius() {
        let fixture = std::fs::read_to_string(
            "./tests/resources/pt_reference/routing_direct_vs_transfer/transit_schedule.xml",
        )
        .unwrap();
        let nearby_stops = (0..13)
            .map(|index| {
                format!(
                    "<stopFacility id=\"near-{index}\" x=\"{}\" y=\"1050\" linkRefId=\"11\" />",
                    1051 + index * 30
                )
            })
            .collect::<String>();
        let fixture = fixture.replace("</transitStops>", &format!("{nearby_stops}</transitStops>"));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("schedule.xml");
        std::fs::write(&path, fixture).unwrap();
        let router = reference_router(TransitSchedule::from_file(&path), 1.0);

        let stops = router.nearest_stops(&Coordinate::new_2d(1050.0, 1050.0));

        assert_eq!(15, stops.len());
        assert!(stops.iter().any(|(stop, _)| stop.external() == "near-12"));
    }

    #[deterministic_id_test]
    fn route_distance_prefixes_match_stop_segment_distances() {
        let router = reference_router(reference_schedule(), 1.0);

        for line in router.schedule.lines().values() {
            for route in line.routes.values() {
                let prefix = &router.distance_prefix_by_route[&(line.id.clone(), route.id.clone())];
                assert_eq!(route.stops.len(), prefix.len());
                for index in 1..route.stops.len() {
                    let from = router
                        .schedule
                        .get_facility(&route.stops[index - 1].facility_id);
                    let to = router
                        .schedule
                        .get_facility(&route.stops[index].facility_id);
                    let segment = Coordinate::euclidean_distance(&from.coord, &to.coord);
                    assert!((segment - (prefix[index] - prefix[index - 1])).abs() < 1e-9);
                }
            }
        }
    }

    #[deterministic_id_test]
    fn unavailable_feeder_routes_are_skipped() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mode = Id::create("bike");
        let mut routers = IntMap::default();
        routers.insert(
            mode.clone(),
            Arc::new(FallbackSpy {
                mode,
                calls: calls.clone(),
                error: Some(RoutingError::NoPath {
                    mode: "bike".to_owned(),
                    from: "11".to_owned(),
                    to: "12".to_owned(),
                }),
            }) as Arc<dyn RoutingModule>,
        );
        let module = TransitRoutingModule::new(
            Arc::new(reference_schedule()),
            1.0,
            1.0,
            Arc::new(Garage::default()),
            None,
        )
        .with_intermodal_access_egress(
            vec![IntermodalAccessEgress {
                mode: "bike".to_owned(),
                initial_search_radius: 1_000.0,
                max_radius: 1_000.0,
                search_extension_radius: 500.0,
                share_trip_search_radius: f64::INFINITY,
                ..IntermodalAccessEgress::default()
            }],
            routers,
            BTreeMap::new(),
            IntermodalModeSelection::LeastCostPerStop,
            IntermodalLegOnlyHandling::Allow,
            4711,
        );
        let from = Facility::new_link_wrapper(Coordinate::new_2d(1050.0, 1050.0), Id::create("11"));
        let to = Facility::new_link_wrapper(Coordinate::new_2d(3950.0, 1050.0), Id::create("33"));
        let request = request(&from, &to, None);

        assert!(
            module
                .intermodal_stops(&request, request.from, true)
                .unwrap()
                .is_empty()
        );
        assert!(calls.load(Ordering::SeqCst) > 0);
    }

    #[deterministic_id_test]
    fn feeder_errors_other_than_no_path_are_propagated() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mode = Id::create("bike");
        let mut routers = IntMap::default();
        routers.insert(
            mode.clone(),
            Arc::new(FallbackSpy {
                mode,
                calls,
                error: Some(RoutingError::MissingEndTime {
                    mode: "bike".to_owned(),
                }),
            }) as Arc<dyn RoutingModule>,
        );
        let module = TransitRoutingModule::new(
            Arc::new(reference_schedule()),
            1.0,
            1.0,
            Arc::new(Garage::default()),
            None,
        )
        .with_intermodal_access_egress(
            vec![IntermodalAccessEgress {
                mode: "bike".to_owned(),
                initial_search_radius: 1_000.0,
                max_radius: 1_000.0,
                search_extension_radius: 500.0,
                share_trip_search_radius: f64::INFINITY,
                ..IntermodalAccessEgress::default()
            }],
            routers,
            BTreeMap::new(),
            IntermodalModeSelection::LeastCostPerStop,
            IntermodalLegOnlyHandling::Allow,
            4711,
        );
        let from = Facility::new_link_wrapper(Coordinate::new_2d(1050.0, 1050.0), Id::create("11"));
        let to = Facility::new_link_wrapper(Coordinate::new_2d(3950.0, 1050.0), Id::create("33"));
        let request = request(&from, &to, None);

        assert!(matches!(
            module.intermodal_stops(&request, request.from, true),
            Err(RoutingError::MissingEndTime { mode }) if mode == "bike"
        ));
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

    #[test]
    fn transit_settings_prefer_exact_subpopulation_over_default() {
        let settings = [
            TransitRouteSelectorSettings::default(),
            TransitRouteSelectorSettings {
                beta_departure_time: 2.0,
                subpopulations: vec!["freight".to_string()],
                ..TransitRouteSelectorSettings::default()
            },
        ];
        assert_eq!(
            Some(2.0),
            matching_transit_settings(&settings, "freight")
                .map(|setting| setting.beta_departure_time)
        );
        assert_eq!(
            Some(0.0),
            matching_transit_settings(&settings, "person")
                .map(|setting| setting.beta_departure_time)
        );

        let range = [TransitRangeQuerySettings {
            max_earlier_departure_sec: 300,
            max_later_departure_sec: 600,
            subpopulations: vec!["freight".to_string()],
        }];
        assert!(matching_transit_settings(&range, "person").is_none());
        assert_eq!(
            Some(300),
            matching_transit_settings(&range, "freight")
                .map(|setting| setting.max_earlier_departure_sec)
        );
    }

    #[deterministic_id_test]
    fn range_query_includes_window_boundaries_and_repeats_the_same_choice() {
        let router = reference_router(reference_schedule(), 0.8333333333333334);
        let destination = Coordinate::new_2d(3950.0, 1050.0);
        let access_stops = [(Id::create("ra"), 0.0)];
        let egress_stops = HashSet::from([Id::create("rc")]);
        let access = router.walk_feeders(&access_stops);
        let egress = router.walk_egress_feeders(&destination, &egress_stops);
        let desired = SimTime::from_secs(8 * 3600);
        let settings = TransitRangeQuerySettings {
            max_earlier_departure_sec: 60,
            max_later_departure_sec: 60,
            subpopulations: Vec::new(),
        };

        let chosen = router
            .select_range_query_path(
                &destination,
                desired,
                &access,
                &egress,
                &settings,
                None,
                &router.resolve_routing_params(""),
                &BTreeMap::new(),
            )
            .unwrap();
        let repeated = router
            .select_range_query_path(
                &destination,
                desired,
                &access,
                &egress,
                &settings,
                None,
                &router.resolve_routing_params(""),
                &BTreeMap::new(),
            )
            .unwrap();

        assert!(chosen.departure >= desired.saturating_sub(Duration::from_secs(60)));
        assert!(chosen.departure <= desired.saturating_add(Duration::from_secs(60)));
        assert_eq!(chosen.departure, repeated.departure);
        assert_eq!(
            transit_path_tiebreak(&chosen, &repeated),
            std::cmp::Ordering::Equal
        );

        let no_window = TransitRangeQuerySettings::default();
        let params = router.resolve_routing_params("");
        let fixed_time = router
            .find_best_path_with_feeders(
                &destination,
                desired,
                &access,
                &egress,
                &params,
                &BTreeMap::new(),
            )
            .unwrap();
        assert_eq!(fixed_time.departure, desired);

        let transfer_averse = reference_router(reference_schedule(), 0.8333333333333334)
            .with_range_queries(
                vec![no_window.clone()],
                vec![TransitRouteSelectorSettings {
                    beta_travel_time: 1.0,
                    beta_departure_time: 0.0,
                    beta_transfer_count: 3_600.0,
                    subpopulations: Vec::new(),
                }],
                42,
            );
        let transfer_averse_params = transfer_averse.resolve_routing_params("");
        let transfer_averse = transfer_averse
            .select_range_query_path(
                &destination,
                desired,
                &access,
                &egress,
                &no_window,
                None,
                &transfer_averse_params,
                &BTreeMap::new(),
            )
            .unwrap();
        assert_eq!("direct", transfer_averse.rides[0].route.external());

        let after_final_service = SimTime::from_secs(9 * 3600 + 60);
        assert!(
            router
                .select_range_query_path(
                    &destination,
                    after_final_service,
                    &access,
                    &egress,
                    &no_window,
                    None,
                    &params,
                    &BTreeMap::new(),
                )
                .is_none()
        );
    }

    #[deterministic_id_test]
    fn range_query_random_streams_are_independent_per_person() {
        let selector = TransitRouteSelectorSettings {
            beta_departure_time: 0.0,
            beta_travel_time: 0.0,
            beta_transfer_count: 0.0,
            subpopulations: Vec::new(),
        };
        let router = reference_router(reference_schedule(), 0.8333333333333334).with_range_queries(
            vec![TransitRangeQuerySettings::default()],
            vec![selector],
            42,
        );
        let destination = Coordinate::new_2d(3950.0, 1050.0);
        let access_stops = [(Id::create("ra"), 0.0)];
        let egress_stops = HashSet::from([Id::create("rc")]);
        let access = router.walk_feeders(&access_stops);
        let egress = router.walk_egress_feeders(&destination, &egress_stops);
        let desired = SimTime::from_secs(8 * 3600);
        let settings = TransitRangeQuerySettings::default();
        let person = |id| {
            InternalPerson::new(
                Id::create(id),
                InternalPlan {
                    score: None,
                    selected: true,
                    elements: Vec::new(),
                    attributes: InternalAttributes::default(),
                },
            )
        };
        let first = person("stream-person-1");
        let second = person("stream-person-2");
        let params = router.resolve_routing_params("");
        let choose = |person: &InternalPerson| {
            router
                .select_range_query_path(
                    &destination,
                    desired,
                    &access,
                    &egress,
                    &settings,
                    Some(person),
                    &params,
                    &BTreeMap::new(),
                )
                .unwrap()
        };

        let first_choice = choose(&first);
        assert_eq!(
            transit_path_tiebreak(&first_choice, &choose(&first)),
            std::cmp::Ordering::Equal
        );
        assert_ne!(
            transit_path_tiebreak(&first_choice, &choose(&second)),
            std::cmp::Ordering::Equal
        );
    }
}
