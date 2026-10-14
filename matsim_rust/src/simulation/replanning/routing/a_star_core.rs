use crate::simulation::replanning::routing::a_star::{AStarHeuristic, ZeroHeuristic};
use crate::simulation::replanning::routing::cost::{Disutility, TravelDisutility, TravelTime};
use crate::simulation::replanning::routing::graph::{
    GraphError, IndexableGraph, LinkIndex, NodeIndex,
};
use crate::simulation::replanning::routing::least_cost_path_calculator::LeastCostPathRequest;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::SimTime;
use derive_builder::Builder;
use ordered_float::OrderedFloat;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fmt::Debug;
use std::time::Duration;
use tracing::warn;

/// Specifies which heuristic to use for A* search
///
/// - `WithHeuristic(&'a H)`: Use the provided heuristic for One-to-One routing with A*
/// - `WithoutHeuristic`: Use zero heuristic (collapses A* to pure Dijkstra),
///   for One-to-Many landmark distance calculations
///     - this allows to run `a_star_core` without a `to`-node, since even when using
///     `ZeroHeuristic`, a node would have to be passed. But with this setting, `a_star_core` knows
///     not to call any Heuristic
#[derive(Debug)]
pub(crate) enum HeuristicMode<'a, H: AStarHeuristic = ZeroHeuristic> {
    WithHeuristic(&'a H),
    WithoutHeuristic,
}

impl<'a, H: AStarHeuristic> Clone for HeuristicMode<'a, H> {
    fn clone(&self) -> Self {
        match self {
            HeuristicMode::WithHeuristic(h) => HeuristicMode::WithHeuristic(h),
            HeuristicMode::WithoutHeuristic => HeuristicMode::WithoutHeuristic,
        }
    }
}

impl<'a, H: AStarHeuristic> HeuristicMode<'a, H> {
    pub fn with_heuristic(heuristic: &'a H) -> Self {
        HeuristicMode::WithHeuristic(heuristic)
    }
}

impl<'a> HeuristicMode<'a, ZeroHeuristic> {
    pub fn without_heuristic() -> Self {
        HeuristicMode::WithoutHeuristic
    }
}

/// Shorthand for `Reverse<OrderedFloat<f64>>`, i.e., an ordered float (implements Eq and Ord,
/// unlike f64) which is sorted in reverse order.
/// To be used in the BinaryHeap in A*, since the heap prefers large values while we
/// prefer small values.
#[derive(Eq, Ord, PartialEq, PartialOrd)]
struct NodePriority(Reverse<OrderedFloat<f64>>);

impl NodePriority {
    pub fn new(priority: f64) -> Self {
        NodePriority(Reverse(OrderedFloat(priority)))
    }
}

/// Per-node search state of `a_star_core`, kept between searches to safe memory consumption.
/// Invariant: between two searches, all entries hold their initial values (disutility infinity,
/// not closed), except for the nodes listed in `touched`, which `prepare` resets.
#[derive(Default)]
pub(crate) struct AStarBuffers {
    disutilities: Vec<Disutility>,
    closed: Vec<bool>,
    touched: Vec<NodeIndex>,
    /// Lazy queue: a node is pushed once per improvement of its disutility, outdated entries are
    /// skipped when popped. On equal priority, the smaller node index is popped first (Reverse),
    /// which makes the choice between equally expensive paths reproducible.
    queue: BinaryHeap<(NodePriority, Reverse<NodeIndex>)>,
}

impl AStarBuffers {
    /// Restores the initial values of the nodes touched by the previous search and grows the
    /// buffers to at least `num_nodes`. Called at the start of every search, so the invariant also
    /// holds after an early return, a `GraphError` or a panic in the previous search.
    pub(crate) fn prepare(&mut self, num_nodes: usize) {
        for &node in &self.touched {
            self.disutilities[node] = f64::INFINITY;
            self.closed[node] = false;
        }
        self.touched.clear();
        self.queue.clear();
        // only grow: shrinking would make alternating searches on graphs of different sizes O(N)
        if self.disutilities.len() < num_nodes {
            self.disutilities.resize(num_nodes, f64::INFINITY);
            self.closed.resize(num_nodes, false);
        }
    }
}

