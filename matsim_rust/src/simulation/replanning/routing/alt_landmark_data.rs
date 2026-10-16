use crate::simulation::id::Id;
use crate::simulation::replanning::routing::a_star_core::{
    AStarBuffers, AStarCoreResult, AStarRequestBuilder, HeuristicMode, LandmarkCalcAStarActions,
    a_star_core,
};
use crate::simulation::replanning::routing::cost::{Disutility, TravelDisutility};
use crate::simulation::replanning::routing::graph::{GraphError, IndexableGraph, NodeIndex};
use crate::simulation::scenario::network::Node;
use nohash_hasher::IntMap;
use rayon::iter::IntoParallelRefIterator;
use rayon::iter::ParallelIterator;
use std::collections::VecDeque;
use std::f64;

/// Disutility data for a pair of nodes, in both forward and backward direction. For a landmark L
/// and a node n, this is (disutility from L to n, disutility from n to L).
pub type ForwardBackwardTravelDisutility = (Disutility, Disutility);

const DEFAULT_NUMBER_OF_LANDMARKS: usize = 16;

/// Landmark data to be used in ALT routing. Contains the chosen landmarks and the pre-calculated
/// disutilities from each landmark to all other nodes in the graph, for both forward and backward
/// directions.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct AltLandmarkData {
    landmarks: Vec<NodeIndex>,
    /// The disutilities of all landmarks for node 0, then for node 1, and so on, i.e., the entry
    /// for node n and the i-th landmark is at `n * landmarks.len() + i`. The heuristic reads the
    /// data of all landmarks for two nodes, which is contiguous in memory with this layout.
    travel_disutilities: Vec<ForwardBackwardTravelDisutility>,
    node_id_to_idx: IntMap<Id<Node>, NodeIndex>,
}

impl AltLandmarkData {
    /// Creates landmark data from the disutilities of each landmark to all nodes, i.e.,
    /// `travel_disutilities_to_all[i][n]` is the entry of the i-th landmark for node n.
    pub fn new(
        landmarks: Vec<NodeIndex>,
        travel_disutilities_to_all: Vec<Vec<ForwardBackwardTravelDisutility>>,
        node_id_to_idx: IntMap<Id<Node>, NodeIndex>,
    ) -> Self {
        assert_eq!(
            landmarks.len(),
            travel_disutilities_to_all.len(),
            "landmark data requires the disutilities of every landmark"
        );
        let num_nodes = travel_disutilities_to_all.first().map_or(0, Vec::len);
        assert!(
            travel_disutilities_to_all
                .iter()
                .all(|disutilities| disutilities.len() == num_nodes),
            "landmark data requires the disutilities of every landmark for all nodes"
        );
        // transpose into the node-major layout
        let travel_disutilities = (0..num_nodes)
            .flat_map(|node| {
                travel_disutilities_to_all
                    .iter()
                    .map(move |disutilities| disutilities[node])
            })
            .collect();
        Self {
            landmarks,
            travel_disutilities,
            node_id_to_idx,
        }
    }

    /// Given a graph and a disutility function, chooses landmarks (see `choose_landmarks`) and
    /// precalculates their travel disutilities to all other nodes, both forward and backward
    pub(crate) fn from_graph(
        graph: &dyn IndexableGraph,
        disutility: &dyn TravelDisutility,
    ) -> Result<Self, GraphError> {
        let number_of_landmarks = if graph.num_nodes() < DEFAULT_NUMBER_OF_LANDMARKS.pow(2) {
            (graph.num_nodes() as f64 / 16.).ceil() as usize
        } else {
            DEFAULT_NUMBER_OF_LANDMARKS
        };
        let landmarks: Vec<NodeIndex> = Self::choose_landmarks(graph, number_of_landmarks)?;
        Self::from_graph_with_landmarks(graph, disutility, landmarks)
    }

    /// Given a graph, a disutility function and landmarks, precalculates the travel disutilities
    /// of the landmarks to all other nodes, both forward and backward
    pub(crate) fn from_graph_with_landmarks(
        graph: &dyn IndexableGraph,
        disutility: &dyn TravelDisutility,
        landmarks: Vec<NodeIndex>,
    ) -> Result<Self, GraphError> {
        let travel_disutilities_to_all =
            Self::calc_all_disutilities(graph, disutility, &landmarks)?;

        Ok(Self::new(
            landmarks,
            travel_disutilities_to_all,
            // map from node ids to node indices. Required so that the landmark data is clearly
            // mapped to true nodes (not just indices)
            graph.get_node_idxs_from_ids().clone(),
        ))
    }

    /// The disutilities of all landmarks for the given node, in the order of the landmarks
    pub(crate) fn node_travel_disutilities(
        &self,
        node: NodeIndex,
    ) -> &[ForwardBackwardTravelDisutility] {
        let num_landmarks = self.landmarks.len();
        &self.travel_disutilities[node * num_landmarks..(node + 1) * num_landmarks]
    }

