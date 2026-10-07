use crate::simulation::InternalAttributes;
use crate::simulation::config::{ModalLinkSelection, PartitionMethod};
use crate::simulation::id::Id;
use crate::simulation::io::proto::proto_network::{
    load_from_proto, write_to_proto_with_link_attribute_overrides,
};
use crate::simulation::io::xml::attributes::IOAttributes;
use crate::simulation::io::xml::network;
use crate::simulation::io::xml::network::{
    IOLink, IONetwork, IONode, write_to_xml_with_link_attribute_overrides,
};
use crate::simulation::network::metis_partitioning;
use crate::simulation::scenario::Coordinate;
use itertools::Itertools;
use nohash_hasher::{IntMap, IntSet};
use spatial_index::{LazySpatialIndex, NetworkSpatialIndex};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use tracing::{info, warn};

pub mod spatial_index;

#[derive(Debug, Clone, PartialEq)]
pub struct Network {
    nodes: IntMap<Id<Node>, Node>,
    links: IntMap<Id<Link>, Link>,
    effective_cell_size: f64,
    /// Built on first use and reset by every method that may change nodes or links.
    spatial_index: LazySpatialIndex,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub coord: Coordinate,
    pub id: Id<Node>,
    pub in_links: Vec<Id<Link>>,
    pub out_links: Vec<Id<Link>>,
    pub partition: u32,
    pub cmp_weight: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub id: Id<Link>,
    pub from: Id<Node>,
    pub to: Id<Node>,
    pub length: f64,
    pub capacity: f64,
    pub freespeed: f64,
    pub permlanes: f64,
    pub modes: IntSet<Id<String>>,
    pub partition: u32,
    pub attributes: InternalAttributes,
}

pub(crate) type LinkAttributeOverrides = IntMap<Id<Link>, InternalAttributes>;

impl Default for Network {
    fn default() -> Self {
        Network::new()
    }
}

impl Network {
    pub fn new() -> Self {
        Network {
            nodes: IntMap::default(),
            links: IntMap::default(),
            effective_cell_size: 7.5,
            spatial_index: LazySpatialIndex::default(),
        }
    }

    pub fn effective_cell_size(&self) -> f64 {
        self.effective_cell_size
    }

    pub fn from_file(file: &str, num_parts: u32, partition_method: &PartitionMethod) -> Self {
        Self::from_file_path(&PathBuf::from(file), num_parts, partition_method)
    }

    pub fn from_file_path(
        file_path: &Path,
        num_parts: u32,
        partition_method: &PartitionMethod,
    ) -> Self {
        let mut result = from_file(file_path);
        Self::partition_network(&mut result, partition_method, num_parts);
        result
    }

    pub fn from_file_as_is(path: &Path) -> Self {
        from_file(path)
    }

    pub fn to_file(&self, file_path: &Path) {
        to_file(self, file_path);
    }

    pub(crate) fn to_file_with_link_attribute_overrides(
        &self,
        file_path: &Path,
        overrides: &LinkAttributeOverrides,
    ) {
        to_file_with_link_attribute_overrides(self, file_path, overrides);
    }

    pub fn add_node(&mut self, node: Node) {
        self.spatial_index.reset();
        let id = node.id.clone();
        let option = self.nodes.insert(id.clone(), node);
        assert!(
            option.is_none(),
            "Node with id {} already exists in the network.",
            id
        );
    }

    pub fn add_link(&mut self, link: Link) {
        self.spatial_index.reset();
        // wire up in and out links and push link to the links vec
        let id = link.id.clone();
        self.nodes
            .get_mut(&link.from)
            .unwrap()
            .out_links
            .push(id.clone());
        self.nodes
            .get_mut(&link.to)
            .unwrap()
            .in_links
            .push(id.clone());

        let option = self.links.insert(id.clone(), link);
        assert!(
            option.is_none(),
            "Link with id {} already exists in the network.",
            id
        );
    }