/// Per-node state of the routing use case (see `RoutingAStarActions`), with the same invariant as
/// `AStarBuffers`: initial values are `None` and `SimTime::max()`.
#[derive(Debug, Default)]
pub(crate) struct RoutingBuffers {
    /// link on which the last search arrived at a node, used to reconstruct the path
    pub(crate) parent_links: Vec<Option<LinkIndex>>,
    arrival_times: Vec<SimTime>,
}

/// Search buffers of a routing search. Parent links and arrival times are only written for nodes
/// whose disutility drops below infinity, so the core's `touched` list covers them as well.
#[derive(Default)]
pub(crate) struct SearchBuffers {
    pub(crate) core: AStarBuffers,
    pub(crate) routing: RoutingBuffers,
}

impl SearchBuffers {
    /// Restores the initial values of all entries written by the previous search and grows the
    /// buffers to at least `num_nodes`. The routing buffers are reset first, since resetting the
    /// core buffers forgets the touched nodes.
    pub(crate) fn prepare(&mut self, num_nodes: usize) {
        let routing = &mut self.routing;
        for &node in &self.core.touched {
            routing.parent_links[node] = None;
            routing.arrival_times[node] = SimTime::max();
        }
        if routing.parent_links.len() < num_nodes {
            routing.parent_links.resize(num_nodes, None);
            routing.arrival_times.resize(num_nodes, SimTime::max());
        }
        self.core.prepare(num_nodes);
    }
}

pub(crate) enum AStarCoreResult {
    /// Distance (=travel disutility) from one node to all other nodes in the graph
    DisutilityToAllWithoutParents(Vec<Disutility>),
    /// Shortest distance (=travel disutility) from one node to another, with the associated travel
    /// time. The parent links (the link from which the algorithm arrived at the node) are tracked
    /// in the `RoutingBuffers`.
    SingleDisutil(Disutility, Duration),
}

/// Implementations of this trait represent different use cases of `a_star_core`.
/// In particular, they set whether the A* search is One2One or One2Many, whether parents are
/// tracked or not and whether arrival times at nodes are tracked or not.
/// Specifically, the implementations decide:
/// - at every current node in the algorithm, whether it should stop, since it reached its goal
/// - upon reaching a node, whether its parent link should be tracked
/// - when scanning neighbours of the current node, whether to track the arrival time at the
///     neighbour nodes.
/// - when the algorithm returns, what form the result should have (e.g. with or without parents)
pub(crate) trait AStarActions: Debug {
    /// Called by `a_star_core` at every visited node, the alg will return if it receives `true`
    fn reached_end(&self, current_node: NodeIndex) -> bool;
    /// Called by `a_star_core` when a node is reached, the implementation decides whether to store
    /// the information about the parent link (the link from which the algorithm arrived at the
    /// node), and if yes, how
    fn set_parent_link_opt(&mut self, child: NodeIndex, parent_link: LinkIndex);
    /// Creates a A* result, the trait implementation chooses the result enum variant.
    /// Consumes self to allow moving values without cloning.
    /// This is okay, since the method is called when A* finishes.
    fn build_result(
        self,
        current_disutility: Option<Disutility>,
        initial_departure_time: SimTime,
        disutilities: &[Disutility],
    ) -> AStarCoreResult;
    /// Called by `a_star_core` to get the to-node, to be able to pass it to a heuristic
    fn get_to_node_opt(&self) -> Option<NodeIndex>;
    /// Called to store the arrival time at a specific node. Implementations decide if and how they
    /// do it.
    fn set_arrival_time_opt(&mut self, node: NodeIndex, time: SimTime);
    /// Called to store the arrival time at a specific neighbour of the current node, using a
    /// given link. Implementations decide if and how they do it (typically based on a call to
    /// a TravelTime function for the given link).
    fn set_arrival_time_at_neighbour_opt(
        &mut self,
        current_node: NodeIndex, // needed to get the arrival time at the start of the link
        neighbour_node: NodeIndex,
        link: &Link,
        person: Option<&InternalPerson>,
        vehicle: Option<&InternalVehicle>,
    );
    /// Called to get the arrival time at a specific node. Implementations that do not track arrival
    /// times will return None.
    fn get_arrival_time_at_node_opt(&self, node: NodeIndex) -> Option<SimTime>;
    /// Called to get the travel disutility, which is used as cost, of a given link. Implementations
    /// choose how to do this, in particular they can either use the minimum travel disutility of a
    /// given link (this is done for landmark calculation) or they can use the actual travel
    /// disutility at the arrival time at the start of the link (this is done for routing).
    fn get_disutility_of_link(
        &self,
        link: &Link,
        start_node_of_link: NodeIndex,
        person: Option<&InternalPerson>,
        vehicle: Option<&InternalVehicle>,
    ) -> Disutility;
}

