use ahash::HashMapExt;
use metis::{Graph, Idx};
use nohash_hasher::IntMap;
use std::collections::BTreeMap;
use tracing::info;

use crate::simulation::config::{EdgeWeight, MetisOptions, VertexWeight};
use crate::simulation::id::Id;
use crate::simulation::scenario::network::{Link, Network, Node};

pub fn partition(
    network: &Network,
    num_parts: u32,
    options: &MetisOptions,
) -> IntMap<Id<Node>, Idx> {
    try_partition(network, num_parts, options)
        .unwrap_or_else(|error| panic!("METIS partitioning failed: {error}"))
}

pub fn try_partition(
    network: &Network,
    num_parts: u32,
    options: &MetisOptions,
) -> Result<IntMap<Id<Node>, Idx>, String> {
    if num_parts <= 1 {
        let mut result = IntMap::new();
        for node in network.nodes() {
            result.insert(node.id.clone(), 0);
        }
        return Ok(result);
    }
    let num_parts = Idx::try_from(num_parts)
        .map_err(|_| "METIS partition count exceeds index range".to_string())?;

    let mut vwgt: Vec<Idx> = Vec::new();
    let mut nodes = network.nodes();
    nodes.sort_by_key(|node| node.id.external().to_string());
    Idx::try_from(nodes.len()).map_err(|_| "METIS node count exceeds index range".to_string())?;
    let mut metis_result: Vec<Idx> = vec![0x00; nodes.len()];

    let mut pos_by_node_id = IntMap::default();
    let mut node_id_by_pos = IntMap::default();
    for (i, node) in nodes.iter().enumerate() {
        pos_by_node_id.insert(node.id.internal(), i);
        node_id_by_pos.insert(i, node.id.internal());
    }

    info!("Converting network into Metis format");
    let (xadj, adjncy, adjwgt) = build_adjacency(network, options, &nodes, &pos_by_node_id)?;
    for node in &nodes {
        // Add vertex weights
        add_vwgt(network, options, &mut vwgt, node)?;
    }

    let ncon = if options.vertex_weight.is_empty() {
        1
    } else {
        options.vertex_weight.len() as Idx
    };
    if !vwgt.is_empty() {
        for dimension in 0..ncon as usize {
            (0..nodes.len())
                .try_fold(0i64, |total, position| {
                    total
                        .checked_add(vwgt[position * ncon as usize + dimension] as i64)
                        .filter(|total| *total <= Idx::MAX as i64)
                })
                .ok_or_else(|| "combined METIS vertex weight exceeds index range".to_string())?;
        }
    }

    info!("Calling Metis Partitioning Library");
    let mut graph = Graph::new(ncon, num_parts, &xadj, &adjncy)
        .map_err(|error| format!("failed to create graph: {error:?}"))?
        .set_option(metis::option::UFactor(options.ufactor() as Idx))
        .set_option(metis::option::Seed(4711))
        .set_option(metis::option::Contig(options.contiguous))
        .set_adjwgt(&adjwgt);

    if !vwgt.is_empty() {
        graph = graph.set_vwgt(&vwgt);
    }

    graph
        .part_kway(&mut metis_result)
        .map_err(|error| format!("METIS k-way partition failed: {error:?}"))?;

    let mut result = IntMap::default();
    for (pos, part) in metis_result.iter().enumerate() {
        let internal_node_id = node_id_by_pos
            .get(&pos)
            .expect("Something went wrong with node positions");
        let id = Id::get(*internal_node_id);
        result.insert(id, *part);
    }

    Ok(result)
}

