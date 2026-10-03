use std::path::PathBuf;

use clap::Parser;
use tracing::info;

use rust_qsim::simulation::config::PartitionMethod;
use rust_qsim::simulation::scenario::network::Network;
use rust_qsim::simulation::scenario::population::Population;
use rust_qsim::simulation::scenario::vehicles::Garage;

#[derive(Parser, Debug)]
struct InputArgs {
    #[arg(short, long)]
    pub network: Option<String>,
    #[arg(short, long)]
    pub population: Option<String>,
    #[arg(short, long)]
    pub vehicles: Option<String>,
    #[arg(short, long)]
    pub ids: String,
}

fn main() {
    rust_qsim::simulation::logging::init_std_out_logging_thread_local();
    let args = InputArgs::parse();
    let ids_path = PathBuf::from(&args.ids);

    let net_path = args.network.map(|s| PathBuf::from(&s));
    let pop_path = args.population.map(|s| PathBuf::from(&s));
    let veh_path = args.vehicles.map(|s| PathBuf::from(&s));

    rust_qsim::simulation::id::load_from_file(&ids_path);

    if let Some(net_path) = net_path {
        info!("Loading network from {:?}", net_path);
        let net = Network::from_file_path(&net_path, 1, &PartitionMethod::None);

        info!("Converting network to XML format");
        net.to_file(&net_path.with_extension(".xml.gz"));
    }

    let mut veh = if let Some(veh_path) = veh_path {
        info!("Loading vehicles from {:?}", veh_path);
        let veh = Garage::from_file(&veh_path);

        info!("Converting vehicles to XML format");
        veh.to_file(&veh_path.with_extension(".xml.gz"));
        Some(veh)
    } else {
        None
    };

    if let Some(pop_path) = pop_path {
        info!("Loading population from {:?}", pop_path);
        let pop = Population::from_file(
            &pop_path,
            veh.as_mut()
                .expect("Vehicles must be provided if population is provided."),
        );
        info!("Converting population to XML format");
        pop.to_file(&pop_path.with_extension(".xml.gz"))
    }
}
