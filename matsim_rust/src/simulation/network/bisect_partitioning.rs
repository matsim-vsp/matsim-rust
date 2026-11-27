use super::partitioning::partition_selected_nodes;
use crate::simulation::config::{BisectAxis, BisectOptions, NonMainModeLinks, VertexWeight};
use crate::simulation::id::Id;
use crate::simulation::scenario::network::{Link, Network, Node};
use itertools::Itertools;
use nohash_hasher::IntMap;

pub(crate) fn partition_with_bisect(
    network: &Network,
    num_parts: u32,
    options: &BisectOptions,
    non_main_mode_links: NonMainModeLinks,
    main_modes: &[String],
) -> IntMap<Id<Node>, u32> {
    partition_selected_nodes(
        network,
        num_parts,
        non_main_mode_links,
        main_modes,
        |nodes, include_link| {
            partition_subnetwork(network, nodes, include_link, num_parts, options)
        },
    )
}

fn partition_subnetwork(
    network: &Network,
    nodes: &[&Node],
    include_link: &dyn Fn(&Link) -> bool,
    num_parts: u32,
    options: &BisectOptions,
) -> IntMap<Id<Node>, u32> {
    if num_parts <= 1 || nodes.is_empty() {
        return nodes.iter().map(|node| (node.id.clone(), 0)).collect();
    }

    let num_parts = num_parts as usize;
    assert!(
        num_parts <= nodes.len(),
        "Bisect cannot split {} eligible nodes into {num_parts} partitions.",
        nodes.len()
    );
    let mut weighted_nodes: Vec<_> = nodes
        .iter()
        .map(|&node| {
            let in_links = || {
                node.in_links
                    .iter()
                    .map(|id| network.get_link(id))
                    .filter(|link| include_link(link))
            };
            let weight = match options.vertex_weight {
                VertexWeight::Constant => 1.0,
                VertexWeight::InLinkCount => in_links().count() as f64,
                // Sum in link ID order so that floating-point rounding is independent of input order.
                VertexWeight::InLinkCapacity => in_links()
                    .sorted_unstable_by_key(|link| link.id.external())
                    .map(|link| link.capacity)
                    .sum(),
                VertexWeight::PreComputed => f64::from(node.cmp_weight),
            };
            (node, weight)
        })
        .collect();
    let mut partitions = IntMap::with_capacity_and_hasher(nodes.len(), Default::default());
    bisect(
        &mut weighted_nodes,
        num_parts,
        0,
        options.axis,
        0,
        &mut partitions,
    );
    partitions
}

