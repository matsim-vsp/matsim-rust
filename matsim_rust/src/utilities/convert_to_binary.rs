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
                .and_modify(|counter| *counter += 1)
                .or_insert(1u32);
            map
        });
    info!("Finished computing computational weights");
    result
}

fn assign_computational_weights(net: &mut Network, cmp_weights: IntMap<Id<Link>, u32>) {
    for (link_id, weight) in cmp_weights {
        let link = net.get_link(&link_id);
        let node = net.get_node_mut(&link.to.clone());
        node.cmp_weight = weight;
    }
}