/// These objects represent the A* use case "Landmark calculation", i.e., A* searches from one node
/// to all others, tracks neither parents nor arrival times, and uses the MIN travel disutility of
/// links as cost (independent of time, person, vehicle). This ensures that an ALT heuristic based
/// on that data is admissible, i.e., doesn't overestimate travel disutilities.
#[derive(Clone, Debug)]
pub(crate) struct LandmarkCalcAStarActions<'a> {
    travel_disutility: &'a dyn TravelDisutility,
}

impl<'a> LandmarkCalcAStarActions<'a> {
    pub fn new(travel_disutility: &'a dyn TravelDisutility) -> Self {
        Self { travel_disutility }
    }
}

impl AStarActions for LandmarkCalcAStarActions<'_> {
    fn reached_end(&self, _current_node: NodeIndex) -> bool {
        false
    }
    fn set_parent_link_opt(&mut self, _child: NodeIndex, _parent_link: LinkIndex) {}
    fn build_result(
        self,
        _current_disutility: Option<Disutility>,
        _initial_departure_time: SimTime,
        disutilities: &[Disutility],
    ) -> AStarCoreResult {
        AStarCoreResult::DisutilityToAllWithoutParents(disutilities.to_vec())
    }
    fn get_to_node_opt(&self) -> Option<NodeIndex> {
        None
    }
    fn set_arrival_time_opt(&mut self, _node: NodeIndex, _time: SimTime) {}
    fn set_arrival_time_at_neighbour_opt(
        &mut self,
        _current_node: NodeIndex,
        _neighbour_node: NodeIndex,
        _link: &Link,
        _person: Option<&InternalPerson>,
        _vehicle: Option<&InternalVehicle>,
    ) {
    }

    fn get_arrival_time_at_node_opt(&self, _node: NodeIndex) -> Option<SimTime> {
        None
    }

    fn get_disutility_of_link(
        &self,
        link: &Link,
        _start_node_of_link: NodeIndex,
        _person: Option<&InternalPerson>,
        _vehicle: Option<&InternalVehicle>,
    ) -> Disutility {
        self.travel_disutility.get_link_min_travel_disutility(link)
    }
}

/// The A* use case "Routing". That is, A* searches from one node to exactly one other, i.e., stops
/// early if the to-node was reached. It will also track parent links (links from which the algorithm
/// arrived at nodes) so that the path can be reconstructed, and it tracks arrival times at nodes
/// on the way. Uses the actual travel disutility of links at the time that they are reached (this
/// is what the arrival times are tracked for).
#[derive(Debug)]
pub(crate) struct RoutingAStarActions<'a> {
    to_node: NodeIndex,
    buffers: &'a mut RoutingBuffers,
    travel_time: &'a dyn TravelTime,
    travel_disutility: &'a dyn TravelDisutility,
}

