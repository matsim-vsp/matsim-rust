use crate::generated::general::Coordinate;
use crate::generated::network::{Link, Node};
use crate::simulation::scenario::network::{
    LinkAttributeOverrides, Network, merged_link_attributes,
};
use std::path::Path;
use tracing::info;

pub fn load_from_proto(path: &Path) -> Network {
    info!("Start reading proto network from path: {path:?}");
    let wire_net: crate::generated::network::Network = crate::generated::read_from_file(path);
    let res = Network::from(wire_net);
    info!("Finished reading proto network from path: {path:?}");
    res
}

pub fn write_to_proto(network: &Network, path: &Path) {
    write_to_proto_with_link_attribute_overrides(network, path, &LinkAttributeOverrides::default());
}

pub(crate) fn write_to_proto_with_link_attribute_overrides(
    network: &Network,
    path: &Path,
    overrides: &LinkAttributeOverrides,
) {
    info!("Start writing proto network to path: {path:?}");
    let wire_network =
        crate::generated::network::Network::from_with_link_attribute_overrides(network, overrides);
    crate::generated::write_to_file(wire_network, path);
    info!("Finished writing proto network to path: {path:?}");
}

impl crate::generated::network::Network {
    pub fn from(network: &Network) -> Self {
        Self::from_with_link_attribute_overrides(network, &LinkAttributeOverrides::default())
    }

    pub(crate) fn from_with_link_attribute_overrides(
        network: &Network,
        overrides: &LinkAttributeOverrides,
    ) -> Self {
        info!("Converting Network into wire format");
        let nodes: Vec<_> = network
            .nodes()
            .iter()
            .map(|n| Node {
                id: n.id.external().to_string(),
                coordinate: Some(Coordinate {
                    x: n.coord.x,
                    y: n.coord.y,
                    z: n.coord.z,
                }),
                partition: n.partition,
                cmp_weight: n.cmp_weight,
            })
            .collect();
        let links: Vec<_> = network
            .links()
            .iter()
            .map(|l| {
                let attributes = merged_link_attributes(l, overrides).as_cloned_map();
                Link {
                    id: l.id.external().to_string(),
                    from: l.from.external().to_string(),
                    to: l.to.external().to_string(),
                    length: l.length,
                    capacity: l.capacity,
                    freespeed: l.freespeed,
                    permlanes: l.permlanes,
                    modes: l.modes.iter().map(|id| id.external().to_string()).collect(),
                    partition: l.partition,
                    attributes,
                }
            })
            .collect();

        crate::generated::network::Network {
            nodes,
            links,
            effective_cell_size: network.effective_cell_size(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::simulation::InternalAttributes;
    use crate::simulation::id::Id;
    use crate::simulation::network::STORAGE_CAPACITY_USED_IN_QSIM;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, LinkAttributeOverrides, Network, Node};
    use macros::deterministic_id_test;

    #[deterministic_id_test]
    fn node_coordinate_round_trip_preserves_none_z() {
        let mut network = Network::new();
        network.add_node(Node::new(
            Id::create("node-1"),
            Coordinate::new_2d(1.0, 2.0),
            3,
            4,
        ));

        let wire = crate::generated::network::Network::from(&network);
        let round_trip = Network::from(wire);

        let node = round_trip.get_node(&Id::get_from_ext("node-1"));
        assert_eq!(1.0, node.coord.x);
        assert_eq!(2.0, node.coord.y);
        assert_eq!(0., node.coord.z);
    }

    #[deterministic_id_test]
    fn storage_capacity_attribute_overlay_round_trips_through_protobuf_without_mutating_network() {
        let mut network = Network::new();
        let from = Node::new(Id::create("from"), Coordinate::default(), 0, 1);
        let to = Node::new(Id::create("to"), Coordinate::default(), 0, 1);
        let link_id = Id::create("link");
        let link = Link::new_with_default(link_id.clone(), &from, &to);
        network.add_node(from);
        network.add_node(to);
        network.add_link(link);

        let mut link_attributes = InternalAttributes::default();
        link_attributes.insert(STORAGE_CAPACITY_USED_IN_QSIM, 200.);
        let mut overrides = LinkAttributeOverrides::default();
        overrides.insert(link_id.clone(), link_attributes);

        let wire = crate::generated::network::Network::from_with_link_attribute_overrides(
            &network, &overrides,
        );
        assert_eq!(
            Some(200.),
            wire.links[0].attributes[STORAGE_CAPACITY_USED_IN_QSIM].as_double_opt()
        );

        let round_trip = Network::from(wire);
        assert_eq!(
            Some(200.),
            round_trip
                .get_link(&link_id)
                .attributes
                .get::<f64>(STORAGE_CAPACITY_USED_IN_QSIM)
        );
        assert_eq!(
            None,
            network
                .get_link(&link_id)
                .attributes
                .get::<f64>(STORAGE_CAPACITY_USED_IN_QSIM)
        );
    }
}
