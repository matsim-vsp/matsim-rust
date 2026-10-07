use crate::simulation::id::Id;
use crate::simulation::replanning::routing::a_star_core::{
    AStarCoreResult, AStarRequestBuilder, HeuristicMode, LandmarkCalcAStarActions, a_star_core,
};
use crate::simulation::replanning::routing::cost::{Disutility, TravelDisutility};
use crate::simulation::replanning::routing::graph::{GraphError, IndexableGraph, NodeIndex};
use crate::simulation::scenario::network::Node;
use nohash_hasher::IntMap;
use rand::SeedableRng;
use rand::prelude::IteratorRandom;
use rand::rngs::StdRng;
use rayon::iter::IntoParallelRefIterator;
use rayon::iter::ParallelIterator;
use std::f64;

/// Disutility data for a pair of nodes, in both forward and backward direction.
pub type ForwardBackwardTravelDisutility = (Disutility, Disutility);

const DEFAULT_NUMBER_OF_LANDMARKS: usize = 16;

/// Landmark data to be used in ALT routing. Contains the chosen landmarks and the pre-calculated
/// disutilities from each landmark to all other nodes in the graph, for both forward and backward
/// directions.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct AltLandmarkData {
    landmarks: Vec<NodeIndex>,
    travel_disutilities_to_all: Vec<Vec<ForwardBackwardTravelDisutility>>,
    node_id_to_idx: IntMap<Id<Node>, NodeIndex>,
}

impl AltLandmarkData {
    pub fn new(
        landmarks: Vec<NodeIndex>,
        travel_disutilities_to_all: Vec<Vec<ForwardBackwardTravelDisutility>>,
        node_id_to_idx: IntMap<Id<Node>, NodeIndex>,
    ) -> Self {
        Self {
            landmarks,
            travel_disutilities_to_all,
            node_id_to_idx,
        }
    }

    /// Given a graph and a disutility function, chooses landmarks (currently randomly) and
    /// precalculates their travel disutilities to all other nodes, both forward and backward
    pub(crate) fn from_graph(
        graph: &dyn IndexableGraph,
        disutility: &dyn TravelDisutility,
    ) -> Result<Self, GraphError> {
        let landmarks: Vec<NodeIndex> = Self::choose_landmarks(graph);

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

    pub(crate) fn travel_disutilities_to_all(&self) -> &Vec<Vec<ForwardBackwardTravelDisutility>> {
        &self.travel_disutilities_to_all
    }
    pub(crate) fn node_id_to_idx(&self) -> &IntMap<Id<Node>, NodeIndex> {
        &self.node_id_to_idx
    }

    fn choose_landmarks(graph: &dyn IndexableGraph) -> Vec<NodeIndex> {
        let number_of_landmarks = if graph.num_nodes() < DEFAULT_NUMBER_OF_LANDMARKS.pow(2) {
            (graph.num_nodes() as f64 / 16.).ceil() as usize
        } else {
            DEFAULT_NUMBER_OF_LANDMARKS
        };
        //TODO do not choose random landmarks
        (0..graph.num_nodes()).sample(&mut StdRng::seed_from_u64(42), number_of_landmarks)
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

        let disutilities_result = match a_star_core(a_star_request) {
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
    use crate::simulation::replanning::routing::alt_landmark_data::AltLandmarkData;
    use crate::simulation::replanning::routing::cost::FreeOrMaxSpeedTravelTimeAndDisutility;
    use crate::simulation::replanning::routing::graph::tests::{
        get_triangle_test_network, net_to_graph,
    };

    #[test]
    fn test_landmark_choice_and_disutility_calculation() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        let alt_data =
            AltLandmarkData::from_graph(&graph, &FreeOrMaxSpeedTravelTimeAndDisutility).unwrap();

        // selection is so far random, but with fixed seed (chooses node with index 3 as landmark)

        // verify that exactly one landmark was chosen, namely node with index 3
        assert_eq!(alt_data.landmarks.len(), 1);
        assert_eq!(alt_data.landmarks[0], 3);
        // verify the travel disutilities explicitly
        assert_eq!(
            alt_data.travel_disutilities_to_all,
            vec![vec![
                (f64::INFINITY, f64::INFINITY),
                (2.0, 2.0),
                (3.0, 4.0),
                (0.0, 0.0)
            ]]
        )
    }
}
