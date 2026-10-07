use crate::simulation::id::Id;
use crate::simulation::replanning::routing::a_star_core::{
    AStarCoreResult, AStarRequestBuilder, HeuristicMode, RoutingAStarActions, a_star_core,
};
use crate::simulation::replanning::routing::alt_landmark_data::AltLandmarkData;
use crate::simulation::replanning::routing::cost::{Disutility, TravelDisutility, TravelTime};
use crate::simulation::replanning::routing::graph::{GraphError, IndexableGraph, LinkIndex};
use crate::simulation::replanning::routing::least_cost_path_calculator::{
    LeastCostPath, LeastCostPathCalculator, LeastCostPathRequest,
};
use crate::simulation::replanning::routing::network_converter::{
    convert_network_for_mode, convert_network_with_modes,
};
use crate::simulation::scenario::network::{Link, Network, Node};
use nohash_hasher::IntMap;
use std::sync::Arc;
use tracing::{error, warn};

/// A heuristic to be used in A*. Given a from and to-node, estimates the disutility between them.
/// Is not allowed to overestimate disutilities. It is expected of implementations to respect this.
/// Heuristics are in general only valid for the graph they have been created for. Therefore, the
/// estimate method does not take a graph as input, but only a to- and from-node.
pub trait AStarHeuristic: Send + Sync {
    /// Estimate travel disutility between from-node and to-node. Never overestimates the
    /// disutility.
    fn estimate(&self, from: Id<Node>, to: Id<Node>) -> Disutility;
    /// Constructor for a heuristic for a given graph using a given travel disutility function as
    /// cost.
    /// Precalculates any data needed to estimate disutilities between nodes, such as landmark data
    /// for the ALT heuristic.
    fn create(
        graph: &dyn IndexableGraph,
        disutility: &dyn TravelDisutility,
    ) -> Result<Self, GraphError>
    where
        Self: Sized;
}

/// Zero heuristic estimates all disutilities to be zero. With this, the A* collapses into Dijkstra.
#[derive(Clone)]
pub struct ZeroHeuristic;

impl AStarHeuristic for ZeroHeuristic {
    fn estimate(&self, _from: Id<Node>, _to: Id<Node>) -> Disutility {
        0.
    }
    fn create(
        _graph: &dyn IndexableGraph,
        _disutility: &dyn TravelDisutility,
    ) -> Result<Self, GraphError> {
        Ok(Self {})
    }
}

/// Heuristic that uses landmarks and triangle inequality to estimate disutility between two nodes
// #[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct AltHeuristic {
    landmark_data: AltLandmarkData,
}

impl AltHeuristic {
    /// Create ALT heuristic based on a given graph and travel disutility function, by initializing
    /// landmarks and calculating the landmark data for them on the graph
    pub(crate) fn from_graph(
        graph: &dyn IndexableGraph,
        disutility: &dyn TravelDisutility,
    ) -> Result<Self, GraphError> {
        // calculate landmark data for the graph
        let landmark_data = AltLandmarkData::from_graph(graph, disutility)?;

        Ok(AltHeuristic { landmark_data })
    }
}

impl AStarHeuristic for AltHeuristic {
    /// Estimate the disutility between the from- and to-node using the ALT heuristic.
    /// Uses landmarks and triangle inequality to compute a lower bound on travel disutility.
    fn estimate(&self, from: Id<Node>, to: Id<Node>) -> Disutility {
        /* The ALT algorithm uses two lower bounds for each Landmark:
         * given: source node S, target node T, landmark L
         * then, due to the triangle inequality:
         *  1) ST + TL >= SL --> ST >= SL - TL (forward estimate)
         *  2) LS + ST >= LT --> ST >= LT - LS (backward estimate)
         * The algorithm is interested in the largest possible value of (SL-TL) and (LT-LS),
         * as this gives the closest approximation for the minimal travel disutility required to
         * go from S to T.
         */

        let from_idx = self.landmark_data.node_id_to_idx()[&from];
        let to_idx = self.landmark_data.node_id_to_idx()[&to];

        let mut h: f64 = 0.0;
        for lm_travel_disutility in self.landmark_data.travel_disutilities_to_all().iter() {
            let from_disutility = lm_travel_disutility[from_idx]; // (SL,LS)
            let to_disutility = lm_travel_disutility[to_idx]; // (LT,TL)

            if from_disutility.0.is_finite() && to_disutility.1.is_finite() {
                h = h.max(from_disutility.0 - to_disutility.1);
            }
            if to_disutility.0.is_finite() && from_disutility.1.is_finite() {
                h = h.max(to_disutility.0 - from_disutility.1);
            }
        }

        let result: Disutility = if h < 0.0 { 0.0 } else { h };

        result
    }
    fn create(
        graph: &dyn IndexableGraph,
        disutility: &dyn TravelDisutility,
    ) -> Result<Self, GraphError> {
        Self::from_graph(graph, disutility)
    }
}

