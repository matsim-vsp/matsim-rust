use crate::simulation::config::{EdgeWeight, MetisOptions, NonMainModeLinks, VertexWeight};
use crate::simulation::id::Id;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::network::{Link, Network, Node};
use ahash::HashMapExt;
use itertools::Itertools;
use metis::{Graph, Idx};
use nohash_hasher::{IntMap, IntSet};
use rstar::RTree;
use rstar::primitives::GeomWithData;
use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use tracing::info;

/// Serializes all calls to METIS. METIS is built with GKlib's own random number generator (see
/// `[patch.crates-io]` in the workspace manifest), whose state is shared by the whole process and
/// not synchronized. Concurrent calls would race on it and make the partitions irreproducible.
static METIS_LOCK: Mutex<()> = Mutex::new(());

/// Partitions the nodes of `network` into `num_parts` parts using METIS.
///
/// METIS partitions undirected graphs. Each node becomes a vertex. Each pair of nodes connected
/// by at least one link becomes one edge, whose weight is the sum of the weights of all links
/// between both nodes in either direction (see [`EdgeWeight`]), but at least 1. Loop links, whose
/// `from` and `to` node are the same, are ignored.
pub fn partition(
    network: &Network,
    num_parts: u32,
    options: &MetisOptions,
) -> IntMap<Id<Node>, Idx> {
    partition_subnetwork(network, &network.nodes(), |_| true, num_parts, options)
}

