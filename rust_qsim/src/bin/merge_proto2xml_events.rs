use clap::Parser;
use rust_qsim::simulation::events::EventsManager;
use rust_qsim::simulation::events::utils::read_partitioned_events;
use rust_qsim::simulation::id;
use rust_qsim::simulation::io::xml::events::XmlEventsWriter;
use rust_qsim::simulation::logging::init_std_out_logging_thread_local;
use std::path::PathBuf;
use tracing::info;

#[derive(Parser, Debug)]
struct InputArgs {
    #[arg(long)]
    pub path: String,
    #[arg(long)]
    pub id_store: String,
    #[arg(long, default_value_t = 1)]
    pub num_parts: u32,
}

/// merges proto events from multiple files into a single proto file
fn main() {
    let _g = init_std_out_logging_thread_local();
    let args = InputArgs::parse();

    info!("Load Id Store");
    id::load_from_file(&PathBuf::from(args.id_store));

    let mut manager = EventsManager::new();

    let output_file_path = PathBuf::from(&args.path).join("events.xml.gz");
    let register_proto_writer = XmlEventsWriter::register_fn(output_file_path.clone());

    register_proto_writer(&mut manager);

    read_partitioned_events(
        &mut manager,
        PathBuf::from(args.path),
        "events",
        args.num_parts,
        "binpb",
    )
    .expect("Failed to read events from file");
    info!(
        "Finished writing to xml file ({}).",
        output_file_path.display()
    );
}
