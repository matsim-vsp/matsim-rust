use crate::simulation::config::NonMainModeLinks;
use crate::simulation::id::Id;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::network::{Link, Network, Node};
use itertools::Itertools;
use nohash_hasher::{IntMap, IntSet};
use rstar::RTree;
use rstar::primitives::GeomWithData;
use tracing::info;

/// Selects the nodes and links that contribute to a partitioning. Nodes outside the selected
/// subnetwork inherit the partition of the nearest selected node.
pub(super) fn partition_selected_nodes<P: Copy>(
    network: &Network,
    num_parts: u32,
    non_main_mode_links: NonMainModeLinks,
    main_modes: &[String],
    partition: impl FnOnce(&[&Node], &dyn Fn(&Link) -> bool) -> IntMap<Id<Node>, P>,
) -> IntMap<Id<Node>, P> {
    if num_parts <= 1 || non_main_mode_links == NonMainModeLinks::Include {
        return partition(&network.nodes(), &|_| true);
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
        return partition(&network.nodes(), &|_| true);
    }

    info!(
        "Partitioning the {} nodes incident to links with a main mode. The other {} nodes are assigned to the partition of the nearest of these nodes.",
        main_mode_nodes.len(),
        other_nodes.len()
    );
    let mut partitions = partition(&main_mode_nodes, &is_main_mode_link);
    if other_nodes.is_empty() {
        return partitions;
    }

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
