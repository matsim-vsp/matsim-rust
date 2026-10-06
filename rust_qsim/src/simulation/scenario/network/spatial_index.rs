use crate::simulation::id::Id;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::network::{Link, Network};
use nohash_hasher::IntMap;
use rstar::RTree;
use rstar::primitives::{GeomWithData, Line};
use std::fmt::{Debug, Formatter};
use std::sync::OnceLock;

type IndexedLink = GeomWithData<Line<[f64; 2]>, Id<Link>>;

/// Spatial index over the link geometries of a network, used to find the nearest link of a
/// coordinate. There is one index over all links and one index per network mode containing only
/// the links that allow this mode.
///
/// Distances are computed in the x/y plane, like MATSim's network geometry; `z` is ignored.
#[derive(Debug)]
pub struct NetworkSpatialIndex {
    all_links: RTree<IndexedLink>,
    links_by_mode: IntMap<Id<String>, RTree<IndexedLink>>,
    modes: Vec<Id<String>>,
}

impl NetworkSpatialIndex {
    pub fn new(network: &Network) -> Self {
        // Sort the input, so that the tree structure does not depend on the network's map order.
        let mut links = network.links();
        links.sort_by_key(|link| link.id.internal());

        let mut modes: Vec<Id<String>> = links
            .iter()
            .flat_map(|link| link.modes.iter().cloned())
            .collect();
        modes.sort_by_key(|mode| mode.internal());
        modes.dedup();

        let to_indexed = |link: &Link| {
            let from = &network.get_node(&link.from).coord;
            let to = &network.get_node(&link.to).coord;
            GeomWithData::new(Line::new([from.x, from.y], [to.x, to.y]), link.id.clone())
        };

        let all_links = RTree::bulk_load(links.iter().map(|link| to_indexed(*link)).collect());
        let links_by_mode = modes
            .iter()
            .map(|mode| {
                let mode_links = links
                    .iter()
                    .filter(|link| link.modes.contains(mode))
                    .map(|link| to_indexed(*link))
                    .collect();
                (mode.clone(), RTree::bulk_load(mode_links))
            })
            .collect();

        Self {
            all_links,
            links_by_mode,
            modes,
        }
    }

    /// All modes that are allowed on at least one link, sorted by internal id.
    pub fn modes(&self) -> &[Id<String>] {
        &self.modes
    }

    /// Returns the link with the smallest distance between `coord` and the link's segment. With
    /// `mode`, only links allowing that mode are considered. Ties are broken by the smallest
    /// external link id. Returns `None` if there is no candidate link.
    pub fn nearest_link(&self, coord: &Coordinate, mode: Option<&Id<String>>) -> Option<Id<Link>> {
        let tree = match mode {
            Some(mode) => self.links_by_mode.get(mode)?,
            None => &self.all_links,
        };

        // The iterator yields links in ascending distance, so all links sharing the minimal
        // distance come first. Exact float equality is intended: only true ties are broken by id.
        let mut candidates = tree.nearest_neighbor_iter_with_distance_2([coord.x, coord.y]);
        let (first, min_distance_2) = candidates.next()?;
        candidates
            .take_while(|(_, distance_2)| *distance_2 == min_distance_2)
            .map(|(link, _)| &link.data)
            .chain(std::iter::once(&first.data))
            .min_by_key(|id| id.external())
            .cloned()
    }
}

/// A [`NetworkSpatialIndex`] owned by a [`Network`] and built on first use.
///
/// The index is derived from the network's nodes and links. Therefore, it is ignored when
/// comparing networks, not copied when cloning them, and reset whenever the network may change.
#[derive(Default)]
pub(super) struct LazySpatialIndex(OnceLock<NetworkSpatialIndex>);

impl LazySpatialIndex {
    pub(super) fn get_or_init(&self, network: &Network) -> &NetworkSpatialIndex {
        self.0.get_or_init(|| NetworkSpatialIndex::new(network))
    }

    pub(super) fn reset(&mut self) {
        self.0.take();
    }
}

impl Clone for LazySpatialIndex {
    fn clone(&self) -> Self {
        // The clone rebuilds its index on demand instead of copying the trees.
        Self::default()
    }
}

impl PartialEq for LazySpatialIndex {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Debug for LazySpatialIndex {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazySpatialIndex")
            .field("initialized", &self.0.get().is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::NetworkSpatialIndex;
    use crate::simulation::id::Id;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Network, Node};
    use macros::deterministic_id_test;
    use nohash_hasher::IntSet;

    fn add_link(network: &mut Network, id: &str, from: &str, to: &str, modes: &[&str]) {
        let modes: IntSet<Id<String>> = modes.iter().map(|mode| Id::create(mode)).collect();
        network.add_link(Link::new(
            Id::create(id),
            Id::get_from_ext(from),
            Id::get_from_ext(to),
            10.0,
            1.0,
            1.0,
            1.0,
            modes,
            0,
        ));
    }

    fn add_node(network: &mut Network, id: &str, x: f64, y: f64) {
        network.add_node(Node::new(Id::create(id), Coordinate::new_2d(x, y), 0, 1));
    }

    /// Two horizontal car links at y=0 and y=10 and one bike link at y=4, all spanning x=0..100.
    fn parallel_network() -> Network {
        let mut network = Network::new();
        add_node(&mut network, "a0", 0.0, 0.0);
        add_node(&mut network, "a1", 100.0, 0.0);
        add_node(&mut network, "b0", 0.0, 4.0);
        add_node(&mut network, "b1", 100.0, 4.0);
        add_node(&mut network, "c0", 0.0, 10.0);
        add_node(&mut network, "c1", 100.0, 10.0);
        add_link(&mut network, "car-low", "a0", "a1", &["car"]);
        add_link(&mut network, "bike", "b0", "b1", &["bike"]);
        add_link(&mut network, "car-high", "c0", "c1", &["car"]);
        network
    }