fn bisect(
    nodes: &mut [(&Node, f64)],
    num_parts: usize,
    first_partition: u32,
    axis: BisectAxis,
    depth: usize,
    partitions: &mut IntMap<Id<Node>, u32>,
) {
    if num_parts == 1 {
        partitions.extend(
            nodes
                .iter()
                .map(|(node, _)| (node.id.clone(), first_partition)),
        );
        return;
    }

    // Sort nodes at initial call, or at every call if alternating axes are used.
    if depth == 0 || axis == BisectAxis::Alternating {
        let coordinate = |node: &Node| {
            if depth.is_multiple_of(2) {
                // this is also used at depth 0, so the first cut is along the X axis
                node.coord.x
            } else {
                node.coord.y
            }
        };
        nodes.sort_unstable_by(|(a, _), (b, _)| {
            coordinate(a)
                .partial_cmp(&coordinate(b))
                .unwrap()
                .then_with(|| a.id.external().cmp(b.id.external()))
        });
    }

    let left_parts = num_parts / 2;
    let right_parts = num_parts - left_parts;
    let total_weight: f64 = nodes.iter().map(|(_, weight)| weight).sum();
    assert!(
        total_weight.is_finite(),
        "Bisect total vertex weight must be finite."
    );
    let target_weight = total_weight * (left_parts as f64 / num_parts as f64);

    // target_count is nodes * left_parts / num_parts. We don't use floating point, so we don't divide by num_parts.
    let target_count = nodes.len() as u128 * left_parts as u128;
    let mut cut = left_parts;
    let mut best_weight_error = f64::INFINITY;
    let mut best_count_error = u128::MAX;
    let mut left_weight = 0.0;

    // make sure that on the right is at least one node per partition.
    let maximum_left_node_count = nodes.len() - right_parts;
    for (index, (_, weight)) in nodes[..maximum_left_node_count].iter().enumerate() {
        left_weight += weight;
        let left_count = index + 1;

        // make sure that on the left is at least one node per partition.
        if left_count >= left_parts {
            let weight_error = (left_weight - target_weight).abs();
            let count_error = (left_count as u128 * num_parts as u128).abs_diff(target_count);

            // Update if the weight error is smaller. Use count error as a tie-breaker.
            if weight_error < best_weight_error
                || (weight_error == best_weight_error && count_error < best_count_error)
            {
                best_weight_error = weight_error;
                best_count_error = count_error;
                cut = left_count;
            }
        }
    }

    let (left, right) = nodes.split_at_mut(cut);
    bisect(
        left,
        left_parts,
        first_partition,
        axis,
        depth + 1,
        partitions,
    );
    bisect(
        right,
        right_parts,
        first_partition + left_parts as u32,
        axis,
        depth + 1,
        partitions,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::scenario::Coordinate;
    use itertools::Itertools;
    use macros::deterministic_id_test;

    fn network_with_nodes(nodes: impl IntoIterator<Item = (&'static str, f64, u32)>) -> Network {
        let mut network = Network::new();
        for (id, x, weight) in nodes {
            network.add_node(Node::new(
                Id::create(id),
                Coordinate::new_2d(x, 0.0),
                7,
                weight,
            ));
        }
        network
    }

    fn add_link(network: &mut Network, id: &str, to: &str, capacity: f64) {
        let mut link = Link::new_with_default(
            Id::create(id),
            network.get_node(&Id::get_from_ext("a")),
            network.get_node(&Id::get_from_ext(to)),
        );
        link.capacity = capacity;
        link.partition = 7;
        network.add_link(link);
    }

    fn partition(
        network: &Network,
        num_parts: u32,
        vertex_weight: VertexWeight,
    ) -> IntMap<Id<Node>, u32> {
        partition_with_bisect(
            network,
            num_parts,
            &BisectOptions {
                vertex_weight,
                ..Default::default()
            },
            NonMainModeLinks::Include,
            &[],
        )
    }

    #[deterministic_id_test]
    fn bisect_balances_each_vertex_weight() {
        let mut network =
            network_with_nodes([("a", 0., 1), ("b", 1., 1), ("c", 2., 1), ("d", 3., 7)]);
        add_link(&mut network, "aa", "a", 0.75);
        add_link(&mut network, "ab", "b", 0.125);
        add_link(&mut network, "ac", "c", 0.0625);
        for id in ["ad1", "ad2", "ad3", "ad4"] {
            add_link(&mut network, id, "d", 0.015625);
        }
        for (weight, expected) in [
            (VertexWeight::Constant, [0, 0, 1, 1]),
            (VertexWeight::InLinkCount, [0, 0, 0, 1]),
            (VertexWeight::InLinkCapacity, [0, 1, 1, 1]),
            (VertexWeight::PreComputed, [0, 0, 0, 1]),
        ] {
            let result = partition(&network, 2, weight);
            assert_eq!(
                ["a", "b", "c", "d"].map(|id| result[&Id::get_from_ext(id)]),
                expected,
                "{weight:?}",
            );
        }
    }

    #[deterministic_id_test]
    fn bisect_alternates_axes_by_recursion_depth() {
        use crate::simulation::id::reset_store;

        let mut coordinates: Vec<_> = (0..4).cartesian_product(0..4).collect();
        for _ in 0..2 {
            reset_store();
            let mut network = Network::new();
            for &(x, y) in &coordinates {
                network.add_node(Node::new(
                    Id::create(&format!("{x}-{y}")),
                    Coordinate::new_2d(f64::from(x), f64::from(y)),
                    0,
                    1,
                ));
            }
            for (axis, num_parts) in [
                (BisectAxis::X, 4),
                (BisectAxis::Alternating, 4),
                (BisectAxis::Alternating, 8),
            ] {
                let result = partition_with_bisect(
                    &network,
                    num_parts,
                    &BisectOptions {
                        axis,
                        ..Default::default()
                    },
                    NonMainModeLinks::Include,
                    &[],
                );
                for &(x, y) in &coordinates {
                    let expected = match (axis, num_parts) {
                        (BisectAxis::X, 4) => x,
                        (BisectAxis::Alternating, 4) => 2 * (x / 2) + y / 2,
                        (BisectAxis::Alternating, 8) => 4 * (x / 2) + 2 * (y / 2) + x % 2,
                        _ => unreachable!(),
                    };
                    let id = Id::get_from_ext(&format!("{x}-{y}"));
                    assert_eq!(
                        result[&id], expected,
                        "{axis:?}, {num_parts} parts, node {id}"
                    );
                }
            }
            coordinates.reverse();
        }
    }

    #[deterministic_id_test]
    fn bisect_splits_proportionally_with_odd_part_counts_and_zero_weights() {
        let network = network_with_nodes([
            ("a", 2., 0),
            ("c", 1., 0),
            ("b", 1., 0),
            ("e", 2., 0),
            ("d", 2., 0),
            ("f", 0., 0),
        ]);
        for weight in [VertexWeight::Constant, VertexWeight::PreComputed] {
            let result = partition(&network, 3, weight);
            assert_eq!(
                ["a", "b", "c", "d", "e", "f"].map(|id| result[&Id::get_from_ext(id)]),
                [1, 0, 1, 2, 2, 0],
            );
        }
    }

    #[deterministic_id_test]
    fn bisect_leaves_enough_nodes_for_every_partition() {
        let network =
            network_with_nodes([("a", 0., 0), ("b", 1., 0), ("c", 2., 100), ("d", 3., 0)]);
        let result = partition(&network, 4, VertexWeight::PreComputed);
        assert_eq!(
            ["a", "b", "c", "d"].map(|id| result[&Id::get_from_ext(id)]),
            [0, 1, 2, 3]
        );
    }

    #[deterministic_id_test]
    fn bisect_assigns_everything_to_partition_zero_for_zero_or_one_part() {
        use crate::simulation::config::{PartitionMethod, Partitioning};

        for num_parts in [0, 1] {
            let mut network = network_with_nodes([("a", 0., 1), ("b", 1., 1)]);
            add_link(&mut network, "ab", "b", 1.0);
            Network::partition_network(
                &mut network,
                &Partitioning {
                    num_parts,
                    method: PartitionMethod::Bisect(BisectOptions::default()),
                    non_main_mode_links: NonMainModeLinks::AssignAfterPartitioningToNearest,
                },
                &["car".to_owned()],
            );
            assert!(network.nodes().iter().all(|node| node.partition == 0));
            assert_eq!(network.get_link(&Id::get_from_ext("ab")).partition, 0);
        }
    }

    #[deterministic_id_test]
    fn bisect_handles_empty_networks() {
        let network = Network::new();
        for num_parts in [0, 1, 2] {
            assert!(partition(&network, num_parts, VertexWeight::Constant).is_empty());
        }
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Bisect cannot split 2 eligible nodes into 3 partitions")]
    fn bisect_rejects_more_partitions_than_eligible_nodes() {
        let network = network_with_nodes([("a", 0., 1), ("b", 1., 1)]);
        partition(&network, 3, VertexWeight::Constant);
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Bisect total vertex weight must be finite")]
    fn bisect_rejects_overflowing_total_weight() {
        let mut network = network_with_nodes([("a", 0., 1), ("b", 1., 1)]);
        add_link(&mut network, "aa", "a", f64::MAX);
        add_link(&mut network, "ab", "b", f64::MAX);
        partition(&network, 2, VertexWeight::InLinkCapacity);
    }

    #[deterministic_id_test]
    fn bisect_is_independent_of_input_order_and_format() {
        use crate::simulation::{config::PartitionMethod, id};

        let nodes = [("a", 0., 1), ("b", 0., 1), ("c", -0., 1), ("d", 1., 1)];
        let large = (1u64 << 53) as f64;
        // Adding the two unit capacities to the large one separately would round them away.
        let links = [
            ("a1-small", "a", 1.),
            ("a2-small", "a", 1.),
            ("a3-large", "a", large),
            ("b", "b", 1.),
            ("d", "d", large - 2.),
        ];
        let mut reversed = network_with_nodes(nodes.into_iter().rev());
        for (link, to, capacity) in links.into_iter().rev() {
            add_link(&mut reversed, link, to, capacity);
        }
        let mut forward = network_with_nodes(nodes);
        for (link, to, capacity) in links {
            add_link(&mut forward, link, to, capacity);
        }
        let assert_partitions = |result: IntMap<Id<Node>, u32>| {
            assert_eq!(
                ["a", "b", "c", "d"].map(|id| result[&Id::get_from_ext(id)]),
                [0, 1, 1, 1]
            );
        };
        for network in [&reversed, &forward] {
            assert_partitions(partition(network, 2, VertexWeight::InLinkCapacity));
        }

        let dir = tempfile::tempdir().unwrap();
        let xml = dir.path().join("network.xml");
        let proto = dir.path().join("network.binpb");
        let ids = dir.path().join("ids.binpb");
        forward.to_file(&xml);
        reversed.to_file(&proto);
        id::store_to_file(&ids);
        id::reset_store();
        let method = PartitionMethod::Bisect(BisectOptions {
            vertex_weight: VertexWeight::InLinkCapacity,
            ..Default::default()
        });
        let assert_file = |path| {
            let network = Network::from_file_with_partitioning(path, 2, &method);
            assert_partitions(
                network
                    .nodes()
                    .iter()
                    .map(|node| (node.id.clone(), node.partition))
                    .collect(),
            );
            assert!(
                network
                    .links()
                    .iter()
                    .all(|link| link.partition == network.get_node(&link.to).partition)
            );
        };
        assert_file(&xml);
        id::reset_store();
        id::load_from_file(&ids);
        assert_file(&proto);
    }
}
