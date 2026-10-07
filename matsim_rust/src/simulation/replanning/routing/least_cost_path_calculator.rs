use crate::simulation::id::Id;
use crate::simulation::replanning::routing::cost::Disutility;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::SimTime;
use derive_builder::Builder;
use std::fmt::Debug;
use std::time::Duration;

/// A request for the calculation of least cost paths. Contain all relevant data for the
/// calculation, that is
/// - from- and to-links
/// - the departure time at the from-node and optionally a person and vehicle. These are passed to
///     travel time and disutility functions in routing (the latter being used as cost, and the
///     former used to determine the arrival times at specific nodes)
///
/// This is what an implementation of `LeastCostPathCalculator` receives as input when calculating
/// a least cost path. Note that the router owns its graph, so the request does not specifiy the
/// graph to route on, instead users must use the correct router.
#[derive(Builder, Clone)]
pub struct LeastCostPathRequest<'r> {
    // From and to are deliberately not nodes but links. This allows considering those links as well during routing.
    pub from: Id<Link>,
    pub to: Id<Link>,
    #[builder(default)]
    pub departure_time: SimTime,
    #[builder(default)]
    pub person: Option<&'r InternalPerson>,
    #[builder(default)]
    pub vehicle: Option<&'r InternalVehicle>,
}

/// A least cost path, given as a vector of network link ids, together with the travel time needed
/// to take the path and the corresponding travel disutility (it's the latter which is optimal, so
/// it's truly a least-disutility path).
#[derive(PartialEq, Debug)]
pub struct LeastCostPath {
    pub path: Vec<Id<Link>>,
    pub travel_time: Duration,
    pub travel_disutility: Disutility,
}

/// Router that calculates a least cost path between given from- and to-links.
/// The router contains the network to route on (typically stored as a graph) and the cost function,
/// which is in this case a travel disutility function.
pub trait LeastCostPathCalculator: Send + Sync {
    /// Calculate the least cost path as defined in the request. Requests contain from- and
    /// to-links as well as the departure time and an optional person and vehicle.
    /// The network to route on and the cost function are properties of the implementation of the
    /// router.
    /// If no path is found, either because the to-link is unreachable or because the from- or
    /// to-link do not exist in the graph, None is returned.
    /// Otherwise, the path is returned together with its travel time and disutility.
    fn calc_least_cost_path(&self, request: LeastCostPathRequest) -> Option<LeastCostPath>;
}

#[cfg(test)]
mod tests {
    use crate::simulation::id::Id;
    use crate::simulation::replanning::routing::a_star::Dijkstra;
    use crate::simulation::replanning::routing::cost::{
        Disutility, FreeOrMaxSpeedTravelTimeAndDisutility,
    };
    use crate::simulation::replanning::routing::graph::tests::get_triangle_test_network;
    use crate::simulation::replanning::routing::least_cost_path_calculator::LeastCostPathCalculator;
    use crate::simulation::replanning::routing::least_cost_path_calculator::{
        LeastCostPath, LeastCostPathRequestBuilder,
    };
    use crate::simulation::scenario::network::Link;
    use macros::deterministic_id_test;
    use std::sync::Arc;
    use std::time::Duration;

    /// simple test just to make sure that the interface works. More precise testing is done
    /// in the respective files where implementations of LeastCostPathCaltulator are defined.
    #[deterministic_id_test]
    fn test_least_cost_path_interface() {
        // triangle graph
        let network = get_triangle_test_network();

        // DijkstraRouter is an alias for AStarRouter<ZeroHeuristic>
        let travel_cost = Arc::new(FreeOrMaxSpeedTravelTimeAndDisutility);
        let router =
            Dijkstra::new(Arc::new(network), None, travel_cost.clone(), travel_cost).unwrap();

        let request = LeastCostPathRequestBuilder::default()
            .from(Id::create("1")) // these links are connected via
            .to(Id::create("5")) // link "4", which takes 4 secs
            .build()
            .unwrap();

        let expected_path: Vec<Id<Link>> = [Id::create("4")].into_iter().collect();

        let result = router.calc_least_cost_path(request);
        assert_eq!(
            result,
            Some(LeastCostPath {
                travel_time: Duration::from_secs(4),
                travel_disutility: 4.0 as Disutility,
                path: expected_path,
            })
        );
    }
}