    #[deterministic_id_test]
    fn nearest_link_uses_segment_distance() {
        // The coordinate is far away from all nodes but close to the middle of the links.
        let network = parallel_network();
        let index = NetworkSpatialIndex::new(&network);

        assert_eq!(
            Some(Id::get_from_ext("car-high")),
            index.nearest_link(&Coordinate::new_2d(50.0, 9.0), None)
        );
        assert_eq!(
            Some(Id::get_from_ext("bike")),
            index.nearest_link(&Coordinate::new_2d(50.0, 3.0), None)
        );
    }

    #[deterministic_id_test]
    fn nearest_link_filters_by_mode() {
        let network = parallel_network();
        let index = NetworkSpatialIndex::new(&network);
        let coord = Coordinate::new_2d(50.0, 4.0);

        assert_eq!(
            Some(Id::get_from_ext("car-low")),
            index.nearest_link(&coord, Some(&Id::get_from_ext("car")))
        );
        assert_eq!(
            Some(Id::get_from_ext("bike")),
            index.nearest_link(&coord, Some(&Id::get_from_ext("bike")))
        );
        assert_eq!(
            None,
            index.nearest_link(&coord, Some(&Id::<String>::create("walk")))
        );
    }

    #[deterministic_id_test]
    fn nearest_link_breaks_ties_by_smallest_internal_id() {
        // Both links are 5 units away. The link created first has the smaller internal id.
        let mut network = Network::new();
        add_node(&mut network, "a0", 0.0, 0.0);
        add_node(&mut network, "a1", 100.0, 0.0);
        add_node(&mut network, "c0", 0.0, 10.0);
        add_node(&mut network, "c1", 100.0, 10.0);
        add_link(&mut network, "first", "c0", "c1", &["car"]);
        add_link(&mut network, "second", "a0", "a1", &["car"]);
        let index = NetworkSpatialIndex::new(&network);

        assert_eq!(
            Some(Id::get_from_ext("first")),
            index.nearest_link(&Coordinate::new_2d(50.0, 5.0), None)
        );
    }

    #[deterministic_id_test]
    fn nearest_link_ignores_z() {
        let mut network = Network::new();
        network.add_node(Node::new(
            Id::create("a0"),
            Coordinate::new_3d(0.0, 0.0, 1000.0),
            0,
            1,
        ));
        network.add_node(Node::new(
            Id::create("a1"),
            Coordinate::new_3d(100.0, 0.0, 1000.0),
            0,
            1,
        ));
        add_node(&mut network, "c0", 0.0, 10.0);
        add_node(&mut network, "c1", 100.0, 10.0);
        add_link(&mut network, "high", "a0", "a1", &["car"]);
        add_link(&mut network, "flat", "c0", "c1", &["car"]);
        let index = NetworkSpatialIndex::new(&network);

        assert_eq!(
            Some(Id::get_from_ext("high")),
            index.nearest_link(&Coordinate::new_2d(50.0, 1.0), None)
        );
    }

    #[deterministic_id_test]
    fn links_without_modes_allow_no_mode() {
        // "no-modes" at y=4.5 is the nearest link, but only when no mode is requested.
        let mut network = parallel_network();
        add_node(&mut network, "u0", 0.0, 4.5);
        add_node(&mut network, "u1", 100.0, 4.5);
        add_link(&mut network, "no-modes", "u0", "u1", &[]);
        let index = NetworkSpatialIndex::new(&network);
        let coord = Coordinate::new_2d(50.0, 5.0);

        assert_eq!(
            Some(Id::get_from_ext("no-modes")),
            index.nearest_link(&coord, None)
        );
        assert_eq!(
            Some(Id::get_from_ext("bike")),
            index.nearest_link(&coord, Some(&Id::get_from_ext("bike")))
        );
        assert_eq!(
            None,
            index.nearest_link(&coord, Some(&Id::<String>::create("walk")))
        );
    }

    #[deterministic_id_test]
    fn empty_network_has_no_nearest_link_or_modes() {
        let index = NetworkSpatialIndex::new(&Network::new());

        assert!(index.modes().is_empty());
        assert_eq!(
            None,
            index.nearest_link(&Coordinate::new_2d(0.0, 0.0), None)
        );
    }

    #[deterministic_id_test]
    fn network_builds_index_lazily_and_resets_it_on_changes() {
        let mut network = parallel_network();
        let coord = Coordinate::new_2d(50.0, 30.0);
        assert_eq!(
            Some(Id::get_from_ext("car-high")),
            network.nearest_link(&coord, None)
        );

        add_node(&mut network, "d0", 0.0, 30.0);
        add_node(&mut network, "d1", 100.0, 30.0);
        add_link(&mut network, "car-top", "d0", "d1", &["car"]);

        assert_eq!(
            Some(Id::get_from_ext("car-top")),
            network.nearest_link(&coord, None)
        );
        assert_eq!(network, network.clone());
    }

    #[deterministic_id_test]
    fn modes_are_sorted_by_internal_id() {
        let network = parallel_network();
        let index = NetworkSpatialIndex::new(&network);

        let modes: Vec<_> = index.modes().iter().map(|mode| mode.external()).collect();
        assert_eq!(vec!["car", "bike"], modes);
    }
}