impl<'a> RoutingAStarActions<'a> {
    /// create a new `RoutingAStarActions` object. Parent links and arrival times are written to the
    /// given buffers, which must hold their initial values `None` and `SimTime::max()` (see
    /// `SearchBuffers::prepare`)
    pub fn new(
        to_node: NodeIndex,
        travel_time: &'a dyn TravelTime,
        travel_disutility: &'a dyn TravelDisutility,
        buffers: &'a mut RoutingBuffers,
    ) -> Self {
        Self {
            to_node,
            buffers,
            travel_time,
            travel_disutility,
        }
    }
}

impl AStarActions for RoutingAStarActions<'_> {
    fn reached_end(&self, current_node: NodeIndex) -> bool {
        self.to_node == current_node
    }
    fn set_parent_link_opt(&mut self, child: NodeIndex, parent_link: LinkIndex) {
        self.buffers.parent_links[child] = Some(parent_link);
    }

    /// constructs a "single distance" result, containing the distance from the from-node to the
    /// to-node and the associated travel time. The tracked parent links remain in the routing
    /// buffers.
    fn build_result(
        self,
        current_disutility: Option<Disutility>,
        initial_departure_time: SimTime,
        _disutilities: &[Disutility],
    ) -> AStarCoreResult {
        let current_arrival_time = self.get_arrival_time_at_node_opt(self.to_node).unwrap();

        let current_travel_time = current_arrival_time
            .as_duration()
            .saturating_sub(initial_departure_time.as_duration());

        AStarCoreResult::SingleDisutil(
            current_disutility.expect("A* use case 1to1 requires that current disutility is given"),
            current_travel_time,
        )
    }

    fn get_to_node_opt(&self) -> Option<NodeIndex> {
        Some(self.to_node)
    }

    fn set_arrival_time_opt(&mut self, node: NodeIndex, time: SimTime) {
        self.buffers.arrival_times[node] = time;
    }

    fn set_arrival_time_at_neighbour_opt(
        &mut self,
        current_node: NodeIndex,
        neighbour_node: NodeIndex,
        link: &Link,
        person: Option<&InternalPerson>,
        vehicle: Option<&InternalVehicle>,
    ) {
        let time_at_link_start = self.get_arrival_time_at_node_opt(current_node).unwrap();

        let travel_time_to_neighbour =
            self.travel_time
                .travel_time(link, time_at_link_start, person, vehicle);

        self.set_arrival_time_opt(
            neighbour_node,
            time_at_link_start.saturating_add(travel_time_to_neighbour),
        );
    }

    fn get_arrival_time_at_node_opt(&self, node: NodeIndex) -> Option<SimTime> {
        Some(self.buffers.arrival_times[node])
    }

    fn get_disutility_of_link(
        &self,
        link: &Link,
        start_node_of_link: NodeIndex,
        person: Option<&InternalPerson>,
        vehicle: Option<&InternalVehicle>,
    ) -> Disutility {
        let arrival_time_at_start_of_link = self
            .get_arrival_time_at_node_opt(start_node_of_link)
            .expect(
                "Start node of link must have been visited and therefore have an arrival time.",
            );

        self.travel_disutility.travel_disutility(
            link,
            arrival_time_at_start_of_link,
            person,
            vehicle,
        )
    }
}

