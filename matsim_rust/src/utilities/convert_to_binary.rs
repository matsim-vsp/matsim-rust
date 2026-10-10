use crate::simulation::config::PartitionMethod;
use crate::simulation::id::Id;
use crate::simulation::network::partition_experiment::{
    TIME_WINDOW_SECONDS, TimeWindowDemand, TimeWindowDemandFile,
};
use crate::simulation::scenario::facilities::ActivityFacilities;
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::population::Population;
use crate::simulation::scenario::transit::TransitSchedule;
use crate::simulation::scenario::vehicles::Garage;
use ahash::HashMapExt;
use clap::Parser;
use nohash_hasher::IntMap;
use std::collections::BTreeMap;
use std::path::PathBuf;
use tracing::info;

#[derive(Parser, Debug)]
pub struct InputArgs {
    #[arg(short, long)]
    pub network: PathBuf,
    #[arg(short, long)]
    pub population: PathBuf,
    #[arg(short, long)]
    pub vehicles: PathBuf,
    #[arg(short, long)]
    pub output_dir: PathBuf,
    #[arg(short, long)]
    pub run_id: String,
    #[arg(short, long)]
    pub transit_schedule: Option<PathBuf>,
    #[arg(short, long)]
    pub facilities: Option<PathBuf>,
}

pub fn run(
    args: &InputArgs,
    f: impl FnOnce(
        &mut Network,
        &mut Population,
        &mut Garage,
        Option<&mut TransitSchedule>,
        Option<&mut ActivityFacilities>,
    ),
) {
    let mut veh = Garage::from_file(&args.vehicles);
    let mut net = Network::from_file_path(&args.network, 1, &PartitionMethod::None);
    let mut transit_schedule = args
        .transit_schedule
        .as_ref()
        .map(|path| TransitSchedule::from_file(path));
    // Facilities are loaded before the population, so that their ids exist when activities
    // reference them. Their modal links are derived in prepare_for_sim and not converted.
    let mut facilities = args
        .facilities
        .as_ref()
        .map(|path| ActivityFacilities::from_file(path));
    let mut pop = Population::from_file(&args.population, &mut veh);

    let cmp_weights = compute_computational_weights(&pop);
    let time_window_demands = compute_time_window_demands(&pop, &net, &args.population);
    assign_computational_weights(&mut net, cmp_weights);

    f(
        &mut net,
        &mut pop,
        &mut veh,
        transit_schedule.as_mut(),
        facilities.as_mut(),
    );

    crate::simulation::id::store_to_file(&create_file_path(args, "ids"));
    net.to_file(&create_file_path(args, "network"));
    veh.to_file(&create_file_path(args, "vehicles"));
    pop.to_file(&create_file_path(args, "plans"));
    if let Some(transit_schedule) = transit_schedule.as_ref() {
        transit_schedule.to_file(&create_file_path(args, "transit_schedule"));
    }
    if let Some(facilities) = facilities.as_ref() {
        facilities.to_file(&create_file_path(args, "facilities"));
    }
    let window_path = args
        .output_dir
        .join(format!("{}.workload_windows.json", args.run_id));
    std::fs::write(
        window_path,
        serde_json::to_vec_pretty(&time_window_demands)
            .expect("time-window workload must serialize"),
    )
    .expect("time-window workload sidecar must be written");
}

fn compute_time_window_demands(
    population: &Population,
    network: &Network,
    source_population: &std::path::Path,
) -> TimeWindowDemandFile {
    let mut windows = BTreeMap::<u64, BTreeMap<String, u64>>::new();
    let mut legs_without_departure_time = 0u64;
    for person in population.persons.values() {
        for leg in person.selected_plan().as_ref().unwrap().legs() {
            let Some(route) = leg.route.as_ref().and_then(|route| route.as_network()) else {
                continue;
            };
            let Some(departure) = leg.dep_time else {
                legs_without_departure_time = legs_without_departure_time
                    .checked_add(1)
                    .expect("un-timed leg count overflow");
                continue;
            };
            let window_start = departure.as_secs() / TIME_WINDOW_SECONDS * TIME_WINDOW_SECONDS;
            let demand = windows.entry(window_start).or_default();
            for link_id in route.route() {
                let node_id = network.get_link(link_id).to.external().to_string();
                let count = demand.entry(node_id).or_default();
                *count = count.checked_add(1).expect("time-window demand overflow");
            }
        }
    }
    TimeWindowDemandFile {
        source_population: source_population.display().to_string(),
        window_seconds: TIME_WINDOW_SECONDS,
        units: "route-link occurrences per node per hour".to_string(),
        legs_without_departure_time,
        windows: windows
            .into_iter()
            .map(
                |(start_seconds, node_work_by_external_id)| TimeWindowDemand {
                    start_seconds,
                    node_work_by_external_id,
                },
            )
            .collect(),
    }
}

fn create_file_path(args: &InputArgs, extension: &str) -> PathBuf {
    args.output_dir
        .join(format!("{}.{}.binpb", args.run_id, extension))
}