fn build_adjacency(
    network: &Network,
    options: &MetisOptions,
    nodes: &[&Node],
    pos_by_node_id: &IntMap<u64, usize>,
) -> Result<(Vec<Idx>, Vec<Idx>, Vec<Idx>), String> {
    let mut edge_weights = BTreeMap::<(usize, usize), Idx>::new();
    for link in network.links() {
        let (from, to) = (
            *pos_by_node_id
                .get(&link.from.internal())
                .ok_or_else(|| format!("link {} has an unknown source node", link.id))?,
            *pos_by_node_id
                .get(&link.to.internal())
                .ok_or_else(|| format!("link {} has an unknown destination node", link.id))?,
        );
        if from == to {
            continue;
        }
        let edge = if from < to { (from, to) } else { (to, from) };
        let weight = edge_weights.entry(edge).or_default();
        *weight = weight
            .checked_add(get_adjwgt(options, link)?)
            .ok_or_else(|| "combined METIS edge weight exceeds index range".to_string())?;
    }

    let mut neighbors = vec![Vec::<(Idx, Idx)>::new(); nodes.len()];
    for ((from, to), weight) in edge_weights {
        let from_pos = from as Idx;
        let to_pos = to as Idx;
        neighbors[from_pos as usize].push((to_pos, weight));
        neighbors[to_pos as usize].push((from_pos, weight));
    }

    if !valid_adjwgt_total(&neighbors) {
        return Err("combined METIS adjacency weight exceeds index range".to_string());
    }

    // Stores start positions of each node in the adjacency list.
    let mut xadj = vec![0];
    let mut adjncy = Vec::new();
    let mut adjwgt = Vec::new();
    for node_neighbors in neighbors {
        for (neighbor, weight) in node_neighbors {
            adjncy.push(neighbor);
            adjwgt.push(weight);
        }
        xadj.push(
            Idx::try_from(adjncy.len())
                .map_err(|_| "METIS adjacency count exceeds index range".to_string())?,
        );
    }
    Ok((xadj, adjncy, adjwgt))
}

fn add_vwgt(
    network: &Network,
    options: &MetisOptions,
    vwgt: &mut Vec<Idx>,
    node: &Node,
) -> Result<(), String> {
    for weight in options.vertex_weight.iter() {
        match weight {
            VertexWeight::InLinkCapacity => {
                let total = node
                    .in_links
                    .iter()
                    .try_fold(0i64, |total, id| {
                        let capacity = network.get_link(id).capacity;
                        if !capacity.is_finite() || capacity < 0.0 || capacity > Idx::MAX as f64 {
                            return None;
                        }
                        total
                            .checked_add(capacity as Idx as i64)
                            .filter(|total| *total <= Idx::MAX as i64)
                    })
                    .ok_or_else(|| {
                        format!(
                            "combined METIS vertex weight exceeds index range at {}",
                            node.id
                        )
                    })?;
                vwgt.push(total as Idx);
            }
            VertexWeight::InLinkCount => {
                vwgt.push(Idx::try_from(node.in_links.len()).map_err(|_| {
                    format!(
                        "METIS incoming-link count exceeds index range at {}",
                        node.id
                    )
                })?);
            }
            VertexWeight::Constant => {
                vwgt.push(1);
            }
            VertexWeight::PreComputed => {
                vwgt.push(Idx::try_from(node.cmp_weight).map_err(|_| {
                    format!(
                        "precomputed METIS vertex weight exceeds index range at {}",
                        node.id
                    )
                })?);
            }
        }
    }
    Ok(())
}

pub(super) fn get_adjwgt(options: &MetisOptions, link: &Link) -> Result<Idx, String> {
    match options.edge_weight {
        EdgeWeight::Capacity => {
            if !(link.capacity.is_finite()
                && link.capacity >= 0.0
                && link.capacity <= Idx::MAX as f64)
            {
                return Err(format!(
                    "link {} has capacity outside the METIS integer edge-weight range",
                    link.id
                ));
            }
            Ok(link.capacity as Idx)
        }
        EdgeWeight::Constant => Ok(1),
    }
}

fn adjwgt_total(neighbors: &[Vec<(Idx, Idx)>]) -> Option<i64> {
    neighbors
        .iter()
        .flatten()
        .try_fold(0i64, |total, (_, weight)| total.checked_add(*weight as i64))
}

fn valid_adjwgt_total(neighbors: &[Vec<(Idx, Idx)>]) -> bool {
    adjwgt_total(neighbors).is_some_and(|total| total <= Idx::MAX as i64)
}

