use matsim_rust::simulation::scenario::Scenario;

pub fn force_train_boundary(scenario: &mut Scenario) {
    let route = scenario
        .transit_schedule
        .lines()
        .values()
        .flat_map(|line| line.routes.values())
        .find(|route| route.transport_mode.external() == "train")
        .unwrap();
    let [from, to] = route
        .network_route
        .windows(2)
        .find(|pair| {
            scenario.network.get_link(&pair[0]).to != scenario.network.get_link(&pair[1]).to
        })
        .unwrap()
    else {
        unreachable!()
    };
    for (link_id, partition) in [(from, 0), (to, 1)] {
        let node = scenario.network.get_link(link_id).to.clone();
        let in_links = scenario.network.get_node(&node).in_links.clone();
        scenario.network.get_node_mut(&node).partition = partition;
        for link in in_links {
            scenario.network.get_link_mut(&link).partition = partition;
        }
    }
}