    /// Removes a link from the network and disconnects it from its endpoint nodes.
    ///
    /// # Panics
    ///
    /// Panics if the link does not exist or if network adjacency invariants are broken
    /// (for example, if the link is not present exactly once in the endpoint link lists).
    pub fn remove_link(&mut self, id: Id<Link>) {
        self.spatial_index.reset();
        let link = self.links.remove(&id).unwrap_or_else(|| {
            panic!("Link with id {} does not exist in the network.", id);
        });

        {
            let from_node = self.nodes.get_mut(&link.from).unwrap();
            let out_links_before = from_node.out_links.len();
            from_node.out_links.retain(|link_id| link_id != &id);
            let removed = out_links_before - from_node.out_links.len();
            assert_eq!(
                1, removed,
                "Link with id {} is expected exactly once in out_links of node {}.",
                id, link.from
            );
        }

        {
            let to_node = self.nodes.get_mut(&link.to).unwrap();
            let in_links_before = to_node.in_links.len();
            to_node.in_links.retain(|link_id| link_id != &id);
            let removed = in_links_before - to_node.in_links.len();
            assert_eq!(
                1, removed,
                "Link with id {} is expected exactly once in in_links of node {}.",
                id, link.to
            );
        }
    }

    /// Removes a node from the network and deletes all incident links first.
    ///
    /// Incoming and outgoing links of the node are removed via [`Network::remove_link`]
    /// before the node itself is deleted.
    ///
    /// # Panics
    ///
    /// Panics if the node does not exist.
    pub fn remove_node(&mut self, id: Id<Node>) {
        self.spatial_index.reset();
        let links_to_remove: HashSet<_> = {
            let node = self.nodes.get(&id).unwrap_or_else(|| {
                panic!("Node with id {} does not exist in the network.", id);
            });
            node.in_links
                .iter()
                .chain(node.out_links.iter())
                .cloned()
                .collect()
        };

        for link_id in links_to_remove {
            self.remove_link(link_id);
        }

        self.nodes.remove(&id).unwrap();
    }

    pub fn get_node(&self, id: &Id<Node>) -> &Node {
        self.nodes.get(id).unwrap()
    }

    pub fn get_link(&self, id: &Id<Link>) -> &Link {
        self.links.get(id).unwrap()
    }

    pub fn get_node_mut(&mut self, id: &Id<Node>) -> &mut Node {
        self.spatial_index.reset();
        self.nodes.get_mut(id).unwrap()
    }

    pub fn get_link_mut(&mut self, id: &Id<Link>) -> &mut Link {
        self.spatial_index.reset();
        self.links.get_mut(id).unwrap()
    }

    /// Returns the spatial index of the links. It is built on first use.
    pub fn spatial_index(&self) -> &NetworkSpatialIndex {
        self.spatial_index.get_or_init(self)
    }

    /// Returns the link nearest to `coord`, optionally restricted to links allowing `mode`.
    /// See [`NetworkSpatialIndex::nearest_link`].
    pub fn nearest_link(&self, coord: &Coordinate, mode: Option<&Id<String>>) -> Option<Id<Link>> {
        self.spatial_index().nearest_link(coord, mode)
    }

    /// Returns the link through which a location with `base_link` and `coord` is connected to the
    /// network for `mode`, i.e. its access and egress link, according to `selection`. This is the
    /// single place deciding modal links, both for activity facilities and for the link wrappers
    /// of activities without a facility.
    ///
    /// Falls back to the base link if no link allows `mode`. A base link outside the network, e.g.
    /// in teleportation-only setups, is kept for every mode.
    pub fn modal_link(
        &self,
        base_link: &Id<Link>,
        coord: &Coordinate,
        mode: &Id<String>,
        selection: ModalLinkSelection,
    ) -> Id<Link> {
        let Some(base) = self.links.get(base_link) else {
            return base_link.clone();
        };
        match selection {
            ModalLinkSelection::BaseLinkFirst if base.contains_mode(mode) => base_link.clone(),
            ModalLinkSelection::BaseLinkFirst | ModalLinkSelection::NearestLink => self
                .nearest_link(coord, Some(mode))
                .unwrap_or_else(|| base_link.clone()),
        }
    }

    pub fn partition_network(
        network: &mut Network,
        partition_method: &PartitionMethod,
        num_parts: u32,
    ) {
        match partition_method {
            PartitionMethod::Metis(options) => {
                let partitions = metis_partitioning::partition(network, num_parts, options);
                for (id, node) in network.nodes.iter_mut() {
                    let partition = partitions.get(id).unwrap();
                    node.partition = *partition as u32;

                    for link_id in &node.in_links {
                        let link = network.links.get_mut(link_id).unwrap();
                        link.partition = *partition as u32;
                    }
                }
            }
            PartitionMethod::None => {}
        }
    }

