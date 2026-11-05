use crate::simulation::id::Id;
use crate::simulation::scenario::network::{Link, Network};
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

/// The distance a passenger travels on a transit route from the link of the access stop to the
/// link of the egress stop, like MATSim's `RouteUtils.calcDistance(TransitRoute,
/// TransitStopFacility, TransitStopFacility, Network)`: the lengths of the links after the access
/// link up to and including the egress link.
///
/// Like Java, it takes the first occurrence of the access link and then the first occurrence of
/// the egress link after it. On routes that pass a link twice, the result can thus differ from the
/// distance between the stops along the route. A route without links has distance 0.
///
/// In the end, this function sums up the lengths of links; thus it is expected to be relatively fast.
pub fn calc_transit_distance(
    network_route: &[Id<Link>],
    access_link: &Id<Link>,
    egress_link: &Id<Link>,
    network: &Network,
) -> f64 {
    let (Some(start_link), Some(end_link)) = (network_route.first(), network_route.last()) else {
        return 0.0;
    };
    // `network_route` contains the start and end links, Java's `getLinkIds` only the links between
    // them.
    let between = if network_route.len() > 2 {
        &network_route[1..network_route.len() - 1]
    } else {
        &[]
    };
    // Java skips the links up to and including the access link. If the access link is the start
    // link or does not occur between start and end (`indexOf` returns -1), it skips none.
    let skip = if start_link == access_link {
        0
    } else {
        between
            .iter()
            .position(|link| link == access_link)
            .map_or(0, |index| index + 1)
    };

    let mut distance = 0.0;
    for link in &between[skip..] {
        distance += network.get_link(link).length;
        if link == egress_link {
            break;
        }
    }
    // The end link is not among the links between, so Java adds it separately, even if the egress
    // link already occurred before.
    if end_link == egress_link {
        distance += network.get_link(end_link).length;
    }
    distance
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

    #[deterministic_id_test]
    fn calculates_transit_distance_like_java() {
        let (network, start, middle, end) = network();
        let route = vec![start.clone(), middle.clone(), end.clone()];

        // From a stop on the start link, the start link does not count.
        assert_eq!(
            200. + 300.,
            calc_transit_distance(&route, &start, &end, &network)
        );
        assert_eq!(
            200.,
            calc_transit_distance(&route, &start, &middle, &network)
        );
        assert_eq!(300., calc_transit_distance(&route, &middle, &end, &network));
        assert_eq!(0., calc_transit_distance(&[], &start, &end, &network));

        // Java looks for the first occurrence of the access link and counts the end link again,
        // although the egress link already occurred.
        let repeating = vec![
            start.clone(),
            middle.clone(),
            end.clone(),
            middle.clone(),
            end.clone(),
        ];
        assert_eq!(
            300. + 300.,
            calc_transit_distance(&repeating, &middle, &end, &network)
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
