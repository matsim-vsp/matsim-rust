use std::path::{Path, PathBuf};

use clap::Parser;
use tracing::info;

use matsim_rust::simulation::config::PartitionMethod;
use matsim_rust::simulation::id;
use matsim_rust::simulation::logging::init_std_out_logging_thread_local;
use matsim_rust::simulation::scenario::network::Network;

fn main() {
    let _g = init_std_out_logging_thread_local();
    let args = InputArgs::parse();
    info!("Starting network conversion with args: {args:?}");

    let net_path = PathBuf::from(&args.network);
    let id_path = PathBuf::from(&args.id_store);

    if is_binary_format(&net_path) {
        info!("Converting from binary to xml format. Load id store first.");
        id::load_from_file(&id_path);
    }

    let net = Network::from_file_path(&net_path, 1, &PartitionMethod::None);
    let out_path = replace_extension(&net_path);
    net.to_file(&out_path);

    if is_binary_format(&out_path) {
        info!("Converting from xml to binary format. Writing ids to store file.");
        id::store_to_file(&id_path);
    }
}

fn is_binary_format(path: &Path) -> bool {
    path.extension()
        .expect("Network files must either end with xml, xml.gz, or binpb")
        .eq("binpb")
}

fn replace_extension(path: &PathBuf) -> PathBuf {
    let new_ext = if is_binary_format(path) {
        "xml.gz"
    } else {
        "binpb"
    };
    let mut result = PathBuf::from(path);
    result.set_extension(new_ext);
    result
}

#[derive(Parser, Debug)]
struct InputArgs {
    #[arg(short, long)]
    pub network: String,
    #[arg(short, long)]
    pub id_store: String,
}
