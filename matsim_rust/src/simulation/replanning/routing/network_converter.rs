use itertools::Itertools;
use nohash_hasher::IntMap;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

use crate::simulation::id::Id;
use crate::simulation::replanning::routing::graph::{
    CsrGraph, ForwardBackwardRoutingGraph, LinkIndex, NodeIndex,
};
use crate::simulation::scenario::network::{Link, Network};

#[allow(dead_code)]

/// convert a network into multiple graphs, one for each given mode, such that only the links on
/// which a mode can travel on are contained in the respective graphs
pub fn convert_network_with_modes(
    network: Arc<Network>,
    modes: &Vec<Id<String>>,
) -> IntMap<Id<String>, ForwardBackwardRoutingGraph> {
    modes
        .iter()
        .map(|mode| {
            (
                mode.clone(),
                convert_network_for_mode(network.clone(), Some(mode.clone())),
            )
        })
        .collect()
}

/// convert a given network into a (forward backward) graph. Optionally, a mode can be given, in
/// which case only the links that allow that mode will be included in the graph
pub(crate) fn convert_network_for_mode(
    network: Arc<Network>,
    mode: Option<Id<String>>,
) -> ForwardBackwardRoutingGraph {
    info!(
        "Converting network to forward backward graph for mode {:?}.",
        mode
    );

    let mut node_index_by_id = IntMap::default();
    let mut node_id_by_index = Vec::new();

    let mut forward_first_out = Vec::new();
    let mut forward_head = Vec::new();
    let mut forward_link_id_by_index = Vec::new();

    let mut backward_first_out = Vec::new();
    let mut backward_head = Vec::new();
    let mut backward_link_id_by_index = Vec::new();

    let nodes = network.get_all_nodes_sorted();
    let node_indices = nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (&node.id, i))
        .collect::<HashMap<_, _>>();

    let mut forward_links_before = 0;
    let mut backward_links_before = 0;

    for (i, node) in nodes.iter().enumerate() {
        forward_first_out.push(forward_links_before as LinkIndex);
        backward_first_out.push(backward_links_before as LinkIndex);

        // keep track of which node id has which index in first_out
        node_index_by_id.insert(node.id.clone(), i as NodeIndex);
        node_id_by_index.push(node.id.clone());

        //calculate adjacent links
        let outgoing_links = get_links(&node.out_links, &network, mode.clone());
        forward_links_before += outgoing_links.len();

        let ingoing_links = get_links(&node.in_links, &network, mode.clone());
        backward_links_before += ingoing_links.len();

        //process outgoing links
        for link in outgoing_links {
            let to_node_index = *node_indices.get(&link.to).unwrap() as NodeIndex;

            forward_head.push(to_node_index);

            forward_link_id_by_index.push(link.id.clone());
        }

        //process ingoing links
        for link in ingoing_links {
            //Watch out: This is in the backward graph
            let to_node_index = *node_indices.get(&link.from).unwrap() as NodeIndex;

            backward_head.push(to_node_index);

            backward_link_id_by_index.push(link.id.clone());
        }
    }
    forward_first_out.push(forward_head.len() as LinkIndex);
    backward_first_out.push(backward_head.len() as LinkIndex);

    let forward_link_index_by_id = forward_link_id_by_index
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), i as LinkIndex))
        .collect::<IntMap<Id<Link>, LinkIndex>>();
    let backward_link_index_by_id = backward_link_id_by_index
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), i as LinkIndex))
        .collect::<IntMap<Id<Link>, LinkIndex>>();

    let forward_graph = CsrGraph::new(forward_first_out, forward_head);
    let backward_graph = CsrGraph::new(backward_first_out, backward_head);

    info!(
        "Finished converting network to forward backward graph for mode {:?}.",
        mode
    );

    ForwardBackwardRoutingGraph::new(
        forward_graph,
        backward_graph,
        network,
        node_index_by_id,
        node_id_by_index,
        forward_link_id_by_index,
        backward_link_id_by_index,
        forward_link_index_by_id,
        backward_link_index_by_id,
    )
}

