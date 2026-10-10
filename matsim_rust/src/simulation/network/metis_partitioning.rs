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
    if num_parts <= 1 {
        let mut result = IntMap::new();
        for node in network.nodes() {
            result.insert(node.id.clone(), 0);
        }
        return result;
    }

    let mut vwgt: Vec<Idx> = Vec::new();
    let mut nodes = network.nodes();
    nodes.sort_by_key(|node| node.id.external().to_string());
    let mut metis_result: Vec<Idx> = vec![0x00; nodes.len()];

    let mut pos_by_node_id = IntMap::default();
    let mut node_id_by_pos = IntMap::default();
    for (i, node) in nodes.iter().enumerate() {
        pos_by_node_id.insert(node.id.internal(), i);
        node_id_by_pos.insert(i, node.id.internal());
    }

    info!("Converting network into Metis format");
    let (xadj, adjncy, adjwgt) = build_adjacency(network, options, &nodes, &pos_by_node_id);
    for node in &nodes {
        // Add vertex weights
        add_vwgt(network, options, &mut vwgt, node);
    }

    let ncon = if options.vertex_weight.is_empty() {
        1
    } else {
        options.vertex_weight.len() as Idx
    };

    info!("Calling Metis Partitioning Library");
    let mut graph = Graph::new(ncon, num_parts as Idx, &xadj, &adjncy)
        .expect("Failed to create graph")
        .set_option(metis::option::UFactor(options.ufactor() as Idx))
        .set_option(metis::option::Seed(4711))
        .set_option(metis::option::Contig(options.contiguous))
        .set_adjwgt(&adjwgt);

    if !vwgt.is_empty() {
        graph = graph.set_vwgt(&vwgt);
    }

    graph.part_kway(&mut metis_result).unwrap();

    let mut result = IntMap::default();
    for (pos, part) in metis_result.iter().enumerate() {
        let internal_node_id = node_id_by_pos
            .get(&pos)
            .expect("Something went wrong with node positions");
        let id = Id::get(*internal_node_id);
        result.insert(id, *part);
    }

    result
}

fn build_adjacency(
    network: &Network,
    options: &MetisOptions,
    nodes: &[&Node],
    pos_by_node_id: &IntMap<u64, usize>,
) -> (Vec<Idx>, Vec<Idx>, Vec<Idx>) {
    let mut edge_weights = BTreeMap::<(usize, usize), Idx>::new();
    for link in network.links() {
        let (from, to) = (
            *pos_by_node_id.get(&link.from.internal()).unwrap(),
            *pos_by_node_id.get(&link.to.internal()).unwrap(),
        );
        if from == to {
            continue;
        }
        let edge = if from < to { (from, to) } else { (to, from) };
        let weight = edge_weights.entry(edge).or_default();
        *weight = weight
            .checked_add(get_adjwgt(options, link) as Idx)
            .expect("combined METIS edge weight exceeds index range");
    }

    let mut neighbors = vec![Vec::<(Idx, Idx)>::new(); nodes.len()];
    for ((from, to), weight) in edge_weights {
        let from_pos = from as Idx;
        let to_pos = to as Idx;
        neighbors[from_pos as usize].push((to_pos, weight));
        neighbors[to_pos as usize].push((from_pos, weight));
    }

    let mut xadj = vec![0];
    let mut adjncy = Vec::new();
    let mut adjwgt = Vec::new();
    for node_neighbors in neighbors {
        for (neighbor, weight) in node_neighbors {
            adjncy.push(neighbor);
            adjwgt.push(weight);
        }
        xadj.push(adjncy.len() as Idx);
    }
    (xadj, adjncy, adjwgt)
}

fn add_vwgt(network: &Network, options: &MetisOptions, vwgt: &mut Vec<Idx>, node: &Node) {
    for weight in options.vertex_weight.iter() {
        match weight {
            VertexWeight::InLinkCapacity => {
                vwgt.push(
                    node.in_links
                        .iter()
                        .map(|id| network.get_link(id).capacity as Idx)
                        .sum(),
                );
            }
            VertexWeight::InLinkCount => {
                vwgt.push(node.in_links.len() as Idx);
            }
            VertexWeight::Constant => {
                vwgt.push(1);
            }
            VertexWeight::PreComputed => {
                vwgt.push(node.cmp_weight as Idx);
            }
        }
    }
}

fn get_adjwgt(options: &MetisOptions, link: &Link) -> f64 {
    match options.edge_weight {
        EdgeWeight::Capacity => link.capacity,
        EdgeWeight::Constant => 1.,
    }
}

#[cfg(test)]
mod tests {
    use crate::simulation::config::{MetisOptions, PartitionMethod, VertexWeight};
    use crate::simulation::id::Id;
    use crate::simulation::network::metis_partitioning::{build_adjacency, partition};
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Network, Node};
    use macros::deterministic_id_test;
    use metis::Idx;
    use std::collections::BTreeMap;

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
        let (xadj, adjncy, _) = build_adjacency(&net, &MetisOptions::default(), &nodes, &positions);

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
            build_adjacency(&net, &MetisOptions::default(), &nodes, &positions);
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