    /// Chooses up to `number_of_landmarks` landmarks with the farthest landmark selection of
    /// MATSim's `SpeedyALTData`: the first landmark is the first node, and every further landmark
    /// is the node with the most links on the way from the nearest landmark chosen so far.
    ///
    /// Deviations from MATSim, which chooses on a graph with only the nodes of the mode:
    /// - The first landmark is the first node with links, since the graph of a mode contains all
    ///   nodes of the network.
    /// - Of equally far nodes, the one with the smallest index is chosen, so that the choice does
    ///   not depend on a queue's internal order.
    /// - The selection stops early instead of choosing a landmark twice, if all nodes reachable
    ///   from the landmarks are landmarks.
    ///
    /// As in MATSim, nodes that are not reachable from the first landmark are never chosen.
    fn choose_landmarks(
        graph: &dyn IndexableGraph,
        number_of_landmarks: usize,
    ) -> Result<Vec<NodeIndex>, GraphError> {
        let has_links = |node: NodeIndex| {
            !graph.outgoing_edges_as_idx(node).is_empty()
                || !graph.incoming_edges_as_idx(node).is_empty()
        };
        let Some(first) = (0..graph.num_nodes()).find(|&node| has_links(node)) else {
            return Ok(Vec::new());
        };

        let mut landmarks = Vec::with_capacity(number_of_landmarks);
        landmarks.push(first);
        while landmarks.len() < number_of_landmarks {
            match Self::farthest_node(graph, &landmarks)? {
                Some(node) => landmarks.push(node),
                None => break,
            }
        }
        Ok(landmarks)
    }

    /// Breadth-first search along outgoing links, starting at all landmarks at once. Returns the
    /// node with the most links on the way from its nearest landmark, preferring the smallest
    /// index, or None if all reached nodes are landmarks.
    fn farthest_node(
        graph: &dyn IndexableGraph,
        landmarks: &[NodeIndex],
    ) -> Result<Option<NodeIndex>, GraphError> {
        let mut hops = vec![usize::MAX; graph.num_nodes()];
        let mut queue = VecDeque::new();
        for &landmark in landmarks {
            hops[landmark] = 0;
            queue.push_back(landmark);
        }

        // (hops, node) of the farthest node so far
        let mut farthest = (0, landmarks[0]);
        while let Some(node) = queue.pop_front() {
            let node_hops = hops[node];
            if node_hops > farthest.0 || (node_hops == farthest.0 && node < farthest.1) {
                farthest = (node_hops, node);
            }
            for edge in graph.outgoing_edges_as_idx(node) {
                let next = graph.get_end_node_as_idx(edge)?;
                if hops[next] == usize::MAX {
                    hops[next] = node_hops + 1;
                    queue.push_back(next);
                }
            }
        }
        Ok((farthest.0 > 0).then_some(farthest.1))
    }

    /// Calculate travel disutilities from given list of landmarks to all other nodes in the graph,
    /// both forward and backward.
    fn calc_all_disutilities(
        graph: &dyn IndexableGraph,
        disutility: &dyn TravelDisutility,
        landmarks: &[NodeIndex],
    ) -> Result<Vec<Vec<ForwardBackwardTravelDisutility>>, GraphError> {
        // for every landmark...
        landmarks
            .par_iter()
            .map(
                |landmark_node| -> Result<Vec<ForwardBackwardTravelDisutility>, GraphError> {
                    // ... calculate forward and backward disutilities to all other nodes
                    let forward_disutilities =
                        Self::disutilities_one_2_many(graph, disutility, *landmark_node, false)?;
                    let backward_disutilities =
                        Self::disutilities_one_2_many(graph, disutility, *landmark_node, true)?;

                    // collect into ForwardBackwardTravelDisutility objects
                    Ok(forward_disutilities
                        .into_iter()
                        .zip(backward_disutilities.into_iter())
                        .collect::<Vec<ForwardBackwardTravelDisutility>>())
                },
            )
            .collect()
    }

    /// Calculates the travel disutilities from one node to all other nodes in the graph using
    /// Dijkstra (or optionally, from all other nodes to one node, if backward=true).
    /// Returns a vector of disutilities, where the index corresponds to the node index in the
    /// graph.
    /// Uses the A* implementation also `a_star_core` which is also used for routing, but without
    /// heuristic.
    fn disutilities_one_2_many(
        graph: &dyn IndexableGraph,
        disutility: &dyn TravelDisutility,
        from: NodeIndex,
        backward: bool, // if true, calculates disutilities FROM all other nodes to the "from"-node
    ) -> Result<Vec<Disutility>, GraphError> {
        let a_star_request = AStarRequestBuilder::default()
            .graph(graph)
            // makes A* calculate the disutility to all other nodes, based on the MIN disutility per
            // link, without tracking parents
            .options(LandmarkCalcAStarActions::new(disutility))
            .from(from)
            // no heuristic used => A* is Dijkstra
            .heuristic_mode(HeuristicMode::without_heuristic())
            .backward(backward)
            .build()
            .unwrap();

        // fresh buffers are fine here, since landmark data is only calculated once per graph
        let disutilities_result = match a_star_core(a_star_request, &mut AStarBuffers::default()) {
            // some graph error occurred in A* (link or node not found). Return it.
            Err(e) => Err(e),
            // everything fine, A* returned a disutility vector; use it
            Ok(AStarCoreResult::DisutilityToAllWithoutParents(disutilities)) => Ok(disutilities),
            // A* returned incorrect result enum variant. Panic, since this is a programming error.
            _ => panic!(
                "A* with LandmarkCalcAStarActions should return DisutilityToAllWithoutParents \
                result."
            ),
        };

        disutilities_result
    }
}

