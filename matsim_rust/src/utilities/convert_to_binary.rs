use crate::simulation::config::PartitionMethod;
use crate::simulation::id::Id;
use crate::simulation::scenario::facilities::ActivityFacilities;
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::population::Population;
use crate::simulation::scenario::transit::TransitSchedule;
use crate::simulation::scenario::vehicles::Garage;
use ahash::HashMapExt;
use clap::Parser;
use nohash_hasher::IntMap;
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
    use super::assign_computational_weights;
    use crate::simulation::id::Id;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Network, Node};
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
        demand.insert(first_link, 3);
        demand.insert(second_link, 4);

        assign_computational_weights(&mut network, demand);

        assert_eq!(7, network.get_node(&downstream).cmp_weight);
        assert_eq!(1, network.get_node(&unused).cmp_weight);
    }
}
