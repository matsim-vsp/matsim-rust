use std::hash::Hasher;
use std::io::Read;
use std::path::PathBuf;

use clap::Parser;
use tracing::info;

use matsim_rust::simulation::config::{MetisOptions, PartitionMethod, VertexWeight};
use matsim_rust::simulation::id;
use matsim_rust::simulation::network::partition_experiment::TimeWindowDemandFile;
use matsim_rust::simulation::scenario::network::Network;

/// This binary partitions a network into a given number of parts.
/// A new network file is written to the same folder as the input network file.
///
/// The new file has the same name as the input file, but with the number of parts appended to the name.
/// e.g. `network.binpb` -> `network.4.binpb`
fn main() {
    let _guard = matsim_rust::simulation::logging::init_std_out_logging_thread_local();
    let args = InputArgs::parse();
    if args.demand_windows.is_some() && args.coarse_regions.is_none() {
        eprintln!("--demand-windows requires --coarse-regions");
        std::process::exit(2);
    }
    let demand_windows_path = args.demand_windows.clone();

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
            &PartitionMethod::Metis(
                MetisOptions::default().add_vertex_weight(VertexWeight::PreComputed),
            ),
        )
    };
    let time_windows = args.demand_windows.map(|path| {
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("Failed to read {}: {error}", path.display()));
        let data: TimeWindowDemandFile = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("Invalid workload {}: {error}", path.display()));
        if data.window_seconds
            != matsim_rust::simulation::network::partition_experiment::TIME_WINDOW_SECONDS
        {
            eprintln!(
                "Unsupported demand window size: {} seconds",
                data.window_seconds
            );
            std::process::exit(2);
        }
        if data.units != matsim_rust::simulation::network::partition_experiment::TIME_WINDOW_UNITS
            || data.source_population.is_empty()
        {
            eprintln!(
                "Unsupported or incomplete workload units/provenance in {}",
                path.display()
            );
            std::process::exit(2);
        }
        data
    });
    let mut experiment_metadata = args.coarse_regions.map(|regions| {
        let mut experiment =
            matsim_rust::simulation::network::partition_experiment::prepare_with_time_windows(
                &net1,
                &args.net_path.display().to_string(),
                args.num_parts,
                regions,
                args.seed,
                &MetisOptions::default(),
                time_windows
                    .as_ref()
                    .map_or(&[][..], |data| data.windows.as_slice()),
                demand_windows_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .as_deref(),
                time_windows
                    .as_ref()
                    .map(|data| data.legs_without_departure_time),
            )
            .unwrap_or_else(|error| {
                eprintln!("Partition experiment rejected its inputs: {error}");
                std::process::exit(2);
            });
        let baseline_path = with_marker(&out_path, "baseline");
        let mut baseline_network = net1.clone();
        baseline_network
            .apply_partition_ownership(&experiment.baseline_node_owners, args.num_parts)
            .unwrap_or_else(|error| {
                eprintln!("Weighted-METIS baseline ownership failed validation: {error}");
                std::process::exit(2);
            });
        baseline_network.to_file(&baseline_path);
        experiment.metadata.baseline_network_path = Some(baseline_path.display().to_string());
        experiment.metadata.baseline_network_fnv64 = Some(file_fnv64(&baseline_path));
        if let Some(data) = time_windows.as_ref() {
            experiment.metadata.time_window_source_population =
                Some(data.source_population.clone());
            experiment.metadata.time_window_units = Some(data.units.clone());
        }
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
    if let Some(mut metadata) = experiment_metadata.take() {
        metadata.selected_network_fnv64 = Some(file_fnv64(&out_path));
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

fn file_fnv64(path: &std::path::Path) -> String {
    let mut file = std::fs::File::open(path)
        .unwrap_or_else(|error| panic!("Failed to read {}: {error}", path.display()));
    let mut hasher = fnv::FnvHasher::default();
    let mut buffer = [0; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .unwrap_or_else(|error| panic!("Failed to read {}: {error}", path.display()));
        if read == 0 {
            break;
        }
        hasher.write(&buffer[..read]);
    }
    format!("{:016x}", hasher.finish())
}

fn with_marker(path: &std::path::Path, marker: &str) -> PathBuf {
    let stem = path.file_stem().expect("network output has a file stem");
    let extension = path.extension().expect("network output has an extension");
    path.with_file_name(format!(
        "{}.{}.{}",
        stem.to_string_lossy(),
        marker,
        extension.to_string_lossy()
    ))
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
    /// Hourly route-demand sidecar written by convert_to_binary.
    #[arg(long)]
    pub demand_windows: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::{file_fnv64, with_marker};
    use std::io::Write;

    #[test]
    fn baseline_output_marker_preserves_network_extension() {
        assert_eq!(
            std::path::PathBuf::from("network.2.baseline.binpb"),
            with_marker(std::path::Path::new("network.2.binpb"), "baseline")
        );
    }

    #[test]
    fn file_fnv64_matches_fnv1a_known_value() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"hello")
            .unwrap();
        assert_eq!("a430d84680aabd0b", file_fnv64(&path));
    }
}