#[cfg(test)]
mod tests {
    use crate::simulation::config::{EdgeWeight, MetisOptions, PartitionMethod, VertexWeight};
    use crate::simulation::id::Id;
    use crate::simulation::network::metis_partitioning::{
        build_adjacency, partition, try_partition, valid_adjwgt_total,
    };
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Network, Node};
    use macros::deterministic_id_test;
    use metis::Idx;
    use std::collections::BTreeMap;

    #[deterministic_id_test]
    fn try_partition_returns_precomputed_vertex_weight_overflow() {
        let mut network = Network::new();
        for (name, weight) in [("too-heavy", Idx::MAX as u32 + 1), ("other", 1)] {
            let id = Id::create(name);
            network.add_node(Node::new(id, Coordinate::new_2d(0., 0.), 0, weight));
        }

        let result = try_partition(
            &network,
            2,
            &MetisOptions::default()
                .add_vertex_weight(VertexWeight::PreComputed)
                .set_contiguous(false),
        );

        assert!(
            result
                .unwrap_err()
                .contains("precomputed METIS vertex weight")
        );
    }

    #[deterministic_id_test]
    fn try_partition_returns_combined_vertex_weight_overflow() {
        let mut network = Network::new();
        for name in ["heavy-a", "heavy-b"] {
            let id = Id::create(name);
            network.add_node(Node::new(
                id,
                Coordinate::new_2d(0., 0.),
                0,
                Idx::MAX as u32,
            ));
        }

        let result = try_partition(
            &network,
            2,
            &MetisOptions::default()
                .add_vertex_weight(VertexWeight::PreComputed)
                .set_contiguous(false),
        );

        assert!(result.unwrap_err().contains("combined METIS vertex weight"));
    }

    #[deterministic_id_test]
    fn incoming_links_connect_back_to_their_source_in_metis_graph() {
        let mut net = Network::new();
        let source = Id::create("source");
        let middle = Id::create("middle");
        let sink = Id::create("sink");
        net.add_node(Node::new(source.clone(), Coordinate::new_2d(0., 0.), 0, 1));
        net.add_node(Node::new(middle.clone(), Coordinate::new_2d(1., 0.), 0, 1));
        net.add_node(Node::new(sink.clone(), Coordinate::new_2d(2., 0.), 0, 1));
        net.add_link(Link::new_with_default(
            Id::create("source-middle"),
            net.get_node(&source),
            net.get_node(&middle),
        ));
        net.add_link(Link::new_with_default(
            Id::create("middle-sink"),
            net.get_node(&middle),
            net.get_node(&sink),
        ));

        let mut nodes = net.nodes();
        nodes.sort_by_key(|node| node.id.external().to_string());
        let positions = nodes
            .iter()
            .enumerate()
            .map(|(position, node)| (node.id.internal(), position))
            .collect();
        let (xadj, adjncy, _) =
            build_adjacency(&net, &MetisOptions::default(), &nodes, &positions).unwrap();

        let middle_position = positions[&middle.internal()];
        let sink_position = positions[&sink.internal()];
        assert_eq!(xadj[sink_position], xadj[sink_position + 1] - 1);
        assert_eq!(adjncy[xadj[sink_position] as usize], middle_position as Idx);
    }

    #[deterministic_id_test]
    fn metis_adjacency_combines_parallel_links_and_drops_self_loops() {
        let mut net = Network::new();
        let a = Id::create("a");
        let b = Id::create("b");
        net.add_node(Node::new(a.clone(), Coordinate::new_2d(0., 0.), 0, 1));
        net.add_node(Node::new(b.clone(), Coordinate::new_2d(1., 0.), 0, 1));
        for id in ["ab-1", "ab-2"] {
            net.add_link(Link::new_with_default(
                Id::create(id),
                net.get_node(&a),
                net.get_node(&b),
            ));
        }
        net.add_link(Link::new_with_default(
            Id::create("ba"),
            net.get_node(&b),
            net.get_node(&a),
        ));
        net.add_link(Link::new_with_default(
            Id::create("self"),
            net.get_node(&a),
            net.get_node(&a),
        ));

        let mut nodes = net.nodes();
        nodes.sort_by_key(|node| node.id.external().to_string());
        let positions = nodes
            .iter()
            .enumerate()
            .map(|(position, node)| (node.id.internal(), position))
            .collect();
        let (xadj, adjncy, adjwgt) =
            build_adjacency(&net, &MetisOptions::default(), &nodes, &positions).unwrap();
        let a_position = positions[&a.internal()];
        let b_position = positions[&b.internal()];

        assert_eq!(1, xadj[a_position + 1] - xadj[a_position]);
        assert_eq!(1, xadj[b_position + 1] - xadj[b_position]);
        assert_eq!(b_position as Idx, adjncy[xadj[a_position] as usize]);
        assert_eq!(a_position as Idx, adjncy[xadj[b_position] as usize]);
        assert_eq!(3, adjwgt[xadj[a_position] as usize]);
        assert_eq!(3, adjwgt[xadj[b_position] as usize]);
    }

    #[deterministic_id_test]
    #[should_panic(expected = "combined METIS vertex weight exceeds index range")]
    fn rejects_vertex_weight_totals_outside_metis_index_range() {
        let mut net = Network::new();
        let first = Id::create("large-first");
        let second = Id::create("large-second");
        net.add_node(Node::new(
            first,
            Coordinate::new_2d(0., 0.),
            0,
            Idx::MAX as u32,
        ));
        net.add_node(Node::new(second, Coordinate::new_2d(1., 0.), 0, 1));
        partition(
            &net,
            2,
            &MetisOptions::default().add_vertex_weight(VertexWeight::PreComputed),
        );
    }

    #[test]
    fn rejects_doubled_total_adjacency_weight_overflow() {
        let neighbors = vec![
            vec![(1, 1_000_000_000)],
            vec![(0, 1_000_000_000), (2, 1_000_000_000)],
            vec![(1, 1_000_000_000)],
        ];
        assert!(!valid_adjwgt_total(&neighbors));
    }

    #[deterministic_id_test]
    fn try_partition_reports_invalid_and_overflowing_capacity_edge_weights() {
        let mut network = Network::new();
        let from = Id::create("edge-weight-from");
        let to = Id::create("edge-weight-to");
        network.add_node(Node::new(from.clone(), Coordinate::new_2d(0., 0.), 0, 1));
        network.add_node(Node::new(to.clone(), Coordinate::new_2d(1., 0.), 0, 1));
        let invalid_capacity_link = Id::create("edge-weight-a");
        for id in [invalid_capacity_link.clone(), Id::create("edge-weight-b")] {
            network.add_link(Link::new_with_default(
                id.clone(),
                network.get_node(&from),
                network.get_node(&to),
            ));
            network.get_link_mut(&id).capacity = Idx::MAX as f64;
        }
        let options = MetisOptions::default().set_edge_weight(EdgeWeight::Capacity);
        assert!(
            try_partition(&network, 2, &options)
                .unwrap_err()
                .contains("combined METIS edge weight")
        );

        network.get_link_mut(&invalid_capacity_link).capacity = f64::NAN;
        assert!(
            try_partition(&network, 2, &options)
                .unwrap_err()
                .contains("capacity outside the METIS integer edge-weight range")
        );
    }

    #[deterministic_id_test]
    fn simple_graph() {
        let mut net = Network::new();
        let from_id = Id::create("from");
        let to_id = Id::create("to");
        net.add_node(Node::new(from_id.clone(), Coordinate::new_2d(0., 0.), 0, 1));
        net.add_node(Node::new(to_id.clone(), Coordinate::new_2d(100., 0.), 0, 1));
        let link_id = Id::create("link");
        net.add_link(Link::new_with_default(
            link_id,
            net.get_node(&from_id),
            net.get_node(&to_id),
        ));

        for _n in 0..100 {
            let _partition_result = partition(&net, 2, &MetisOptions::default());
        }
    }

    #[deterministic_id_test]
    fn test_andorra_with_default() {
        let network = Network::from_file(
            "./assets/andorra-network.xml.gz",
            5,
            &PartitionMethod::Metis(MetisOptions::default().set_contiguous(false)),
        );
        println!("=== Default ===");
        let _node_count = node_count(&network);
        let _edge_count = edge_count(network);
    }

    #[deterministic_id_test]
    fn test_andorra_with_capacity() {
        let network = Network::from_file(
            "./assets/andorra-network.xml.gz",
            5,
            &PartitionMethod::Metis(
                MetisOptions::default()
                    .add_vertex_weight(VertexWeight::InLinkCapacity)
                    .set_imbalance_factor(0.)
                    .set_contiguous(false),
            ),
        );
        println!("=== Capacity ===");
        let _node_count = node_count(&network);
        let _edge_count = edge_count(network);
    }

    #[deterministic_id_test]
    fn test_andorra_with_inlinkcount() {
        let network = Network::from_file(
            "./assets/andorra-network.xml.gz",
            5,
            &PartitionMethod::Metis(
                MetisOptions::default()
                    .add_vertex_weight(VertexWeight::InLinkCount)
                    .set_imbalance_factor(0.)
                    .set_contiguous(false),
            ),
        );
        println!("=== InLinkCount ===");
        let _node_count = node_count(&network);
        let _edge_count = edge_count(network);
    }

    #[deterministic_id_test]
    fn test_andorra_with_inlinkcount_and_capacity() {
        let network = Network::from_file(
            "./assets/andorra-network.xml.gz",
            5,
            &PartitionMethod::Metis(
                MetisOptions::default()
                    .add_vertex_weight(VertexWeight::InLinkCapacity)
                    .add_vertex_weight(VertexWeight::InLinkCount)
                    .set_imbalance_factor(0.)
                    .set_contiguous(false),
            ),
        );
        println!("=== Capacity & InLinkCount ===");
        let _node_count = node_count(&network);
        let _edge_count = edge_count(network);
    }

    #[deterministic_id_test]
    fn test_andorra_with_vertex_constant() {
        let network = Network::from_file(
            "./assets/andorra-network.xml.gz",
            5,
            &PartitionMethod::Metis(
                MetisOptions::default()
                    .add_vertex_weight(VertexWeight::Constant)
                    .set_imbalance_factor(0.)
                    .set_contiguous(false),
            ),
        );
        println!("=== Constant Vertex ===");
        let _node_count = node_count(&network);
        let _edge_count = edge_count(network);
    }

    #[deterministic_id_test]
    fn test_andorra_with_vertex_constant_and_inlinkcount() {
        let network = Network::from_file(
            "./assets/andorra-network.xml.gz",
            5,
            &PartitionMethod::Metis(
                MetisOptions::default()
                    .add_vertex_weight(VertexWeight::Constant)
                    .add_vertex_weight(VertexWeight::InLinkCount)
                    .set_imbalance_factor(0.)
                    .set_iteration_number(100)
                    .set_contiguous(false),
            ),
        );
        println!("=== Constant Vertex & InLinkCount ===");
        let _node_count = node_count(&network);
        let _edge_count = edge_count(network);
    }

    fn node_count(network: &Network) -> BTreeMap<u32, usize> {
        let map = network.nodes().iter().map(|n| n.partition).fold(
            BTreeMap::<u32, usize>::new(),
            |mut m, x| {
                *m.entry(x).or_default() += 1;
                m
            },
        );
        println!("Node count per partition: {:?}", map);
        map
    }

    fn edge_count(network: Network) -> BTreeMap<u32, usize> {
        let map = network.links().iter().map(|l| l.partition).fold(
            BTreeMap::<u32, usize>::new(),
            |mut m, x| {
                *m.entry(x).or_default() += 1;
                m
            },
        );
        println!("Edge count per partition: {:?}", map);
        map
    }
}