/// A* router, an implementation of the LeastCostPathCalculator trait.
/// Owns a graph on which the path is searched, and a heuristic function, a travel time and a
/// travel disutility function, which are used in the A* search.
/// The heuristic is used to estimate the remaining travel disutility to the destination, and must
/// be admissible (i.e., never overestimate the actual remaining travel disutility).
/// The travel time is used to track the arrival time at the nodes along the path, while the travel
/// disutility is used as cost, i.e., this is what the A* search minimizes.
pub struct AStar<H: AStarHeuristic> {
    graph: Box<dyn IndexableGraph>,
    heuristic: H,
    travel_time: Arc<dyn TravelTime>,
    travel_disutility: Arc<dyn TravelDisutility>,
}

pub type Dijkstra = AStar<ZeroHeuristic>;
pub type Alt = AStar<AltHeuristic>;

impl<H: AStarHeuristic> AStar<H> {
    /// create a new A* router on a given network, optionally for a specific mode using the given
    /// travel time and travel disutility functions.
    /// The heuristic is automatically initialized (i.e., required data is calculated automatically)
    pub fn new(
        network: Arc<Network>,
        mode: Option<Id<String>>,
        travel_time: Arc<dyn TravelTime>,
        travel_disutility: Arc<dyn TravelDisutility>,
    ) -> Result<Self, GraphError> {
        let graph = convert_network_for_mode(network, mode);
        // create heuristic based on the graph. For instance, calculate landmark data in the case
        // of AltHeuristic
        let heuristic = H::create(&graph, travel_disutility.as_ref())?;

        Ok(Self {
            graph: Box::new(graph),
            heuristic,
            travel_time,
            travel_disutility,
        })
    }

    /// Create new A* routers for a given network, for a list of modes, using the same travel time
    /// and disutility functions for each.
    /// If a GraphError occurs when creating the router for one of the modes, returns GraphError,
    /// i.e., the routers for any other modes are discarded.
    pub fn new_for_modes(
        network: Arc<Network>,
        modes: &Vec<Id<String>>,
        travel_time: Arc<dyn TravelTime>,
        travel_disutility: Arc<dyn TravelDisutility>,
    ) -> Result<IntMap<Id<String>, Self>, GraphError> {
        let graphs = convert_network_with_modes(network, modes);

        graphs
            .into_iter()
            .try_fold(IntMap::default(), |mut map, (mode, graph)| {
                let heuristic = H::create(&graph, travel_disutility.as_ref())?;
                map.insert(
                    mode,
                    Self {
                        graph: Box::new(graph),
                        heuristic,
                        travel_time: travel_time.clone(),
                        travel_disutility: travel_disutility.clone(),
                    },
                );
                Ok(map)
            })
    }

    /// Given a to-link and a vector of parent links, extracts the path of links to the to-link.
    /// Uses the above extract_node_path to get the path of nodes, and then looks up the
    /// corresponding links in the graph.
    /// Calls the below `verify_path` to check correctness of the found path. Because of this, a
    /// from-link must also be given.
    fn extract_link_path(
        &self,
        to_link: Id<Link>,
        from_link: Id<Link>,
        parent_links: Vec<Option<LinkIndex>>,
    ) -> Result<Option<Vec<Id<Link>>>, GraphError> {
        // convert given "to" link id to node id, by looking for the start node of the link
        let to_node_id = self.graph.get_start_node(to_link.clone())?;
        let to_node_idx = self.graph.get_node_idx_from_id(to_node_id);

        let mut link_path = Vec::new();
        let mut current_node = to_node_idx;

        while let Some(parent_link) = parent_links[current_node] {
            // while a parent link exists, add the link id to the link path
            link_path.push(self.graph.get_link_id_from_idx(parent_link)?);
            // and set the start node of that link as current node
            current_node = self.graph.get_start_node_as_idx(parent_link)?;
        }
        link_path.reverse();

        // verify the found path: if incorrect, return None instead of a path
        if !self.verify_path(&link_path, from_link, to_link)? {
            return Ok(None);
        }
        Ok(Some(link_path))
    }

