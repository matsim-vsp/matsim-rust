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
    let _guard = matsim_rust::simulation::logging::init_std_out_logging_thread_local();
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

    let mut net1 = if args.coarse_regions.is_some() {
        Network::from_file_path(&args.net_path, 1, &PartitionMethod::None)
    } else {
        Network::from_file_path(
            &args.net_path,
            args.num_parts,
            &PartitionMethod::Metis(MetisOptions::default()),
        )
    };
    let experiment_metadata = args.coarse_regions.map(|regions| {
        let experiment = matsim_rust::simulation::network::partition_experiment::prepare(
            &net1,
            &args.net_path.display().to_string(),
            args.num_parts,
            regions,
            args.seed,
            &MetisOptions::default(),
        )
        .unwrap_or_else(|error| {
            eprintln!("Partition experiment rejected its inputs: {error}");
            std::process::exit(2);
        });
        net1.apply_partition_ownership(&experiment.node_owners, args.num_parts)
            .unwrap_or_else(|error| {
                eprintln!("Partition experiment ownership failed validation: {error}");
                std::process::exit(2);
            });
        info!(
            "Selected {} layout for offline partition experiment; reasons: {:?}",
            experiment.metadata.selected_layout, experiment.metadata.rejection_reasons
        );
        experiment.metadata
    });
    info!(
        "Network is loaded with {} links and {} nodes.",
        net1.links().len(),
        net1.nodes().len()
    );

    net1.to_file(&out_path);
    if let Some(metadata) = experiment_metadata {
        let metadata_path = out_path.with_extension("experiment.json");
        std::fs::write(
            &metadata_path,
            serde_json::to_vec_pretty(&metadata).expect("experiment metadata must serialize"),
        )
        .expect("experiment metadata must be written");
        info!("Wrote partition experiment metadata to {:?}", metadata_path);
    }

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
    /// Opt in to coarse-region assignment. Must exceed --num-parts.
    #[arg(long)]
    pub coarse_regions: Option<u32>,
    /// Reproducible assignment-solver seed.
    #[arg(long, default_value_t = matsim_rust::simulation::config::DEFAULT_RANDOM_SEED)]
    pub seed: u64,
}