/// Like [`partition`], but only partitions `nodes`, and only the links for which `include_link`
/// is true become edges or contribute to vertex weights. Both nodes of every included link must be
/// in `nodes`. The result contains exactly the nodes in `nodes`. Their order determines the order
/// of the METIS vertices, so it must be reproducible.
pub fn partition_subnetwork(
    network: &Network,
    nodes: &[&Node],
    include_link: impl Fn(&Link) -> bool,
    num_parts: u32,
    options: &MetisOptions,
) -> IntMap<Id<Node>, Idx> {
    if num_parts <= 1 {
        let mut result = IntMap::new();
        for node in nodes {
            result.insert(node.id.clone(), 0);
        }
        return result;
    }

    let mut metis_result: Vec<Idx> = vec![0x00; nodes.len()];

    let mut pos_by_node_id = IntMap::default();
    let mut node_id_by_pos = IntMap::default();
    for (i, node) in nodes.iter().enumerate() {
        pos_by_node_id.insert(node.id.internal(), i);
        node_id_by_pos.insert(i, node.id.internal());
    }

    info!("Converting network into Metis format");
    let metis_graph = to_metis_graph(network, nodes, &include_link, options, &pos_by_node_id);

    let ncon = if options.vertex_weight.is_empty() {
        1
    } else {
        options.vertex_weight.len() as Idx
    };

    info!("Calling Metis Partitioning Library");
    // METIS reseeds the random number generator with the seed below in every call. Holding the
    // lock until the call returns keeps other calls from drawing numbers in between.
    let _metis_guard = METIS_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut graph = Graph::new(
        ncon,
        num_parts as Idx,
        &metis_graph.xadj,
        &metis_graph.adjncy,
    )
    .expect("Failed to create graph")
    .set_option(metis::option::UFactor(options.ufactor() as Idx))
    .set_option(metis::option::Seed(4711))
    .set_option(metis::option::Contig(options.contiguous))
    .set_option(metis::option::MinConn(options.min_conn))
    .set_adjwgt(&metis_graph.adjwgt);

    if !metis_graph.vwgt.is_empty() {
        graph = graph.set_vwgt(&metis_graph.vwgt);
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

/// The network in the CSR format expected by METIS. Vertex `i` is the node at position `i` in
/// `pos_by_node_id`.
struct MetisGraph {
    //stores start positions of each node in the adjacency list
    xadj: Vec<Idx>,
    adjncy: Vec<Idx>,
    adjwgt: Vec<Idx>,
    vwgt: Vec<Idx>,
}

fn to_metis_graph(
    network: &Network,
    nodes: &[&Node],
    include_link: &impl Fn(&Link) -> bool,
    options: &MetisOptions,
    pos_by_node_id: &IntMap<u64, usize>,
) -> MetisGraph {
    let mut xadj: Vec<Idx> = Vec::from([0]);
    let mut adjncy: Vec<Idx> = Vec::new();
    let mut adjwgt: Vec<Idx> = Vec::new();
    let mut vwgt: Vec<Idx> = Vec::new();

    for node in nodes {
        // Add vertex weights
        add_vwgt(network, include_link, options, &mut vwgt, node);

        // METIS expects an undirected graph without loops and without duplicate edges, in which
        // every edge u-v is also stored as v-u with the same weight. It does not check this, and
        // invalid input makes it access memory out of bounds. Links are directed, so all links
        // between two nodes, in either direction, are merged into one edge. The weight of each
        // link is truncated to an integer before summing, so the sum is the same from both ends.
        // Loop links do not connect two nodes and are dropped. The BTreeMap orders the neighbors
        // by position, which keeps the input to METIS reproducible. Excluded links are skipped at
        // both of their nodes, so the graph stays symmetric.
        let own_pos = pos_by_node_id[&node.id.internal()];
        let mut neighbors: BTreeMap<usize, Idx> = BTreeMap::new();
        let out_neighbors = node.out_links.iter().map(|id| {
            let link = network.get_link(id);
            (&link.to, link)
        });
        let in_neighbors = node.in_links.iter().map(|id| {
            let link = network.get_link(id);
            (&link.from, link)
        });
        let included_neighbors = out_neighbors
            .chain(in_neighbors)
            .filter(|(_, link)| include_link(link));
        for (neighbor_id, link) in included_neighbors {
            let neighbor_pos = pos_by_node_id[&neighbor_id.internal()];
            if neighbor_pos != own_pos {
                let weight = neighbors.entry(neighbor_pos).or_insert(0);
                *weight = weight.saturating_add(get_adjwgt(options, link) as Idx);
            }
        }

        for (neighbor_pos, weight) in neighbors {
            adjncy.push(neighbor_pos as Idx);
            // METIS requires positive edge weights, but links with a capacity below 1 yield 0.
            adjwgt.push(weight.max(1));
        }
        xadj.push(adjncy.len() as Idx);
    }

    MetisGraph {
        xadj,
        adjncy,
        adjwgt,
        vwgt,
    }
}

fn add_vwgt(
    network: &Network,
    include_link: &impl Fn(&Link) -> bool,
    options: &MetisOptions,
    vwgt: &mut Vec<Idx>,
    node: &Node,
) {
    let in_links = || {
        node.in_links
            .iter()
            .map(|id| network.get_link(id))
            .filter(|link| include_link(link))
    };
    for weight in options.vertex_weight.iter() {
        match weight {
            VertexWeight::InLinkCapacity => {
                vwgt.push(in_links().map(|link| link.capacity as Idx).sum());
            }
            VertexWeight::InLinkCount => {
                vwgt.push(in_links().count() as Idx);
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

/// Partitions the nodes of `network` with METIS. See [`NonMainModeLinks`] for the handling of links
/// that allow none of `main_modes`.
pub fn partition_with_metis(
    network: &Network,
    num_parts: u32,
    options: &MetisOptions,
    non_main_mode_links: NonMainModeLinks,
    main_modes: &[String],
) -> IntMap<Id<Node>, Idx> {
    if num_parts <= 1 || non_main_mode_links == NonMainModeLinks::Include {
        return partition(network, num_parts, options);
    }

    // Only look up the mode ids. Creating them would change the id store. A main mode without an
    // id is allowed on no link.
    let main_modes: IntSet<Id<String>> = main_modes
        .iter()
        .filter_map(|mode| Id::try_get_from_ext(mode))
        .collect();
    let is_main_mode_link = |link: &Link| link.modes.iter().any(|mode| main_modes.contains(mode));

    let nodes = network.nodes();
    let (main_mode_nodes, other_nodes): (Vec<&Node>, Vec<&Node>) = nodes.iter().partition(|node| {
        node.in_links
            .iter()
            .chain(&node.out_links)
            .any(|id| is_main_mode_link(network.get_link(id)))
    });

    if main_mode_nodes.is_empty() {
        info!(
            "No link allows one of the main modes {:?}. Partitioning all nodes and links.",
            main_modes.iter().map(Id::external).sorted().collect_vec()
        );
        return partition(network, num_parts, options);
    }

    info!(
        "Partitioning the {} nodes incident to links with a main mode. The other {} nodes are assigned to the partition of the nearest of these nodes.",
        main_mode_nodes.len(),
        other_nodes.len()
    );
    let mut partitions = partition_subnetwork(
        network,
        &main_mode_nodes,
        is_main_mode_link,
        num_parts,
        options,
    );

    let tree = RTree::bulk_load(
        main_mode_nodes
            .iter()
            .map(|node| GeomWithData::new([node.coord.x, node.coord.y], node.id.clone()))
            .collect(),
    );
    for node in other_nodes {
        let nearest = nearest_node(&tree, &node.coord);
        let partition = partitions[&nearest];
        partitions.insert(node.id.clone(), partition);
    }
    partitions
}

/// Returns the node in `tree` with the smallest distance to `coord` in the x/y plane. Ties are
/// broken by the smallest external node id, so that the result does not depend on the order of the
/// nodes in the tree.
pub fn nearest_node(
    tree: &RTree<GeomWithData<[f64; 2], Id<Node>>>,
    coord: &Coordinate,
) -> Id<Node> {
    // The iterator yields nodes in ascending distance, so all nodes sharing the minimal distance
    // come first. Exact float equality is intended: only true ties are broken by id.
    let mut candidates = tree.nearest_neighbor_iter_with_distance_2([coord.x, coord.y]);
    let (first, min_distance_2) = candidates
        .next()
        .expect("The tree contains at least one node");
    candidates
        .take_while(|(_, distance_2)| *distance_2 == min_distance_2)
        .map(|(node, _)| &node.data)
        .chain(std::iter::once(&first.data))
        .min_by_key(|id| id.external())
        .cloned()
        .unwrap()
}

#[cfg(test)]
mod tests {
    use crate::simulation::config::{EdgeWeight, MetisOptions, PartitionMethod, VertexWeight};
    use crate::simulation::id::Id;
    use crate::simulation::network::metis_partitioning::{
        MetisGraph, partition, partition_subnetwork, to_metis_graph,
    };
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Network, Node};
    use macros::deterministic_id_test;
    use metis::Idx;
    use nohash_hasher::IntMap;
    use std::collections::{BTreeMap, BTreeSet};

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
            let partition_result = partition(&net, 2, &MetisOptions::default());
            assert_ne!(partition_result[&from_id], partition_result[&to_id]);
        }
    }

    /// Covers all cases in which links do not map one-to-one to METIS edges: a one-way link, a
    /// pair of opposite links, parallel links, a loop link, an isolated node and capacities
    /// below 1.
    #[deterministic_id_test]
    fn metis_graph_merges_links_into_undirected_edges() {
        let mut net = Network::new();
        for (i, name) in ["a", "b", "c", "d", "e"].into_iter().enumerate() {
            let coord = Coordinate::new_2d(i as f64 * 100., 0.);
            net.add_node(Node::new(Id::create(name), coord, 0, 1));
        }
        for (name, from, to, capacity) in [
            ("ab", "a", "b", 100.),
            ("ad", "a", "d", 0.5),
            ("bc", "b", "c", 300.),
            ("cb", "c", "b", 200.),
            ("cd-1", "c", "d", 50.),
            ("cd-2", "c", "d", 0.4),
            ("dd", "d", "d", 10.),
        ] {
            let mut link = Link::new_with_default(
                Id::create(name),
                net.get_node(&Id::get_from_ext(from)),
                net.get_node(&Id::get_from_ext(to)),
            );
            link.capacity = capacity;
            net.add_link(link);
        }

        let capacity = MetisOptions::default()
            .set_edge_weight(EdgeWeight::Capacity)
            .add_vertex_weight(VertexWeight::InLinkCount);
        let graph = full_metis_graph(&net, &capacity);
        assert_valid_metis_graph(&graph, 1);
        // Capacities are truncated per link before they are summed: 0.5 becomes 0 and is raised
        // to the minimum weight 1, 0.4 becomes 0 and adds nothing to the 50 of the parallel link.
        assert_eq!(
            BTreeMap::from([
                ("a", vec![("b", 100), ("d", 1)]),
                ("b", vec![("a", 100), ("c", 500)]),
                ("c", vec![("b", 500), ("d", 50)]),
                ("d", vec![("a", 1), ("c", 50)]),
                ("e", vec![]),
            ]),
            adjacency_by_name(&net.nodes(), &graph)
        );

        let constant = MetisOptions::default().set_edge_weight(EdgeWeight::Constant);
        let graph = full_metis_graph(&net, &constant);
        assert_valid_metis_graph(&graph, 0);
        assert_eq!(
            BTreeMap::from([
                ("a", vec![("b", 1), ("d", 1)]),
                ("b", vec![("a", 1), ("c", 2)]),
                ("c", vec![("b", 2), ("d", 2)]),
                ("d", vec![("a", 1), ("c", 2)]),
                ("e", vec![]),
            ]),
            adjacency_by_name(&net.nodes(), &graph)
        );
    }

    #[deterministic_id_test]
    fn metis_graph_of_andorra_is_valid() {
        let network = Network::from_file_with_partitioning(
            "./assets/andorra-network.xml.gz",
            1,
            &PartitionMethod::None,
        );
        for edge_weight in [EdgeWeight::Capacity, EdgeWeight::Constant] {
            let options = MetisOptions::default()
                .set_edge_weight(edge_weight)
                .add_vertex_weight(VertexWeight::InLinkCapacity)
                .add_vertex_weight(VertexWeight::InLinkCount);
            let graph = full_metis_graph(&network, &options);
            assert_valid_metis_graph(&graph, 2);
        }
    }

    fn andorra_network() -> Network {
        Network::from_file_with_partitioning(
            "./assets/andorra-network.xml.gz",
            1,
            &PartitionMethod::None,
        )
    }

    fn andorra_options() -> MetisOptions {
        MetisOptions::default()
            .set_edge_weight(EdgeWeight::Capacity)
            .add_vertex_weight(VertexWeight::InLinkCount)
            .set_contiguous(false)
    }

    #[deterministic_id_test]
    fn min_conn_does_not_increase_the_maximum_number_of_neighbors() {
        let network = andorra_network();
        let max_neighbors = |options: &MetisOptions| {
            let partitions = partition(&network, 16, options);
            assert_eq!(network.nodes().len(), partitions.len());
            let mut neighbors: BTreeMap<Idx, BTreeSet<Idx>> = BTreeMap::new();
            for link in network.links() {
                let (from, to) = (partitions[&link.from], partitions[&link.to]);
                if from != to {
                    neighbors.entry(from).or_default().insert(to);
                    neighbors.entry(to).or_default().insert(from);
                }
            }
            neighbors.values().map(BTreeSet::len).max().unwrap()
        };

        let without = max_neighbors(&andorra_options());
        let with = max_neighbors(&andorra_options().set_min_conn(true));
        assert!(
            with <= without,
            "max. neighbors with MinConn: {with}, without: {without}"
        );
    }

    /// The partition of each node by external id, independent of the internal ids.
    fn partitions_by_name(partitions: &IntMap<Id<Node>, Idx>) -> BTreeMap<String, Idx> {
        partitions
            .iter()
            .map(|(id, partition)| (id.external().to_string(), *partition))
            .collect()
    }

    /// METIS draws random numbers. It is built with GKlib's own random number generator and
    /// without floating-point contraction, so the same seed yields the same partitions on every
    /// platform. The expected values were computed once and must be reproduced by CI, which runs
    /// on another platform. A network as large as Andorra is needed, because the partitions of
    /// small networks hardly depend on the random numbers.
    #[deterministic_id_test]
    fn partition_is_platform_independent() {
        let network = andorra_network();
        let partitions = partitions_by_name(&partition(&network, 8, &andorra_options()));

        let mut nodes_per_partition = BTreeMap::new();
        for partition in partitions.values() {
            *nodes_per_partition.entry(*partition).or_insert(0) += 1;
        }
        // FNV-1a hash of all external ids and their partitions, ordered by external id.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for (name, partition) in &partitions {
            let bytes = name.bytes().chain([0]).chain(partition.to_le_bytes());
            for byte in bytes {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
            }
        }

        assert_eq!(
            BTreeMap::from([
                (0, 314),
                (1, 276),
                (2, 305),
                (3, 270),
                (4, 272),
                (5, 274),
                (6, 276),
                (7, 272)
            ]),
            nodes_per_partition
        );
        assert_eq!(517_327_694_060_400_677, hash);
    }

    /// METIS calls are serialized, so that concurrent calls do not draw random numbers from each
    /// other's sequence.
    #[deterministic_id_test]
    fn concurrent_partitioning_is_deterministic() {
        let network = andorra_network();
        let expected = partitions_by_name(&partition(&network, 8, &andorra_options()));

        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| partition(&network, 8, &andorra_options())))
                .collect();
            for handle in handles {
                assert_eq!(expected, partitions_by_name(&handle.join().unwrap()));
            }
        });
    }

    /// Only nodes `a` to `c` are connected by `car` links. The `bike` link `ca` connects two of
    /// them, and the `pt` links connect the stops `s1` and `s2`, which are not connected to the
    /// other nodes.
    #[deterministic_id_test]
    fn metis_graph_of_subnetwork_skips_excluded_links() {
        let mut net = Network::new();
        for (i, name) in ["a", "b", "c", "s1", "s2"].into_iter().enumerate() {
            let coord = Coordinate::new_2d(i as f64 * 100., 0.);
            net.add_node(Node::new(Id::create(name), coord, 0, 1));
        }
        for (name, from, to, mode) in [
            ("ab", "a", "b", "car"),
            ("bc", "b", "c", "car"),
            ("ca", "c", "a", "bike"),
            ("s1s1", "s1", "s1", "pt"),
            ("s1s2", "s1", "s2", "pt"),
            ("s2s2", "s2", "s2", "pt"),
        ] {
            let mut link = Link::new_with_default(
                Id::create(name),
                net.get_node(&Id::get_from_ext(from)),
                net.get_node(&Id::get_from_ext(to)),
            );
            link.modes.insert(Id::create(mode));
            net.add_link(link);
        }

        let car: Id<String> = Id::get_from_ext("car");
        let is_car_link = |link: &Link| link.contains_mode(&car);
        let nodes: Vec<&Node> = net
            .nodes()
            .into_iter()
            .filter(|node| ["a", "b", "c"].contains(&node.id.external()))
            .collect();
        let options = MetisOptions::default().add_vertex_weight(VertexWeight::InLinkCount);

        let graph = to_metis_graph(&net, &nodes, &is_car_link, &options, &positions(&nodes));
        assert_valid_metis_graph(&graph, 1);
        assert_eq!(
            BTreeMap::from([
                ("a", vec![("b", 1)]),
                ("b", vec![("a", 1), ("c", 1)]),
                ("c", vec![("b", 1)]),
            ]),
            adjacency_by_name(&nodes, &graph)
        );
        // The only incoming link of `a` is the bike link.
        let vertex_weights: BTreeMap<&str, Idx> = nodes
            .iter()
            .map(|node| node.id.external())
            .zip(graph.vwgt.iter().copied())
            .collect();
        assert_eq!(
            BTreeMap::from([("a", 0), ("b", 1), ("c", 1)]),
            vertex_weights
        );

        let partitions = partition_subnetwork(&net, &nodes, is_car_link, 2, &options);
        let mut partitioned: Vec<&str> = partitions.keys().map(|id| id.external()).collect();
        partitioned.sort();
        assert_eq!(vec!["a", "b", "c"], partitioned);
    }

    /// The METIS graph of all nodes and links, as built by [`partition`].
    fn full_metis_graph(network: &Network, options: &MetisOptions) -> MetisGraph {
        let nodes = network.nodes();
        to_metis_graph(network, &nodes, &|_| true, options, &positions(&nodes))
    }

    /// Node positions in the same order as in [`partition_subnetwork`].
    fn positions(nodes: &[&Node]) -> IntMap<u64, usize> {
        nodes
            .iter()
            .enumerate()
            .map(|(i, node)| (node.id.internal(), i))
            .collect()
    }

    /// Neighbors and edge weights of each node by external id, independent of node positions.
    fn adjacency_by_name<'a>(
        nodes: &[&'a Node],
        graph: &MetisGraph,
    ) -> BTreeMap<&'a str, Vec<(&'a str, Idx)>> {
        let name = |pos: usize| nodes[pos].id.external();
        (0..nodes.len())
            .map(|u| {
                let edges = graph.xadj[u] as usize..graph.xadj[u + 1] as usize;
                let mut neighbors: Vec<_> = edges
                    .map(|e| (name(graph.adjncy[e] as usize), graph.adjwgt[e]))
                    .collect();
                neighbors.sort();
                (name(u), neighbors)
            })
            .collect()
    }

    /// Checks the input requirements of METIS, similar to its `graphchk` program: an undirected
    /// graph without loops and without duplicate edges, in which every edge is stored in both
    /// directions with the same positive weight.
    fn assert_valid_metis_graph(graph: &MetisGraph, num_vertex_weights: usize) {
        let num_vertices = graph.xadj.len() - 1;
        assert_eq!(0, graph.xadj[0]);
        assert!(graph.xadj.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(graph.adjncy.len(), *graph.xadj.last().unwrap() as usize);
        assert_eq!(graph.adjncy.len(), graph.adjwgt.len());
        assert_eq!(num_vertex_weights * num_vertices, graph.vwgt.len());

        let mut edges = BTreeMap::new();
        for u in 0..num_vertices {
            for e in graph.xadj[u] as usize..graph.xadj[u + 1] as usize {
                let v = graph.adjncy[e] as usize;
                let weight = graph.adjwgt[e];
                assert!(v < num_vertices, "vertex {u} has neighbor {v} out of range");
                assert_ne!(u, v, "vertex {u} has a loop");
                assert!(weight > 0, "edge {u}-{v} has weight {weight}");
                assert!(
                    edges.insert((u, v), weight).is_none(),
                    "edge {u}-{v} is duplicated"
                );
            }
        }
        for (&(u, v), weight) in &edges {
            assert_eq!(
                Some(weight),
                edges.get(&(v, u)),
                "edge {u}-{v} has no reverse edge with the same weight"
            );
        }
    }

    #[deterministic_id_test]
    fn test_andorra_with_default() {
        let network = Network::from_file_with_partitioning(
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
        let network = Network::from_file_with_partitioning(
            "./assets/andorra-network.xml.gz",
            5,
            &PartitionMethod::Metis(
                MetisOptions::default()
                    .add_vertex_weight(VertexWeight::InLinkCapacity)
                    .set_imbalance_factor(1.)
                    .set_contiguous(false),
            ),
        );
        println!("=== Capacity ===");
        let _node_count = node_count(&network);
        let _edge_count = edge_count(network);
    }

    #[deterministic_id_test]
    fn test_andorra_with_inlinkcount() {
        let network = Network::from_file_with_partitioning(
            "./assets/andorra-network.xml.gz",
            5,
            &PartitionMethod::Metis(
                MetisOptions::default()
                    .add_vertex_weight(VertexWeight::InLinkCount)
                    .set_imbalance_factor(1.)
                    .set_contiguous(false),
            ),
        );
        println!("=== InLinkCount ===");
        let _node_count = node_count(&network);
        let _edge_count = edge_count(network);
    }

    #[deterministic_id_test]
    fn test_andorra_with_inlinkcount_and_capacity() {
        let network = Network::from_file_with_partitioning(
            "./assets/andorra-network.xml.gz",
            5,
            &PartitionMethod::Metis(
                MetisOptions::default()
                    .add_vertex_weight(VertexWeight::InLinkCapacity)
                    .add_vertex_weight(VertexWeight::InLinkCount)
                    .set_imbalance_factor(1.)
                    .set_contiguous(false),
            ),
        );
        println!("=== Capacity & InLinkCount ===");
        let _node_count = node_count(&network);
        let _edge_count = edge_count(network);
    }

    #[deterministic_id_test]
    fn test_andorra_with_vertex_constant() {
        let network = Network::from_file_with_partitioning(
            "./assets/andorra-network.xml.gz",
            5,
            &PartitionMethod::Metis(
                MetisOptions::default()
                    .add_vertex_weight(VertexWeight::Constant)
                    .set_imbalance_factor(1.)
                    .set_contiguous(false),
            ),
        );
        println!("=== Constant Vertex ===");
        let _node_count = node_count(&network);
        let _edge_count = edge_count(network);
    }

    #[deterministic_id_test]
    fn test_andorra_with_vertex_constant_and_inlinkcount() {
        let network = Network::from_file_with_partitioning(
            "./assets/andorra-network.xml.gz",
            5,
            &PartitionMethod::Metis(
                MetisOptions::default()
                    .add_vertex_weight(VertexWeight::Constant)
                    .add_vertex_weight(VertexWeight::InLinkCount)
                    .set_imbalance_factor(1.)
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