    /// Given a path, graph from- and to-link, verifies that the path starts at the end node of the
    /// from-link and ends at the start node of the to-link.
    fn verify_path(
        &self,
        path: &Vec<Id<Link>>,
        from_link: Id<Link>,
        to_link: Id<Link>,
    ) -> Result<bool, GraphError> {
        let end_node_of_from_link = self.graph.get_end_node(from_link)?;
        let start_node_of_to_link = self.graph.get_start_node(to_link)?;

        let last_index = match path.len() {
            0 => return Ok(end_node_of_from_link == start_node_of_to_link),
            path_length => path_length - 1,
        };

        let first_node_of_path = self.graph.get_start_node(path[0].clone())?;
        let last_node_of_path = self.graph.get_end_node(path[last_index].clone())?;

        // verify if path starts at end node of from-link and ends at start node of to-link
        Ok(first_node_of_path == end_node_of_from_link
            && last_node_of_path == start_node_of_to_link)
    }
}

impl<H: AStarHeuristic> LeastCostPathCalculator for AStar<H> {
    fn calc_least_cost_path(&self, request: LeastCostPathRequest) -> Option<LeastCostPath> {
        // convert given "to" link id to node id, by looking for the start node of the link
        let to_node_id = match self.graph.get_start_node(request.to.clone()).ok() {
            Some(node_id) => node_id, // the link was found as expected
            None => {
                // if the to link is not in the graph, we cannot calculate a path, so return None
                warn!(
                    "To link {} not found in graph, cannot calculate path",
                    request.to
                );
                return None;
            }
        };

        // convert to-node id to node index
        let to_node_idx = self.graph.get_node_idx_from_id(to_node_id);

        // create request for a_star_core
        let a_star_request = match AStarRequestBuilder::default()
            // copies from, departure time, person, vehicle values from the lcp request.
            // The graph is required to transform the from-link to from-node, and is
            // added itself to the A* request as well
            .from_least_cost_path_request_with_graph(&request, &*self.graph)
        {
            Ok(builder) => {
                // if succesful, continue building
                builder
                    // set heuristic to the heuristic of the router
                    .heuristic_mode(HeuristicMode::with_heuristic(&self.heuristic))
                    // set AStarActions to the Routing use case
                    .options(RoutingAStarActions::new(
                        to_node_idx,
                        self.travel_time.as_ref(),
                        self.travel_disutility.as_ref(),
                        self.graph.num_nodes(),
                    ))
                    .build()
                    .unwrap()
            }
            Err(err) => {
                // else, likely the given from- or to-links do not exist
                warn!(
                    "Error building A* request from least cost path request: {}, \
                    cannot calculate path",
                    err
                );
                return None;
            }
        };

        // call a_star_core with the request, and extract the distance to the goal and the
        // parent links vector from the result
        let (optimal_disutility, associated_travel_time, parent_links) =
            match a_star_core(a_star_request) {
                // Standard case: A* returned a valid result.
                Ok(AStarCoreResult::SingleDisutilWithParents(distance, time, parent_links)) => {
                    // if the returned distance to the target is infinity or NaN, it is unreachable, so
                    // we return None
                    if distance == f64::INFINITY || distance.is_nan() {
                        warn!(
                            "To link {} is unreachable from from link {}, cannot calculate path",
                            request.to, request.from
                        );
                        return None;
                    }
                    // else, we take the found shortest "distance" as the optimal disutility
                    (distance, time, parent_links)
                }
                // Unsuccesful case: Some error occurred in A*, e.g., a given link or node was not
                // found, so we cannot calculate a path. Return None
                Err(e) => {
                    warn!("Error during A*: {} cannot calculate path.", e);
                    return None;
                }
                // Unrecoverable error: A* returned the wrong result type. This should not happen,
                // since we use the A* use case RoutingAStarActions, which always builds results
                // of type SingleDistWithParents.
                _ => panic!(
                    "A* with RoutingAStarActions should return \
                SingleDistWithParents result"
                ),
            };

        let link_path = match self.extract_link_path(request.to, request.from, parent_links) {
            Ok(Some(link_path)) => link_path, // all good, path was found
            Ok(None) => {
                // verification negative: incorrect path was found
                error!(
                    "Path search unsuccesful: A path was found, but it does not connect \
                    the given from- and to-links. Something went wrong in Dijkstra or path \
                    extraction."
                );
                return None;
            }
            // from- or to-link not found in the graph. Note: this case should never occur,
            // since an invalid from- or to-link would have been detected already during A*
            Err(e) => {
                error!(
                    "Path search unsuccessful: A path was found, but when verifying its\
                    correctness, an error occured: {}",
                    e
                );
                return None;
            }
        };

        Some(LeastCostPath {
            path: link_path,
            travel_time: associated_travel_time,
            travel_disutility: optimal_disutility,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::simulation::replanning::routing::cost::TravelTime;
    use crate::simulation::replanning::routing::cost::{
        Disutility, FreeOrMaxSpeedTravelTimeAndDisutility, FreeSpeedTravelTimeAndDisutility,
        TravelDisutility,
    };
    use crate::simulation::scenario::population::InternalPerson;

    use crate::simulation::replanning::routing::least_cost_path_calculator::LeastCostPathCalculator;

    use crate::simulation::config::{MetisOptions, PartitionMethod};
    use crate::simulation::id::Id;
    use crate::simulation::replanning::routing::a_star::{
        AStar, AStarHeuristic, Alt, AltHeuristic, Dijkstra, ZeroHeuristic,
    };
    use crate::simulation::replanning::routing::graph::tests::{
        get_triangle_test_network, net_to_graph,
    };
    use crate::simulation::replanning::routing::least_cost_path_calculator::{
        LeastCostPath, LeastCostPathRequestBuilder,
    };

    use crate::simulation::scenario::network::{Link, Network};
    use crate::simulation::scenario::vehicles::{Garage, InternalVehicle, InternalVehicleType};
    use crate::simulation::time::SimTime;
    use rayon::prelude::*;
    use std::time::Duration;

    use macros::deterministic_id_test;

    use std::path::PathBuf;
    use std::sync::Arc;

    /// Runs an A* least cost path run based on the given input and compares to expected output.
    fn calc_path_and_check<H: AStarHeuristic>(
        path_calculator: &AStar<H>,
        from: &str,
        to: &str,
        vehicle: Option<&InternalVehicle>,
        expected_travel_time: Option<Duration>,
        expexted_travel_disutility: Option<Disutility>,
        expected_path: Option<Vec<&str>>,
    ) {
        let request = LeastCostPathRequestBuilder::default()
            .from(Id::create(from))
            .to(Id::create(to))
            .vehicle(vehicle)
            .build()
            .unwrap();

        let result = path_calculator.calc_least_cost_path(request);
        let expected_result = match (
            expected_travel_time,
            expexted_travel_disutility,
            expected_path,
        ) {
            (Some(tt), Some(td), Some(expected_path)) => Some(LeastCostPath {
                travel_time: tt,
                travel_disutility: td,
                path: expected_path
                    .iter()
                    .map(|link_id_str| Id::create(link_id_str))
                    .collect(),
            }),
            (None, None, None) => None,
            _ => panic!(
                "Expected travel time, expected travel disutility and expected path \
            should either all be None or both be Some"
            ),
        };

        assert_eq!(result, expected_result)
    }

    /// Time-dependent travel disutility for testing.
    /// Disutility = (free or max speed) travel_time * (1 + 10 * departure time)
    /// Very fast increase in disutility with time, to ensure that we see a difference also for
    /// very short routes, such as in the triangle test graph.
    #[derive(Clone, Debug)]
    struct TimeDependentDisutility;

    impl TravelDisutility for TimeDependentDisutility {
        fn travel_disutility(
            &self,
            link: &Link,
            departure_time: SimTime,
            _person: Option<&InternalPerson>,
            vehicle: Option<&InternalVehicle>,
        ) -> Disutility {
            // Get base travel time using free or max speed
            let free_speed_calc = FreeOrMaxSpeedTravelTimeAndDisutility;
            let travel_time = free_speed_calc.travel_time(link, departure_time, None, vehicle);

            // Apply time-dependent factor: increases with time (minimal congestion at time 0)
            let time_dep_factor = 1 + 10 * departure_time.as_secs();

            (travel_time * time_dep_factor as u32).as_secs_f64()
        }
        fn get_link_min_travel_disutility(&self, link: &Link) -> Disutility {
            // Get base travel time using free or max speed
            let free_speed_calc = FreeOrMaxSpeedTravelTimeAndDisutility;
            // min travel disutility is at time 0, and coincides with the free or max speed travel
            // time, since the time dependent factor is 1 at time 0
            free_speed_calc.get_link_min_travel_disutility(link)
        }
    }

    /// simple test of Dijkstra (A* with zero heuristic) and free speed travel disutility
    #[deterministic_id_test]
    fn test_simple_dijkstra_routing() {
        let network = get_triangle_test_network();

        let travel_cost = Arc::new(FreeSpeedTravelTimeAndDisutility {});
        let router =
            Dijkstra::new(Arc::new(network), None, travel_cost.clone(), travel_cost).unwrap();

        calc_path_and_check(
            &router,
            "1",
            "2",
            None,                         // vehicle
            Some(Duration::from_secs(6)), // tt
            Some(6.0 as Disutility),      // td
            Some(vec!["4", "5"]),
        );
        calc_path_and_check(
            &router,
            "2",
            "3",
            None,                         // vehicle
            Some(Duration::from_secs(3)), // tt
            Some(3.0),                    // td
            Some(vec!["5", "1"]),
        );
        calc_path_and_check(
            &router,
            "1",
            "5",
            None,                         // vehicle
            Some(Duration::from_secs(4)), // tt
            Some(4.0 as Disutility),      // td
            Some(vec!["4"]),
        );
    }

    /// Test routing with ALT heuristic, with two different vehicle types (car and bike).
    /// The network is such that all links are available for both modes, but the modes have
    /// different max speeds and thus different travel times on the same links, and thus different
    /// optimal paths.
    #[deterministic_id_test]
    fn test_mode_alt_routing_same_graphs() {
        // load network
        let network = Network::from_file(
            "./assets/adhoc_routing/no_updates/network.xml",
            1,
            &PartitionMethod::Metis(MetisOptions::default()),
        );
        // load garage. This one only contains vehicle types, no vehicles.
        let mut garage = Garage::from_file(&PathBuf::from(
            "./assets/adhoc_routing/no_updates/vehicles.xml",
        ));

        // load ids of vehicle types into variables, for bike and car
        let bike_type_id = Id::<InternalVehicleType>::get_from_ext("bike");
        let car_type_id = Id::<InternalVehicleType>::get_from_ext("car");

        // Add vehicles for each vehicle type (since the garage file only contains vehicle types)
        garage.add_veh_by_type(
            &Id::create("bike_person"), // create some person
            &bike_type_id,              // vehicle type
        );
        garage.add_veh_by_type(&Id::create("car_person"), &car_type_id);

        // load ids of the newly created vehicles into variables
        let bike_vehicle_id = garage.veh_id(
            &Id::get_from_ext("bike_person"), // person id
            &bike_type_id,                    // vehicle type id
        );

        let car_vehicle_id = garage.veh_id(
            &Id::get_from_ext("car_person"), // person id
            &car_type_id,                    // vehicle type id
        );

        // Create ALT routers on the network for the two modes.

        // Note: in this particular network, all links can be used by car and bike, so both routers
        // are actually the same
        // So while in a normal use case, one would create two different routers, here, we
        // explicitly do not, to verify that the same router respects different travel times of
        // different modes
        let travel_cost = Arc::new(FreeOrMaxSpeedTravelTimeAndDisutility);
        let router = Alt::new(
            Arc::new(network),
            None, // mode can be set to None here, since all links in the given network allow modes bike and car.
            travel_cost.clone(),
            travel_cost,
        )
        .unwrap();

        // check routing for bike

        calc_path_and_check(
            &router,
            "link0",
            "link4",
            garage.vehicles.get(&bike_vehicle_id), // bike vehicle
            Some(Duration::from_secs(240)),
            Some(240.0 as Disutility),
            Some(vec!["link1", "link2", "link3"]),
        );

        // check routing for car

        calc_path_and_check(
            &router,
            "link0",
            "link4",
            garage.vehicles.get(&car_vehicle_id), // car vehicle
            Some(Duration::from_secs(100)),       // tt
            Some(100.0 as Disutility),            // td
            Some(vec!["link5", "link6"]),
        )
    }

    /// Test routing with ALT heuristic, with two different vehicle types (car and bike).
    /// The network is such that not all links are available for both modes, so the routers per mode
    /// use different graphs internally. We deliberately use the FreeSpeed travel disutility (not
    /// respecting max speed) to test the different travel times and optimal paths due to the
    /// differing graphs (only).
    #[deterministic_id_test]
    fn test_mode_alt_routing_different_graphs() {
        // load network
        let network = Network::from_file(
            "./assets/routing_tests/network_different_modes.xml",
            1,
            &PartitionMethod::Metis(MetisOptions::default()),
        );
        // load garage. This one only contains vehicle types, no vehicles.
        let mut garage = Garage::from_file(&PathBuf::from(
            "./assets/adhoc_routing/no_updates/vehicles.xml",
        ));

        // load ids of vehicle types and modes into variables, for bike and car
        let bike_type_id = Id::<InternalVehicleType>::get_from_ext("bike");
        let bike_mode_id = Id::<String>::get_from_ext("bike");
        let car_type_id = Id::<InternalVehicleType>::get_from_ext("car");
        let car_mode_id = Id::<String>::get_from_ext("car");

        // Add vehicles for each vehicle type (since the garage file only contains vehicle types)
        garage.add_veh_by_type(
            &Id::create("bike_person"), // create some person
            &bike_type_id,              // vehicle type
        );
        garage.add_veh_by_type(&Id::create("car_person"), &car_type_id);

        // load ids of the newly created vehicles into variables
        let bike_vehicle_id = garage.veh_id(
            &Id::get_from_ext("bike_person"), // person id
            &bike_type_id,                    // vehicle type id
        );

        let car_vehicle_id = garage.veh_id(
            &Id::get_from_ext("car_person"), // person id
            &car_type_id,                    // vehicle type id
        );

        // Create ALT routers on the network for the two modes.

        // Note: in this network, not all links can be used by both car and bike, so the two routers
        // are actually needed. This is is the usual case.
        let travel_cost = Arc::new(FreeSpeedTravelTimeAndDisutility {});
        let router_by_mode = Alt::new_for_modes(
            Arc::new(network),
            &vec![car_mode_id, bike_mode_id],
            travel_cost.clone(),
            travel_cost,
        )
        .unwrap();

        // check routing for bike

        calc_path_and_check(
            router_by_mode.get(&Id::get_from_ext("bike")).unwrap(), // bike router
            "3",
            "1",
            garage.vehicles.get(&bike_vehicle_id), // bike vehicle
            Some(Duration::from_secs(6)),
            Some(6.0 as Disutility),
            Some(vec!["4", "5"]),
        );

        // check routing for car

        calc_path_and_check(
            router_by_mode.get(&Id::get_from_ext("car")).unwrap(), // car router
            "3",
            "2", // Note: this is a different link than in the bike test above, but it starts at the same node, so the routing destination is the same
            garage.vehicles.get(&car_vehicle_id), // car vehicle
            Some(Duration::from_secs(5)), // tt
            Some(5.0 as Disutility), // td
            Some(vec!["7"]),
        )
    }

    /// Test that ALT heuristic and zero heuristic find the same optimal path
    #[deterministic_id_test]
    fn test_alt_vs_zero_heuristic_same_result() {
        let network = Arc::new(get_triangle_test_network());

        // Router with zero heuristic (pure Dijkstra)
        let travel_cost = Arc::new(FreeOrMaxSpeedTravelTimeAndDisutility);

        let zero_router = Dijkstra::new(
            network.clone(),
            None, // mode
            travel_cost.clone(),
            travel_cost.clone(),
        )
        .unwrap();

        // Router with ALT heuristic
        let alt_router = AStar::<AltHeuristic>::new(
            network.clone(),
            None, // mode
            travel_cost.clone(),
            travel_cost,
        )
        .unwrap();

        // Both should find the same optimal path
        let request = LeastCostPathRequestBuilder::default()
            .from(Id::get_from_ext("1"))
            .to(Id::get_from_ext("2"))
            .build()
            .unwrap();

        let zero_result = zero_router.calc_least_cost_path(request.clone());
        let alt_result = alt_router.calc_least_cost_path(request);

        assert_eq!(
            zero_result, alt_result,
            "ALT and ZeroHeuristic should find the same optimal path"
        );
    }

    /// Test that one shared ALT router can serve multiple route requests in parallel.
    #[deterministic_id_test]
    fn test_shared_alt_router_parallel_routes() {
        let network = Arc::new(get_triangle_test_network());
        let travel_cost = Arc::new(FreeOrMaxSpeedTravelTimeAndDisutility);
        let router = Arc::new(Alt::new(network, None, travel_cost.clone(), travel_cost).unwrap());

        let requests = vec![
            LeastCostPathRequestBuilder::default()
                .from(Id::get_from_ext("1"))
                .to(Id::get_from_ext("2"))
                .build()
                .unwrap(),
            LeastCostPathRequestBuilder::default()
                .from(Id::get_from_ext("2"))
                .to(Id::get_from_ext("3"))
                .build()
                .unwrap(),
            LeastCostPathRequestBuilder::default()
                .from(Id::get_from_ext("1"))
                .to(Id::get_from_ext("5"))
                .build()
                .unwrap(),
            LeastCostPathRequestBuilder::default()
                .from(Id::get_from_ext("1"))
                .to(Id::get_from_ext("4"))
                .build()
                .unwrap(),
        ];

        let sequential_results = requests
            .iter()
            .cloned()
            .map(|request| router.calc_least_cost_path(request))
            .collect::<Vec<_>>();

        let parallel_results = requests
            .par_iter()
            .cloned()
            .map(|request| router.calc_least_cost_path(request))
            .collect::<Vec<_>>();

        assert_eq!(parallel_results, sequential_results);
    }

    /// Test routing when start and destination are the same (zero distance)
    #[deterministic_id_test]
    fn test_same_start_and_destination() {
        let network = get_triangle_test_network();

        let travel_cost = Arc::new(FreeOrMaxSpeedTravelTimeAndDisutility);
        let router =
            Dijkstra::new(Arc::new(network), None, travel_cost.clone(), travel_cost).unwrap();

        let request = LeastCostPathRequestBuilder::default()
            .from(Id::get_from_ext("1")) // link 1 ends in node 2
            .to(Id::get_from_ext("4")) // link 4 starts in node 2
            .build()
            .unwrap();

        let result = router.calc_least_cost_path(request);

        // Route from node to itself should have zero distance and empty path, since we are routing
        // from node 2 to node 2
        assert!(result.is_some());
        let path = result.unwrap();
        assert_eq!(path.travel_time, Duration::from_secs(0));
        assert_eq!(path.travel_disutility, 0.0);
        assert!(path.path.is_empty());
    }

    /// Test time-dependent routing: when travel disutility varies with time, the returned travel
    /// disutility differs from the time-independent case, even if the travel times are the same.
    #[deterministic_id_test]
    fn test_time_dependent_routing() {
        let network = Arc::new(get_triangle_test_network());

        // Create a router with time-independent disutility
        // disutility = freespeed travel_time
        let time_independent_cost = Arc::new(FreeOrMaxSpeedTravelTimeAndDisutility);
        let router_time_indep = AStar::<ZeroHeuristic>::new(
            network.clone(),
            None, // mode
            time_independent_cost.clone(),
            time_independent_cost,
        )
        .unwrap();

        // Create a router with time-dependent disutility
        // disutility = freespeed travel_time * (1 + 10 * departure_time)
        // Note that at departure_time=0, the disutility coincides with the time-independent router
        // from above.
        // Therefore, if both routers start at the same time, if they return different disutilities,
        // this implies that time-dependent routing is working (or is at least doing something)
        let router_time_dep = Dijkstra::new(
            network.clone(),
            None,
            Arc::new(FreeOrMaxSpeedTravelTimeAndDisutility),
            Arc::new(TimeDependentDisutility),
        )
        .unwrap();

        // Route at time 0.0
        let request = LeastCostPathRequestBuilder::default()
            .from(Id::get_from_ext("1"))
            .to(Id::get_from_ext("2"))
            .departure_time(SimTime::from_secs(0))
            .build()
            .unwrap();

        let result_time_indep = router_time_indep
            .calc_least_cost_path(request.clone())
            .unwrap();
        let result_time_dep = router_time_dep.calc_least_cost_path(request).unwrap();

        let tt_time_indep = result_time_indep.travel_time;
        let tt_time_dep = result_time_dep.travel_time;

        let td_time_indep = result_time_indep.travel_disutility;
        let td_time_dep = result_time_dep.travel_disutility;

        let td_ratio = td_time_indep / td_time_dep;

        // travel times should be the same, since only the disutility is time dependent in our case
        assert_eq!(tt_time_indep, tt_time_dep);
        // travel disutilities should not be the same
        assert!(
            td_ratio < 1.0,
            "Ratio of time independent disutility to time dependent disutility should be less \
            than 1.0, since the time dependent disutility increases with time, but got {}",
            td_ratio
        );
    }

    /// Test routing with non-existing or disconnected links, should return None
    #[deterministic_id_test]
    fn test_nonexisting_or_disconnected_links() {
        let network = Network::from_file(
            "./assets/adhoc_routing/no_updates/network.xml",
            1,
            &PartitionMethod::Metis(MetisOptions::default()),
        );

        let travel_cost = Arc::new(FreeOrMaxSpeedTravelTimeAndDisutility);
        let router =
            Dijkstra::new(Arc::new(network), None, travel_cost.clone(), travel_cost).unwrap();

        // Verify the behaviour when the from-link or to-link doesn't exist, and when they exist but
        // are not connected

        let nonexisting_from_link_request = LeastCostPathRequestBuilder::default()
            .from(Id::create("link100")) // Non-existent link ID
            .to(Id::get_from_ext("link4"))
            .build()
            .unwrap();
        let nonexisting_to_link_request = LeastCostPathRequestBuilder::default()
            .from(Id::get_from_ext("link0"))
            .to(Id::create("link999")) // Non-existent link ID
            .build()
            .unwrap();
        let unreachable_request = LeastCostPathRequestBuilder::default()
            .from(Id::get_from_ext("link6"))
            .to(Id::get_from_ext("link0")) // Link is not reachable
            .build()
            .unwrap();

        for request in [
            nonexisting_from_link_request,
            nonexisting_to_link_request,
            unreachable_request,
        ]
        .iter()
        {
            let result = router.calc_least_cost_path((*request).clone());
            // In all cases, should return none
            assert!(result.is_none());
        }
    }

    /// Test that ALT heuristic provides a valid admissible lower bound
    /// (never overestimates the actual distance)
    #[deterministic_id_test]
    fn test_alt_heuristic_admissibility() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        let alt_heuristic =
            AltHeuristic::from_graph(&graph, &FreeOrMaxSpeedTravelTimeAndDisutility).unwrap();

        // Test heuristic estimates for various node pairs
        let test_pairs = vec![("1", "2"), ("2", "3"), ("1", "3"), ("2", "1")];

        // These are the true disutilities between the node pairs based on the triangle test graph
        // and free speed travel disutilities.
        let test_pair_true_disutilities_freespeed = vec![1.0, 4.0, 2.0, 6.0];

        for (i, (from_str, to_str)) in test_pairs.iter().enumerate() {
            let heuristic_estimate =
                alt_heuristic.estimate(Id::get_from_ext(from_str), Id::get_from_ext(to_str));

            // Heuristic should not be NaN
            assert!(
                !heuristic_estimate.is_nan(),
                "Heuristic estimate should not be NaN for {} to {}",
                from_str,
                to_str
            );

            // Heuristic should be non-negative
            assert!(
                heuristic_estimate >= 0.0,
                "Heuristic estimate should be non-negative for {} to {}, got {}",
                from_str,
                to_str,
                heuristic_estimate
            );

            // Heuristic must be lower or equal to the true distance
            assert!(
                heuristic_estimate <= test_pair_true_disutilities_freespeed[i],
                "Heuristic estimate should always be lower or equal to the true distance"
            );
        }
    }
}
