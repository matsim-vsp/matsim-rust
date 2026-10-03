use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use ahash::HashSet;
use clap::Parser;
use tracing::info;

use rust_qsim::simulation::config::{
    Config, EdgeWeight, MetisOptions, PartitionMethod, VertexWeight,
};
use rust_qsim::simulation::id;
use rust_qsim::simulation::logging::init_std_out_logging_thread_local;
use rust_qsim::simulation::network::LinkStorageCapacities;
use rust_qsim::simulation::network::sim_network::SimNetworkPartition;
use rust_qsim::simulation::scenario::network::Network;

// I would have expected, that we read already partitioned networks and write the neighbors of each partition to a file.
// But we are partitioning the network in each iteration and write the neighbors of each partition to a file. paul, jan'25
fn main() {
    let _g = init_std_out_logging_thread_local();
    let args = InputArgs::parse();

    id::load_from_file(&args.id_store);
    let mut writer =
        BufWriter::new(File::create(&args.output).expect("Could not open output file."));
    writer
        .write_all("size,rank,neighbors\n".as_bytes())
        .expect("failed to write header");

    for i in 1..12 {
        info!("Loading network from {:?}", args.network);
        let num_parts: u32 = 2_i32.pow(i) as u32;
        let net = Network::from_file_path(
            &args.network,
            num_parts,
            &PartitionMethod::Metis(MetisOptions {
                vertex_weight: vec![VertexWeight::PreComputed],
                edge_weight: EdgeWeight::Capacity,
                imbalance_factor: 0.03,
                iteration_number: 10,
                contiguous: true,
            }),
        );
        let config = Config::default();
        let storage_capacities = LinkStorageCapacities::from_network(&net, config.qsim());
        let distinct_partitions: HashSet<u32> = net.nodes().iter().map(|n| n.partition).collect();
        for partition in distinct_partitions {
            let net_partition =
                SimNetworkPartition::from_network(&net, &storage_capacities, partition, &config);
            let neighbors = net_partition.neighbors().len();
            let serialized = format!("{},{},{}\n", num_parts, partition, neighbors);
            writer
                .write_all(serialized.as_bytes())
                .expect("Failed to write entry.");
        }
    }

    match writer.flush() {
        Ok(_) => info!("Finished writing output file to: {:?}", args.output),
        Err(e) => panic!("Failed to flush writer: {}", e),
    }
}

#[derive(Parser, Debug)]
struct InputArgs {
    #[arg(short, long)]
    pub id_store: PathBuf,
    #[arg(short, long)]
    pub network: PathBuf,
    #[arg(short, long)]
    pub output: PathBuf,
}
