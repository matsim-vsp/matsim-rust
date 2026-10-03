use std::path::PathBuf;

use ahash::HashMapExt;
use clap::Parser;
use nohash_hasher::IntMap;
use tracing::info;

use rust_qsim::simulation::config::PartitionMethod;
use rust_qsim::simulation::id::Id;
use rust_qsim::simulation::scenario::network::{Link, Network};
use rust_qsim::simulation::scenario::population::Population;
use rust_qsim::simulation::scenario::transit::TransitSchedule;
use rust_qsim::simulation::scenario::vehicles::Garage;

#[derive(Parser, Debug)]
struct InputArgs {
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
}

fn main() {
    rust_qsim::simulation::logging::init_std_out_logging_thread_local();
    let args = InputArgs::parse();

    let mut veh = Garage::from_file(&args.vehicles);
    let mut net = Network::from_file_path(&args.network, 1, &PartitionMethod::None);
    let transit_schedule = args
        .transit_schedule
        .as_ref()
        .map(|path| TransitSchedule::from_file(path));
    let pop = Population::from_file(&args.population, &mut veh);

    let cmp_weights = compute_computational_weights(&pop);
    assign_computational_weights(&mut net, cmp_weights);

    rust_qsim::simulation::id::store_to_file(&create_file_path(&args, "ids"));
    net.to_file(&create_file_path(&args, "network"));
    veh.to_file(&create_file_path(&args, "vehicles"));
    pop.to_file(&create_file_path(&args, "plans"));
    if let Some(transit_schedule) = transit_schedule.as_ref() {
        transit_schedule.to_file(&create_file_path(&args, "transit_schedule"));
    }
    info!("Finished conversion. Exiting.")
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
        .flat_map(|p| {
            p.selected_plan()
                .expect("Person should have InternalPlan.")
                .legs()
        })
        .filter_map(|leg| leg.route.as_ref())
        .filter_map(|route| route.as_network())
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