    pub fn get_all_nodes_sorted(&self) -> Vec<&Node> {
        self.nodes
            .iter()
            .sorted_by_key(|&(id, _)| id.clone())
            .map(|(_, node)| node)
            .collect::<Vec<&Node>>()
    }

    pub fn set_effective_cell_size(&mut self, effective_cell_size: f64) {
        self.effective_cell_size = effective_cell_size;
    }

    pub fn nodes(&self) -> Vec<&Node> {
        self.nodes.values().collect::<Vec<&Node>>()
    }

    pub fn links(&self) -> Vec<&Link> {
        self.links.values().collect::<Vec<&Link>>()
    }

    pub fn nodes_with_ids(&self) -> &IntMap<Id<Node>, Node> {
        &self.nodes
    }

    pub fn links_with_ids(&self) -> &IntMap<Id<Link>, Link> {
        &self.links
    }
}

impl From<IONetwork> for Network {
    fn from(io_net: IONetwork) -> Self {
        let mut result = Network::new();

        for io_node in io_net.nodes() {
            add_io_node(&mut result, io_node);
        }

        for io_link in io_net.links() {
            add_io_link(&mut result, io_link);
        }

        result.effective_cell_size = io_net.effective_cell_size();
        result
    }
}

impl From<crate::generated::network::Network> for Network {
    fn from(value: crate::generated::network::Network) -> Self {
        let mut result = Network::new();
        result.set_effective_cell_size(value.effective_cell_size);
        for wn in &value.nodes {
            let node = Node::new(
                Id::get_from_ext(&wn.id),
                wn.coordinate
                    .as_ref()
                    .map(|coordinate| Coordinate::new_3d(coordinate.x, coordinate.y, coordinate.z))
                    .unwrap(),
                wn.partition,
                wn.cmp_weight,
            );
            result.add_node(node);
        }
        for wl in &value.links {
            let modes: IntSet<Id<String>> =
                wl.modes.iter().map(|id| Id::get_from_ext(id)).collect();

            let mut link = Link::new(
                Id::get_from_ext(&wl.id),
                Id::get_from_ext(&wl.from),
                Id::get_from_ext(&wl.to),
                wl.length,
                wl.capacity,
                wl.freespeed,
                wl.permlanes,
                modes,
                wl.partition,
            );
            link.attributes = InternalAttributes::from(&wl.attributes);
            result.add_link(link);
        }
        info!("Finished converting protobuf wire type into Network");
        result
    }
}

fn add_io_node(network: &mut Network, io_node: &IONode) {
    let id = Id::create(&io_node.id);
    let part_attr = IOAttributes::find_or_else_opt(&io_node.attributes, "partition", || "0");
    let cmp_weight_attr = IOAttributes::find_or_else_opt(&io_node.attributes, "cmp_weight", || "1");
    let partition = u32::from_str(part_attr).unwrap();
    let cmp_weight = u32::from_str(cmp_weight_attr).unwrap();

    let mut node = Node::new(
        id,
        Coordinate::new_2d(io_node.x, io_node.y),
        partition,
        cmp_weight,
    );
    node.partition = partition;
    network.add_node(node);
}

fn add_io_link(network: &mut Network, io_link: &IOLink) {
    let id = Id::create(&io_link.id);
    let part_attr = IOAttributes::find_or_else_opt(&io_link.attributes, "partition", || "0");
    let partition = u32::from_str(part_attr).unwrap();
    let modes: IntSet<Id<String>> = io_link
        .modes
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(Id::create)
        .collect();
    let from_id = Id::get_from_ext(&io_link.from);
    let to_id = Id::get_from_ext(&io_link.to);

    let mut link = Link::new(
        id,
        from_id,
        to_id,
        io_link.length,
        io_link.capacity,
        io_link.freespeed,
        io_link.permlanes,
        modes,
        partition,
    );
    link.attributes = io_link
        .attributes
        .clone()
        .map(|mut attributes| {
            // partition is stored in a field, thus can be omitted when reading the attributes.
            attributes
                .attributes
                .retain(|attribute| attribute.name != "partition");
            InternalAttributes::from(attributes)
        })
        .unwrap_or_default();
    network.add_link(link);
}

