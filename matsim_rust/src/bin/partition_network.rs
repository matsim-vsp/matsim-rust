use std::path::PathBuf;

use clap::Parser;
use tracing::info;

use matsim_rust::simulation::config::{MetisOptions, PartitionMethod};
use matsim_rust::simulation::id;
use matsim_rust::simulation::scenario::network::Network;

/// This binary partitions a network into a given number of parts.
/// A new network file is written to the same folder as the input network file.
///
/// The new file has the same name as the input file, but with the number of parts appended to the name.
/// e.g. `network.binpb` -> `network.4.binpb`
fn main() {
    matsim_rust::simulation::logging::init_std_out_logging_thread_local();
    let args = InputArgs::parse();

    if let Some(id_path) = args.id_path {
        id::load_from_file(&id_path);
    }

    //let input_path = PathBuf::from(&args.in_path);
    let folder = args.net_path.parent().unwrap();
    let mut name_parts: Vec<&str> = args
        .net_path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .split('.')
        .collect();
    let num_parts_string = args.num_parts.to_string();
    name_parts.insert(name_parts.len() - 1, num_parts_string.as_str());
    let out_path = folder.join(name_parts.join("."));
    //info!("Writing to {:?}", out_path);
    //name_parts.insert(name_parts.len() - 3, "internal-ids");
    // let out_path_internal = folder.join(name_parts.join("."));
    //info!("Writing to {:?}", out_path_internal);

    info!(
        "Partition network: {} into {} parts.",
        args.net_path.to_str().unwrap(),
        args.num_parts
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