#[cfg(test)]
mod tests {
    use crate::simulation::id::Id;
    use crate::simulation::replanning::routing::alt_landmark_data::AltLandmarkData;
    use crate::simulation::replanning::routing::cost::FreeOrMaxSpeedTravelTimeAndDisutility;
    use crate::simulation::replanning::routing::graph::IndexableGraph;
    use crate::simulation::replanning::routing::graph::tests::{
        get_triangle_test_network, net_to_graph,
    };
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Network, Node};
    use macros::deterministic_id_test;
    use nohash_hasher::IntMap;

    #[deterministic_id_test]
    fn test_landmark_choice_and_disutility_calculation() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        let alt_data =
            AltLandmarkData::from_graph(&graph, &FreeOrMaxSpeedTravelTimeAndDisutility).unwrap();

        // verify that exactly one landmark was chosen, namely the first node with links. Node 0
        // has no links.
        assert_eq!(alt_data.landmarks.len(), 1);
        assert_eq!(alt_data.landmarks[0], 1);
        // verify the travel disutilities explicitly
        assert_eq!(
            alt_data.travel_disutilities,
            vec![
                (f64::INFINITY, f64::INFINITY),
                (0.0, 0.0),
                (1.0, 6.0),
                (2.0, 2.0)
            ]
        )
    }

    /// Test that the data of all landmarks for a node is stored next to each other.
    #[test]
    fn test_node_major_layout() {
        let alt_data = AltLandmarkData::new(
            vec![0, 2],
            vec![
                vec![(0.0, 0.0), (1.0, 2.0), (3.0, 4.0)],
                vec![(5.0, 6.0), (7.0, 8.0), (0.0, 0.0)],
            ],
            IntMap::default(),
        );
        assert_eq!(
            alt_data.node_travel_disutilities(0),
            [(0.0, 0.0), (5.0, 6.0)]
        );
        assert_eq!(
            alt_data.node_travel_disutilities(1),
            [(1.0, 2.0), (7.0, 8.0)]
        );
        assert_eq!(
            alt_data.node_travel_disutilities(2),
            [(3.0, 4.0), (0.0, 0.0)]
        );
    }

    fn equil_landmarks(number_of_landmarks: usize) -> Vec<String> {
        let network = Network::from_file("./assets/equil/equil-network.xml");
        let graph = net_to_graph(&network);
        AltLandmarkData::choose_landmarks(&graph, number_of_landmarks)
            .unwrap()
            .into_iter()
            .map(|idx| {
                graph
                    .get_node_id_from_idx(idx)
                    .unwrap()
                    .external()
                    .to_string()
            })
            .collect()
    }

    /// Test the farthest landmark selection on the equil ring 1 -> 2 -> (3 to 11) -> 12 -> 13 ->
    /// 14 -> 15 -> 1. Each landmark is the node with the most links from the nearest landmark
    /// chosen before. The last landmark is the first of nodes 3 to 11, which are all two links
    /// away from node 1.
    #[deterministic_id_test]
    fn test_farthest_landmark_selection() {
        assert_eq!(equil_landmarks(6), ["1", "15", "14", "13", "12", "3"]);
        // the selection is deterministic
        assert_eq!(equil_landmarks(6), equil_landmarks(6));
    }

    /// Test that the selection stops when all nodes are landmarks, instead of choosing a landmark
    /// twice.
    #[deterministic_id_test]
    fn test_farthest_landmark_selection_stops_without_duplicates() {
        let mut landmarks = equil_landmarks(20);
        assert_eq!(landmarks.len(), 15);
        landmarks.sort();
        landmarks.dedup();
        assert_eq!(landmarks.len(), 15);
    }

    /// Test that a graph without links has no landmarks.
    #[deterministic_id_test]
    fn test_no_landmarks_without_links() {
        let mut network = Network::new();
        network.add_node(Node::new(
            Id::create("a"),
            Coordinate::new_2d(0.0, 0.0),
            0,
            1,
        ));
        let graph = net_to_graph(&network);
        let alt_data =
            AltLandmarkData::from_graph(&graph, &FreeOrMaxSpeedTravelTimeAndDisutility).unwrap();
        assert!(alt_data.landmarks.is_empty());
    }
}