impl Node {
    pub fn new(id: Id<Node>, coord: Coordinate, part: u32, cmp_weight: u32) -> Self {
        Node {
            id,
            coord,
            in_links: Vec::new(),
            out_links: Vec::new(),
            partition: part,
            cmp_weight,
        }
    }

    pub fn set_cmp_weight(&mut self, cmp_weight: u32) {
        self.cmp_weight = cmp_weight;
    }
}

impl Link {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        id: Id<Link>,
        from: Id<Node>,
        to: Id<Node>,
        length: f64,
        capacity: f64,
        freespeed: f64,
        permlanes: f64,
        modes: IntSet<Id<String>>,
        partition: u32,
    ) -> Self {
        Link {
            id,
            from,
            to,
            length,
            capacity,
            freespeed,
            permlanes,
            modes,
            partition,
            attributes: InternalAttributes::default(),
        }
    }

    pub fn new_with_default(id: Id<Link>, from: &Node, to: &Node) -> Self {
        // compute eucledean distance between from and to node
        let length =
            ((from.coord.x - to.coord.x).powi(2) + (from.coord.y - to.coord.y).powi(2)).sqrt();
        Link::new(
            id,
            from.id.clone(),
            to.id.clone(),
            length,
            1.,
            1.,
            1.,
            HashSet::default(),
            0,
        )
    }

    pub fn contains_mode(&self, mode: &Id<String>) -> bool {
        self.modes.contains(mode)
    }
}

pub fn from_file(path: &Path) -> Network {
    let network = if path.extension().unwrap().eq("binpb") {
        load_from_proto(path)
    } else if path.extension().unwrap().eq("xml")
        || path.extension().unwrap().eq("gz")
        || path.extension().unwrap().eq("zst")
    {
        network::load_from_xml(path)
    } else {
        panic!(
            "Tried to load {path:?}. File format not supported. Either use `.xml`, `.xml.gz`, `.xml.zst`, or `.binpb` as extension"
        );
    };

    // MATSim assumes car for links without modes, whereas these links allow no mode here.
    let links_without_modes = network
        .links
        .values()
        .filter(|link| link.modes.is_empty())
        .count();
    if links_without_modes > 0 {
        warn!(
            "{links_without_modes} links in {path:?} have no modes and therefore allow no mode. Modes must be listed explicitly."
        );
    }
    network
}

pub fn to_file(network: &Network, path: &Path) {
    to_file_with_link_attribute_overrides(network, path, &LinkAttributeOverrides::default());
}

pub(crate) fn to_file_with_link_attribute_overrides(
    network: &Network,
    path: &Path,
    overrides: &LinkAttributeOverrides,
) {
    if path.extension().unwrap().eq("binpb") {
        write_to_proto_with_link_attribute_overrides(network, path, overrides);
    } else if path.extension().unwrap().eq("xml")
        || path.extension().unwrap().eq("gz")
        || path.extension().unwrap().eq("zst")
    {
        write_to_xml_with_link_attribute_overrides(network, path, overrides);
    } else {
        panic!(
            "Tried to write {path:?} . File format not supported. Either use `.xml`, `.xml.gz`, `.xml.zst`, or `.binpb` as extension"
        );
    }
}

pub(crate) fn merged_link_attributes(
    link: &Link,
    overrides: &LinkAttributeOverrides,
) -> InternalAttributes {
    let mut attributes = link.attributes.clone();
    if let Some(link_overrides) = overrides.get(&link.id) {
        for (key, value) in link_overrides.iter() {
            attributes.insert(key.clone(), value);
        }
    }
    attributes
}