/// Request for A* runs. Contains
/// - data needed for calculation, that is the graph, the travel time and travel disutility
///     functions, the from-node, the departure time, the person and vehicle (if applicable)
/// - a `AStarActions` implementation that determines the use case (routing or landmark calculation,
///     that is, parent tracking or not, one to many or not, arrival time tracking or not). The
///     implementation also contains the travel disutility function, and the travel time function
///     and the to-node when applicable.
/// - the `HeuristicMode`: a heuristic to be used, or the information that none is to be used
/// - a bool specifying whether the search is to be performed forwards or backwards. In the
///     latter case, paths using incoming edges, i.e., paths leading going to the from-node,
///     are searched.
#[derive(Builder, Debug)]
#[builder(pattern = "owned")]
pub(crate) struct AStarRequest<'a, H: AStarHeuristic, O: AStarActions> {
    heuristic_mode: HeuristicMode<'a, H>,
    from: NodeIndex,
    // Note: the to-node is stored in the options, when applicable, since it is only used in certain use cases (1to1)
    // same for the TravelTime function. TravelDisutility is also stored in the options since the
    // travel disutility is called via the options object.
    graph: &'a dyn IndexableGraph,
    options: O,
    #[builder(default)]
    departure_time: SimTime,
    #[builder(default)]
    person: Option<&'a InternalPerson>,
    #[builder(default)]
    vehicle: Option<&'a InternalVehicle>,
    #[builder(default)]
    backward: bool, // if true, uses the incoming edges (backward graph) when looking for neighbours
}

impl<'a, H: AStarHeuristic, O: AStarActions> AStarRequestBuilder<'a, H, O> {
    /// partially builds a A* request using data from a given least cost path request and graph
    pub(crate) fn from_least_cost_path_request_with_graph(
        self,
        request: &LeastCostPathRequest<'a>,
        graph: &'a dyn IndexableGraph,
    ) -> Result<Self, GraphError> {
        // convert "from"-link id to corresponding from-node id, and then to NodeIndex
        let from_node_id = graph.get_end_node(request.from.clone())?;

        let from_idx = graph.get_node_idx_from_id(from_node_id);

        Ok(self
            .graph(graph)
            .departure_time(request.departure_time)
            .person(request.person)
            .vehicle(request.vehicle)
            .from(from_idx))
    }
}

