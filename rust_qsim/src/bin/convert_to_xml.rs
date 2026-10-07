use std::path::PathBuf;

use clap::Parser;
use tracing::info;

use rust_qsim::simulation::config::PartitionMethod;
use rust_qsim::simulation::scenario::facilities::ActivityFacilities;
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
    pub facilities: Option<String>,
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
    let facilities_path = args.facilities.map(|s| PathBuf::from(&s));

    rust_qsim::simulation::id::load_from_file(&ids_path);

    if let Some(net_path) = net_path {
        info!("Loading network from {:?}", net_path);
        let net = Network::from_file_path(&net_path, 1, &PartitionMethod::None);

        info!("Converting network to XML format");
        net.to_file(&replace_filename(net_path));
    }

    if let Some(facilities_path) = facilities_path {
        info!("Loading facilities from {:?}", facilities_path);
        let facilities = ActivityFacilities::from_file(&facilities_path);

        info!("Converting facilities to XML format");
        facilities.to_file(&replace_filename(facilities_path));
    }

    let mut veh = if let Some(veh_path) = veh_path {
        info!("Loading vehicles from {:?}", veh_path);
        let veh = Garage::from_file(&veh_path);

        info!("Converting vehicles to XML format");
        veh.to_file(&replace_filename(veh_path));
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
        pop.to_file(&replace_filename(pop_path))
    }
}

fn replace_filename(path: PathBuf) -> PathBuf {
    let file_name = path.file_name().unwrap().to_str().unwrap();
    let stripped = file_name.strip_suffix(".binpb");
    let new_file_name = format!("{}.xml.gz", stripped.unwrap());
    info!("New file name: {}", new_file_name);
    path.parent().unwrap().join(new_file_name)
}