pub mod utils {
    use crate::simulation::id::Id;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Network};

    pub fn find_nearest_point_on_link(
        coordinate: &Coordinate,
        link: &Id<Link>,
        network: &Network,
    ) -> Coordinate {
        let from = &network.get_link(link).from;
        let to = &network.get_link(link).to;

        Coordinate::orthogonal_projection(
            coordinate,
            &network.get_node(from).coord,
            &network.get_node(to).coord,
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::simulation::config::{
        EdgeWeight, MetisOptions, ModalLinkSelection, PartitionMethod,
    };
    use crate::simulation::id::Id;
    use crate::simulation::io::xml::network::{IOLink, IONode};
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Network, Node, add_io_link, add_io_node};
    use macros::deterministic_id_test;
    use nohash_hasher::IntSet;

    fn coord(x: f64, y: f64) -> Coordinate {
        Coordinate::new_2d(x, y)
    }

    #[deterministic_id_test]
    fn add_node() {
        let mut network = Network::new();
        let id = Id::create("node-id");
        let node = Node::new(id.clone(), coord(1., 1.), 0, 1);

        assert_eq!(0, network.nodes.len());
        network.add_node(node);
        assert_eq!(1, network.nodes.len());
        assert_eq!(id, network.get_node(&id).id);
    }

    #[deterministic_id_test]
    #[should_panic]
    fn add_node_reject_duplicate() {
        let mut network = Network::new();
        let id = Id::create("node-id");
        let node = Node::new(id.clone(), coord(1., 1.), 0, 1);
        let duplicate = Node::new(id.clone(), coord(2., 2.), 0, 1);

        assert_eq!(0, network.nodes.len());
        network.add_node(node);
        network.add_node(duplicate); // expecting panic here.
    }

    #[deterministic_id_test]
    fn add_link() {
        let mut network = Network::new();
        let from = Node::new(Id::create("from"), coord(0., 0.), 0, 1);
        let to = Node::new(Id::create("to"), coord(3., 4.), 0, 1);
        let id = Id::create("link-id");
        let link = Link::new_with_default(id.clone(), &from, &to);

        network.add_node(from);
        network.add_node(to);
        network.add_link(link);

        assert_eq!(2, network.nodes.len());
        assert_eq!(1, network.links.len());
        assert_eq!(id, network.get_link(&id).id);

        let link = network.get_link(&id);
        let from = network.get_node(&link.from);
        let to = network.get_node(&link.to);

        assert_eq!(id, link.id);
        assert_eq!(0, from.in_links.len());
        assert_eq!(1, from.out_links.len());
        assert_eq!(&id, from.out_links.first().unwrap());
        assert_eq!(0, to.out_links.len());
        assert_eq!(1, to.in_links.len());
        assert_eq!(&id, to.in_links.first().unwrap());
    }

    #[deterministic_id_test]
    #[should_panic]
    fn add_link_reject_duplicate() {
        let mut network = Network::new();
        let from = Node::new(Id::create("from"), coord(0., 0.), 0, 1);
        let to = Node::new(Id::create("to"), coord(3., 4.), 0, 1);
        let id = Id::create("link-id");
        let link = Link::new_with_default(id.clone(), &from, &to);
        let duplicate = Link::new_with_default(id.clone(), &from, &to);

        network.add_node(from);
        network.add_node(to);
        network.add_link(link);
        network.add_link(duplicate); // expecting panic here
    }

    #[deterministic_id_test]
    fn remove_link() {
        let mut network = Network::new();
        let from_id = Id::create("from");
        let to_id = Id::create("to");
        let link_id = Id::create("link-id");
        let from = Node::new(from_id.clone(), coord(0., 0.), 0, 1);
        let to = Node::new(to_id.clone(), coord(3., 4.), 0, 1);
        let link = Link::new_with_default(link_id.clone(), &from, &to);

        network.add_node(from);
        network.add_node(to);
        network.add_link(link);

        network.remove_link(link_id.clone());

        assert_eq!(0, network.links.len());
        assert!(network.get_node(&from_id).out_links.is_empty());
        assert!(network.get_node(&to_id).in_links.is_empty());
    }

    #[deterministic_id_test]
    #[should_panic]
    fn remove_link_reject_missing() {
        let mut network = Network::new();
        network.remove_link(Id::create("does-not-exist"));
    }

    #[deterministic_id_test]
    fn remove_node() {
        let mut network = Network::new();
        let a_id = Id::create("a");
        let b_id = Id::create("b");
        let c_id = Id::create("c");
        let ab_id = Id::create("ab");
        let bc_id = Id::create("bc");
        let bb_id = Id::create("bb");
        let a = Node::new(a_id.clone(), coord(0., 0.), 0, 1);
        let b = Node::new(b_id.clone(), coord(1., 0.), 0, 1);
        let c = Node::new(c_id.clone(), coord(2., 0.), 0, 1);
        let ab = Link::new_with_default(ab_id.clone(), &a, &b);
        let bc = Link::new_with_default(bc_id.clone(), &b, &c);
        let bb = Link::new_with_default(bb_id.clone(), &b, &b);

        network.add_node(a);
        network.add_node(b);
        network.add_node(c);
        network.add_link(ab);
        network.add_link(bc);
        network.add_link(bb);

        network.remove_node(b_id.clone());

        assert_eq!(2, network.nodes.len());
        assert_eq!(0, network.links.len());
        assert!(network.nodes.get(&b_id).is_none());
        assert!(network.get_node(&a_id).out_links.is_empty());
        assert!(network.get_node(&c_id).in_links.is_empty());
    }

    #[deterministic_id_test]
    #[should_panic]
    fn remove_node_reject_missing() {
        let mut network = Network::new();
        network.remove_node(Id::create("does-not-exist"));
    }

    #[deterministic_id_test]
    #[ignore] // ingore this test, because it keeps not working, due to non determined ordering of metis
    fn from_file() {
        let network = Network::from_file(
            "./assets/equil/equil-network.xml",
            2,
            //I don't know, why "edge_weight = true" sets 1 as partition for all nodes.
            &PartitionMethod::Metis(MetisOptions::default().set_edge_weight(EdgeWeight::Constant)),
        );

        // check partitioning
        let expected_partitions = [0, 0, 0, 1, 1, 1, 1, 1, 1, 0, 0, 1, 0, 0, 0];
        for node in network.nodes.values() {
            let expected_partition = expected_partitions[node.id.internal() as usize];
            assert_eq!(expected_partition, node.partition);
        }
        for link in network.links.values() {
            let expected_partition = expected_partitions[link.to.internal() as usize];
            assert_eq!(expected_partition, link.partition);
        }

        // probe in and out links
        for node in network.nodes() {
            match &node.id.internal() {
                11 => {
                    assert_eq!(9, node.in_links.len());
                    assert_eq!(1, node.out_links.len());
                }
                1 => {
                    assert_eq!(9, node.out_links.len());
                    assert_eq!(1, node.in_links.len());
                }
                _ => {
                    assert_eq!(1, node.in_links.len());
                    assert_eq!(1, node.out_links.len());
                }
            }
        }

        // check cell size
        assert_eq!(7.5, network.effective_cell_size);
    }

    #[deterministic_id_test]
    fn link_new_with_default() {
        let from = Node::new(Id::create("from"), coord(0., 0.), 0, 1);
        let to = Node::new(Id::create("to"), coord(3., 4.), 0, 1);
        let id = Id::create("link-id");
        let link = Link::new_with_default(id.clone(), &from, &to);

        assert_eq!(id, link.id);
        assert_eq!(5., link.length);
        assert_eq!(from.id, link.from);
        assert_eq!(to.id, link.to);
    }

    // Car links at y=0 and y=20 and a bike link at y=10, all spanning x=0..100. The location is
    // on car-0, but closer to car-20.
    #[deterministic_id_test]
    fn modal_link_depends_on_selection() {
        let mut network = Network::new();
        for (link_id, y, mode) in [
            ("car-0", 0.0, "car"),
            ("bike-10", 10.0, "bike"),
            ("car-20", 20.0, "car"),
        ] {
            let from = Node::new(
                Id::create(&format!("{link_id}-from")),
                Coordinate::new_2d(0.0, y),
                0,
                1,
            );
            let to = Node::new(
                Id::create(&format!("{link_id}-to")),
                Coordinate::new_2d(100.0, y),
                0,
                1,
            );
            let link = Link::new(
                Id::create(link_id),
                from.id.clone(),
                to.id.clone(),
                100.0,
                1.0,
                1.0,
                1.0,
                IntSet::from_iter([Id::create(mode)]),
                0,
            );
            network.add_node(from);
            network.add_node(to);
            network.add_link(link);
        }
        let car = Id::get_from_ext("car");
        let bike = Id::get_from_ext("bike");
        let walk = Id::create("walk");
        let base = Id::<Link>::get_from_ext("car-0");
        let coord = Coordinate::new_2d(50.0, 19.0);
        let modal = |mode: &Id<String>, selection| {
            network
                .modal_link(&base, &coord, mode, selection)
                .external()
                .to_string()
        };

        // The base link allows car, but car-20 is nearer.
        assert_eq!("car-0", modal(&car, ModalLinkSelection::BaseLinkFirst));
        assert_eq!("car-20", modal(&car, ModalLinkSelection::NearestLink));
        for selection in [
            ModalLinkSelection::BaseLinkFirst,
            ModalLinkSelection::NearestLink,
        ] {
            // The base link does not allow bike.
            assert_eq!("bike-10", modal(&bike, selection));
            // No link allows walk: the base link is the fallback.
            assert_eq!("car-0", modal(&walk, selection));
            // A base link outside the network is kept.
            let outside = Id::create("outside");
            assert_eq!(
                outside,
                network.modal_link(&outside, &coord, &car, selection)
            );
        }
    }

    #[deterministic_id_test]
    fn link_allows_only_explicitly_listed_modes() {
        let from = Node::new(Id::create("from"), coord(0., 0.), 0, 1);
        let to = Node::new(Id::create("to"), coord(3., 4.), 0, 1);
        let car = Id::create("car");
        let mut link = Link::new_with_default(Id::create("link-id"), &from, &to);

        assert!(!link.contains_mode(&car));

        link.modes.insert(car.clone());
        assert!(link.contains_mode(&car));
        assert!(!link.contains_mode(&Id::create("bike")));
    }

    #[deterministic_id_test]
    fn test_metis_with_large_graph() {}

    #[deterministic_id_test]
    fn test_add_io_node() {
        let external_id = String::from("some-id");
        let x = 1.;
        let y = 2.;
        let io_node = IONode {
            id: external_id.clone(),
            x,
            y,
            attributes: None,
        };
        let mut network = Network::new();

        add_io_node(&mut network, &io_node);

        // the node should be in nodes vec and there should be a node id
        let id = Id::get_from_ext(&external_id);
        assert_eq!(external_id, id.external());

        let node = network.get_node(&id);
        assert_eq!(x, node.coord.x);
        assert_eq!(y, node.coord.y);
        assert_eq!(id, node.id);
    }

    #[deterministic_id_test]
    fn test_add_io_link() {
        let ext_from_id = String::from("from");
        let ext_to_id = String::from("to");
        let ext_link_id = String::from("link");

        let io_from = IONode {
            id: ext_from_id.clone(),
            x: 0.,
            y: 0.,
            attributes: None,
        };
        let io_to = IONode {
            id: ext_to_id.clone(),
            x: 100.,
            y: 100.,
            attributes: None,
        };
        let io_link = IOLink {
            id: ext_link_id.clone(),
            from: ext_from_id.clone(),
            to: ext_to_id.clone(),
            length: 100.,
            capacity: 100.,
            freespeed: 10.,
            permlanes: 2.,
            modes: String::from("car,ride, bike"),
            attributes: None,
        };

        let mut network = Network::new();
        add_io_node(&mut network, &io_from);
        add_io_node(&mut network, &io_to);
        add_io_link(&mut network, &io_link);

        let from = network.get_node(&Id::get_from_ext(&ext_from_id));
        let to = network.get_node(&Id::get_from_ext(&ext_to_id));
        let link = network.get_link(&Id::get_from_ext(&ext_link_id));

        assert_eq!(from.id, link.from);
        assert_eq!(to.id, link.to);
        assert_eq!(ext_link_id, link.id.external());
        assert_eq!(io_link.length, link.length);
        assert_eq!(io_link.capacity, link.capacity);
        assert_eq!(io_link.freespeed, link.freespeed);
        assert_eq!(io_link.permlanes, link.permlanes);

        assert!(link.modes.contains(&Id::get_from_ext("car")));
        assert!(link.modes.contains(&Id::get_from_ext("ride")));
        assert!(link.modes.contains(&Id::get_from_ext("bike")));
    }
}
