use crate::simulation::scenario::network::Network;
use crate::simulation::scenario::population::InternalNetworkRoute;

pub fn calc_distance(
    route: &InternalNetworkRoute,
    relative_position_start: f64,
    relative_position_end: f64,
    network: &Network,
) -> f64 {
    // InternalNetworkRoute stores the start and end links in `route`, so only
    // the links between them contribute their full length.
    let mut route_distance = route
        .route()
        .iter()
        .skip(1)
        .take(route.route().len().saturating_sub(2))
        .map(|link_id| network.get_link(link_id).length)
        .sum::<f64>();

    let generic_route = route.generic_delegate();
    let start_link = network.get_link(generic_route.start_link());
    route_distance += start_link.length * (1.0 - relative_position_start);

    let end_link = network.get_link(generic_route.end_link());
    if generic_route.start_link() != generic_route.end_link() {
        route_distance += end_link.length * relative_position_end;
    } else {
        route_distance -= end_link.length * (1.0 - relative_position_end);
    }

    route_distance
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::InternalAttributes;
    use crate::simulation::id::Id;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Node};
    use crate::simulation::scenario::population::InternalGenericRoute;
    use macros::deterministic_id_test;
    use nohash_hasher::IntSet;

    #[deterministic_id_test]
    fn calculates_distance_for_different_start_and_end_links() {
        let (network, start, middle, end) = network();
        let generic_route = InternalGenericRoute::new(start.clone(), end.clone(), None, None, None);
        let route = InternalNetworkRoute::new(generic_route, vec![start, middle, end]);

        assert_eq!(
            75. + 200. + 150.,
            calc_distance(&route, 0.25, 0.5, &network)
        );

        assert_eq!(0. + 200. + 300., calc_distance(&route, 1.0, 1.0, &network));
    }

    #[deterministic_id_test]
    fn calculates_distance_when_start_and_end_link_are_the_same() {
        let (network, start, middle, _) = network();
        let generic_route =
            InternalGenericRoute::new(start.clone(), start.clone(), None, None, None);

        let direct_route = InternalNetworkRoute::new(generic_route.clone(), vec![start.clone()]);
        assert_eq!(
            25.0 + 25.0,
            calc_distance(&direct_route, 0.25, 0.75, &network)
        );

        // This is a bit weird, but follows the Java implementation. Not sure if we can even encounter this situation (since a "true"
        // round trip with more than one link would not be the fastest route). The least cost path calculator would never produce such routes.
        // Thus, the behavior is only needed for single link routes, which is correct. paul, sep'26.
        let round_trip =
            InternalNetworkRoute::new(generic_route, vec![start.clone(), middle, start]);
        assert_eq!(
            75. + 200. - 25.,
            calc_distance(&round_trip, 0.25, 0.75, &network)
        );
    }

    fn network() -> (Network, Id<Link>, Id<Link>, Id<Link>) {
        let mut network = Network::new();
        let nodes = (0..4)
            .map(|index| Id::create(&format!("distance_node_{index}")))
            .collect::<Vec<Id<Node>>>();

        for node in &nodes {
            network.add_node(Node::new(node.clone(), Coordinate::default(), 0, 1));
        }

        let start = add_link(&mut network, "distance_start", &nodes[0], &nodes[1], 100.0);
        let middle = add_link(&mut network, "distance_middle", &nodes[1], &nodes[2], 200.0);
        let end = add_link(&mut network, "distance_end", &nodes[2], &nodes[3], 300.0);

        (network, start, middle, end)
    }

    fn add_link(
        network: &mut Network,
        id: &str,
        from: &Id<Node>,
        to: &Id<Node>,
        length: f64,
    ) -> Id<Link> {
        let link_id = Id::create(id);
        network.add_link(Link {
            id: link_id.clone(),
            from: from.clone(),
            to: to.clone(),
            length,
            capacity: 1.0,
            freespeed: 1.0,
            permlanes: 1.0,
            modes: IntSet::default(),
            partition: 0,
            attributes: InternalAttributes::default(),
        });
        link_id
    }
}