fn compute_computational_weights(pop: &Population) -> IntMap<Id<Link>, u32> {
    info!("Computing computational weights based on routes in plans file");
    let result: IntMap<Id<Link>, u32> = pop
        .persons
        .values()
        .flat_map(|p| p.selected_plan().as_ref().unwrap().legs())
        .filter(|leg| leg.route.is_some())
        .filter_map(|leg| leg.route.as_ref()?.as_network())
        .flat_map(|n| n.route().iter())
        .fold(IntMap::new(), |mut map, link_id| {
            map.entry(link_id.clone())
                .and_modify(|counter: &mut u32| {
                    *counter = counter
                        .checked_add(1)
                        .expect("computational link demand exceeds u32::MAX");
                })
                .or_insert(1u32);
            map
        });
    info!("Finished computing computational weights");
    result
}

fn assign_computational_weights(net: &mut Network, cmp_weights: IntMap<Id<Link>, u32>) {
    let nodes = net
        .nodes()
        .into_iter()
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();
    for node_id in nodes {
        net.get_node_mut(&node_id).cmp_weight = 1;
    }
    let mut node_weights = IntMap::new();
    for (link_id, weight) in cmp_weights {
        let link = net.get_link(&link_id);
        let total = node_weights.entry(link.to.clone()).or_insert(0u32);
        *total = total
            .checked_add(weight)
            .expect("computational node demand exceeds u32::MAX");
    }
    for (node_id, weight) in node_weights {
        net.get_node_mut(&node_id).cmp_weight = weight;
    }
}

#[cfg(test)]
mod tests {
    use super::{assign_computational_weights, compute_time_window_demands};
    use crate::simulation::id::Id;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Network, Node};
    use crate::simulation::scenario::population::Population;
    use crate::simulation::scenario::vehicles::Garage;
    use ahash::HashMapExt;
    use macros::deterministic_id_test;
    use nohash_hasher::IntMap;

    #[deterministic_id_test]
    fn incoming_link_demands_accumulate_and_unobserved_nodes_keep_nonzero_fallback() {
        let mut network = Network::new();
        let first_source = Id::create("first-source");
        let second_source = Id::create("second-source");
        let downstream = Id::create("downstream");
        let unused = Id::create("unused");
        for (index, id) in [
            first_source.clone(),
            second_source.clone(),
            downstream.clone(),
            unused.clone(),
        ]
        .into_iter()
        .enumerate()
        {
            network.add_node(Node::new(id, Coordinate::new_2d(index as f64, 0.), 0, 99));
        }
        let first_link = Id::create("first-link");
        let second_link = Id::create("second-link");
        network.add_link(Link::new_with_default(
            first_link.clone(),
            network.get_node(&first_source),
            network.get_node(&downstream),
        ));
        network.add_link(Link::new_with_default(
            second_link.clone(),
            network.get_node(&second_source),
            network.get_node(&downstream),
        ));
        let mut demand = IntMap::new();
        demand.insert(first_link.clone(), 3);
        demand.insert(second_link.clone(), 4);
        let mut reordered_demand = IntMap::new();
        reordered_demand.insert(second_link.clone(), 4);
        reordered_demand.insert(first_link.clone(), 3);
        let mut reordered_network = network.clone();

        assign_computational_weights(&mut network, demand);
        assign_computational_weights(&mut reordered_network, reordered_demand);

        assert_eq!(7, network.get_node(&downstream).cmp_weight);
        assert_eq!(1, network.get_node(&unused).cmp_weight);
        assert_eq!(
            network.get_node(&downstream).cmp_weight,
            reordered_network.get_node(&downstream).cmp_weight
        );

        let mut overflowing_demand = IntMap::new();
        overflowing_demand.insert(first_link, u32::MAX);
        overflowing_demand.insert(second_link, 1);
        let overflow_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assign_computational_weights(&mut network, overflowing_demand);
        }));
        assert!(overflow_result.is_err());
    }

    #[deterministic_id_test]
    fn routed_population_demand_is_grouped_into_sorted_hourly_windows() {
        let network =
            Network::from_file_as_is(std::path::Path::new("./assets/equil/equil-network.xml"));
        let mut vehicles =
            Garage::from_file(std::path::Path::new("./assets/equil/equil-vehicles.xml"));
        let population = Population::from_file("./assets/equil/equil-plans.xml", &mut vehicles);

        let windows =
            compute_time_window_demands(&population, &network, std::path::Path::new("plans.xml"));

        assert_eq!(3600, windows.window_seconds);
        assert_eq!("route-link occurrences per node per hour", windows.units);
        assert!(!windows.windows.is_empty());
        assert!(
            windows
                .windows
                .windows(2)
                .all(|pair| { pair[0].start_seconds < pair[1].start_seconds })
        );
        assert!(
            windows.windows.iter().all(|window| window
                .node_work_by_external_id
                .values()
                .sum::<u64>()
                > 0)
        );
    }
}