fn get_links<'net>(
    link_ids: &'net [Id<Link>],
    network: &'net Network,
    mode: Option<Id<String>>,
) -> Vec<&'net Link> {
    link_ids
        .iter()
        .sorted_by_key(|&l| l.internal())
        .map(|l| network.get_link(l))
        // if mode is None, include all links. Otherwise, only include links that allow the given mode
        .filter(|&l| mode.as_ref().map_or(true, |m| l.contains_mode(m)))
        .collect::<Vec<&Link>>()
}

#[cfg(test)]
mod test {
    use crate::simulation::config::{MetisOptions, PartitionMethod};
    use crate::simulation::id::Id;
    use crate::simulation::replanning::routing::graph::tests::{
        get_triangle_test_network, net_to_graph,
    };
    use crate::simulation::replanning::routing::network_converter;
    use crate::simulation::scenario::network::Network;
    use macros::deterministic_id_test;
    use std::sync::Arc;

    #[deterministic_id_test]
    fn test_simple_network() {
        let network = Network::from_file(
            "./assets/routing_tests/triangle-network.xml",
            1,
            &PartitionMethod::Metis(MetisOptions::default()),
        );
        let graph = network_converter::convert_network_for_mode(Arc::new(network), None);

        assert_eq!(graph.forward_first_out(), &vec![0usize, 0, 2, 4, 6]);
        assert_eq!(graph.forward_head(), &vec![2usize, 3, 2, 3, 1, 2]);
        assert_eq!(graph.forward_link_ids().len(), 6);

        assert_eq!(graph.backward_first_out(), &vec![0usize, 0, 1, 4, 6]);
        assert_eq!(graph.backward_head(), &vec![3usize, 1, 2, 3, 1, 2]);
        assert_eq!(graph.backward_link_ids().len(), 6);
    }

    /// test whether the network converter correctly creates separate graphs for different modes,
    /// and only includes links that allow the respective mode in each graph
    #[deterministic_id_test]
    fn test_simple_network_with_modes() {
        let network = Network::from_file(
            "./assets/routing_tests/network_different_modes.xml",
            1,
            &PartitionMethod::Metis(MetisOptions::default()),
        );

        let car_mode_id = Id::<String>::get_from_ext("car");
        let bike_mode_id = Id::<String>::get_from_ext("bike");

        // create graphs based on the given network based on the given nodes
        let mut graph_by_mode = network_converter::convert_network_with_modes(
            Arc::new(network),
            &vec![car_mode_id.clone(), bike_mode_id.clone()],
        );

        assert_eq!(graph_by_mode.keys().len(), 2);

        let car_graph = graph_by_mode.remove(&car_mode_id.clone()).unwrap();
        assert_eq!(car_graph.forward_first_out(), &vec![0, 0, 1, 3, 4]);
        assert_eq!(car_graph.forward_head(), &vec![3, 2, 1, 2]);
        assert_eq!(car_graph.forward_link_ids().len(), 4);

        let bike_graph = graph_by_mode.remove(&bike_mode_id.clone()).unwrap();
        assert_eq!(bike_graph.forward_first_out(), &vec![0, 0, 1, 3, 4]);
        assert_eq!(bike_graph.forward_head(), &vec![2, 2, 3, 1]);
        assert_eq!(bike_graph.forward_link_ids().len(), 4);
    }

    /// Test that all links exist in both forward and backward directions
    #[deterministic_id_test]
    fn test_all_links_in_both_directions() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        // Every link should exist in forward_link_ids
        // and also in backward_link_ids (possibly at different position)
        let forward_links = graph.forward_link_ids();
        let backward_links = graph.backward_link_ids();

        assert_eq!(
            forward_links.len(),
            backward_links.len(),
            "Should have same number of links in both directions"
        );

        for forward_link in forward_links {
            let found_in_backward = backward_links.iter().any(|bl| bl == forward_link);
            assert!(
                found_in_backward,
                "Every forward link should exist in backward links"
            );
        }
    }
}
