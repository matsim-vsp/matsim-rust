use std::path::PathBuf;

use clap::Parser;
use tracing::info;

use rust_qsim::simulation::config::{MetisOptions, PartitionMethod};
use rust_qsim::simulation::id;
use rust_qsim::simulation::scenario::network::Network;

/// This binary partitions a network into a given number of parts.
/// A new network file is written to the same folder as the input network file.
///
/// The new file has the same name as the input file, but with the number of parts appended to the name.
/// e.g. `network.binpb` -> `network.4.binpb`
fn main() {
    rust_qsim::simulation::logging::init_std_out_logging_thread_local();
    let args = InputArgs::parse();

    if let Some(id_path) = args.id_path {
        id::load_from_file(&id_path);
    }

    //let input_path = PathBuf::from(&args.in_path);
    let current_extension = args
        .net_path
        .extension()
        .expect("Population path should have extension.");
    let out_path = args
        .net_path
        .with_extension(args.num_parts.to_string())
        .with_added_extension(current_extension);
    //info!("Writing to {:?}", out_path);
    //name_parts.insert(name_parts.len() - 3, "internal-ids");
    // let out_path_internal = folder.join(name_parts.join("."));
    //info!("Writing to {:?}", out_path_internal);

    info!(
        "Partition network: {:?} into {} parts.",
        args.net_path, args.num_parts
    );

    let net1 = Network::from_file_path(
        &args.net_path,
        args.num_parts,
        &PartitionMethod::Metis(MetisOptions::default()),
    );
    info!(
        "Network is loaded with {} links and {} nodes.",
        net1.links().len(),
        net1.nodes().len()
    );

    net1.to_file(&out_path);

    info!(
        "Finished partitioning Network. Written file to {:?}",
        out_path
    );
}

#[derive(Parser, Debug)]
struct InputArgs {
    #[arg(short, long)]
    pub net_path: PathBuf,
    #[arg(short, long)]
    pub id_path: Option<PathBuf>,
    #[arg(long)]
    pub num_parts: u32,
}