/// Core A* logic.
/// Can be used for different use cases, currently:
/// - Routing: calculate the least cost path from one node to another, tracking
///     parent links and arrival times at all nodes
/// - Landmark calculation: calculate disutilites from one to all other nodes, based on the
///     minimum travel disutility for each link (independent of time, vehicle, ...).
/// The search state is kept in the given `buffers`, which are prepared at the start, so the
/// effort of a search depends on the number of visited nodes, not on the size of the graph.
pub(crate) fn a_star_core<H: AStarHeuristic, O: AStarActions>(
    mut request: AStarRequest<H, O>,
    buffers: &mut AStarBuffers,
) -> Result<AStarCoreResult, GraphError> {
    let number_of_nodes = request.graph.num_nodes();

    let from_node = request.from;

    // all disutilities are infinity, no node is closed and the queue is empty
    buffers.prepare(number_of_nodes);
    let AStarBuffers {
        disutilities,
        closed,
        touched,
        queue,
    } = buffers;

    disutilities[from_node] = 0.0;
    touched.push(from_node);
    queue.push((NodePriority::new(0.0), Reverse(from_node)));

    // The arrival times are initialized with SimTime::max() for all nodes, so the arrival time at
    // the from-node must be set to the departure time manually.
    request
        .options
        .set_arrival_time_opt(from_node, request.departure_time);

    // MAIN LOOP of A* search
    while let Some((_, Reverse(current_id))) = queue.pop() {
        // skip outdated entries: the node was already popped with a smaller priority
        if closed[current_id] {
            continue;
        }
        closed[current_id] = true;

        // disutility from "from"-node to the current_id node
        let current_disutility = disutilities[current_id];

        // checking "unusual" values of current_disutility
        match current_disutility {
            f64::INFINITY => {
                //The smallest value in queue was unreachable. So abort here.

                // this chooses the correct result enum variant automatically
                return Ok(request.options.build_result(
                    Some(current_disutility),
                    request.departure_time,
                    &disutilities[..number_of_nodes],
                ));
            }
            f64::NEG_INFINITY => {
                warn!("Disutility of negative infinity encountered in A*.");
            }
            nan_disutility if nan_disutility.is_nan() => {
                // The smallest value in queue is NaN, treated as worse than disutility infinity
                warn!(
                    "Queue in A* only contains entries with disutility NaN, which are\
                    treated as unreachable. Aborting A*."
                );

                return Ok(request.options.build_result(
                    Some(nan_disutility),
                    request.departure_time,
                    &disutilities[..number_of_nodes],
                ));
            }
            _ => {}
        }

        // check if the target node has been reached, if applicable, in that case return early
        if request.options.reached_end(current_id) == true {
            // this chooses the correct result enum variant automatically
            return Ok(request.options.build_result(
                Some(current_disutility),
                request.departure_time,
                &disutilities[..number_of_nodes],
            ));
        }

        // if request.backward=true, we consider the incoming edges, to consider paths from
        // other nodes to the "from"-node
        let neighbour_edges = if request.backward {
            request.graph.incoming_edges_as_idx(current_id)
        } else {
            request.graph.outgoing_edges_as_idx(current_id)
        };

        // go through all neighbours of the current node. If the disutility to get there is smaller
        // than what was previously found, set the disutility of the neighbour to the smaller value
        // and update its priority in the queue. Also, if parent tracking is enabled, update the
        // parent link of the neighbour node to be the current link.
        for i in neighbour_edges {
            // When backward=true, incoming_edges return edges TO the current node,
            // so we need the start node to get the neighbours.
            // When backward=false, outgoing_edges return edges FROM the current node, so we
            // need the end node.
            let neighbour = if request.backward {
                request.graph.get_start_node_as_idx(i)
            } else {
                request.graph.get_end_node_as_idx(i)
            }?;

            // Skip neighbours that have already been popped from the queue (closed).
            if closed[neighbour] {
                continue;
            }

            let link_i = request.graph.get_link_from_idx(i)?;

            // Evaluates the link disutility at the actual arrival time at the current node, not
            // the initial departure time.
            // This is handled by the options object.
            let neighbour_disutility = current_disutility
                + request.options.get_disutility_of_link(
                    link_i,
                    current_id, // start_node_of_link
                    request.person,
                    request.vehicle,
                );

            if disutilities[neighbour] > neighbour_disutility {
                // first time the neighbour is reached: remember it before writing any of its
                // entries, so that the next `prepare` resets them
                if disutilities[neighbour] == f64::INFINITY {
                    touched.push(neighbour);
                }
                // update disutility to neighbour node
                disutilities[neighbour] = neighbour_disutility;

                // tell options object to track the arrival time at the neighbour node
                request.options.set_arrival_time_at_neighbour_opt(
                    current_id,
                    neighbour,
                    link_i,
                    request.person,
                    request.vehicle,
                );

                // Compute heuristic estimate based on the heuristic mode
                let heuristic_estimate = match &request.heuristic_mode {
                    HeuristicMode::WithHeuristic(h) => {
                        let to_node_idx = request.options.get_to_node_opt().expect(
                            "Heuristic mode is WithHeuristic, but no to_node \
                                provided in AStarOptions.",
                        );

                        let to_node_id = request.graph.get_node_id_from_idx(to_node_idx)?;

                        h.estimate(request.graph.get_node_id_from_idx(neighbour)?, to_node_id)
                    }
                    HeuristicMode::WithoutHeuristic => {
                        // This collapses A* to pure Dijkstra.
                        0.0
                    }
                };

                // push the neighbour with its new priority, which is the (now lower) disutility
                // to get there plus the heuristic estimate to get to the target (if applicable).
                // An older entry of the neighbour stays in the queue and is skipped when popped.
                queue.push((
                    NodePriority::new(neighbour_disutility + heuristic_estimate),
                    Reverse(neighbour),
                ));
                // update parent link if applicable
                request.options.set_parent_link_opt(neighbour, i)
            }
        }
    }

    Ok(request.options.build_result(
        Some(f64::INFINITY),
        request.departure_time,
        &disutilities[..number_of_nodes],
    ))
}
