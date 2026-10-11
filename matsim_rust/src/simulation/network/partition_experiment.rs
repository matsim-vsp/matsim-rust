//! Deterministic, offline coarse-region assignment experiment.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::hash::Hasher;
use std::time::Instant;

use fnv::FnvHasher;
use katgpt_assign::NEVER_WORSE_GUARD;
use katgpt_assign::{Limits, Problem, Seed, Spec, StopReason, solve};
use nohash_hasher::IntMap;
use rand::RngExt;
use serde::{Deserialize, Serialize};

use crate::simulation::build_info::GIT_VERSION;
use crate::simulation::config::{MetisOptions, VertexWeight};
use crate::simulation::id::Id;
use crate::simulation::network::metis_partitioning::{self, get_adjwgt};
use crate::simulation::random::get_rng;
use crate::simulation::scenario::network::{Network, Node};

const ASSIGNMENT_REVISION: &str = "b72a2c03383ed0c055df15e56b6a2d4c55cd1474";
const MEMORY_ESTIMATE_BYTES_PER_NODE: i64 = 64;
pub const TIME_WINDOW_SECONDS: u64 = 3600;
pub const TIME_WINDOW_UNITS: &str = "route-link occurrences per node per hour";
const ASSIGNMENT_PURPOSE: &str = "partition.experiment.region_assignment";

pub struct PartitionExperimentResult {
    pub node_owners: IntMap<Id<Node>, u32>,
    pub baseline_node_owners: IntMap<Id<Node>, u32>,
    pub metadata: PartitionExperimentMetadata,
}

#[derive(Debug, Serialize)]
pub struct PartitionExperimentMetadata {
    pub input_network: String,
    pub source_revision: &'static str,
    pub assignment_revision: &'static str,
    pub worker_count: u32,
    pub region_count: u32,
    pub metis_contiguous: bool,
    pub coarse_partition_error: Option<String>,
    pub base_seed: u64,
    pub derived_seed: u64,
    pub evaluations_limit: u64,
    pub moves_limit: u64,
    pub sweeps_limit: u64,
    pub max_swap_source_containers: usize,
    pub sideways_budget: u64,
    pub advisory_time_limit_enabled: bool,
    pub workload_source: &'static str,
    pub input_ordering: &'static str,
    pub objective_weights: &'static str,
    pub memory_estimate_bytes_per_node: i64,
    pub time_window_workload: &'static str,
    pub selected_layout: &'static str,
    pub baseline_layout_id: String,
    pub baseline_network_path: Option<String>,
    pub baseline_network_fnv64: Option<String>,
    pub initial_layout_id: String,
    pub initial_region_representatives: Vec<String>,
    pub initial_region_assignment: Vec<u32>,
    pub candidate_layout_id: Option<String>,
    pub selected_layout_id: String,
    pub selected_network_fnv64: Option<String>,
    pub rejection_reasons: Vec<String>,
    pub baseline_max_work: u64,
    pub baseline_work_by_worker: Vec<u64>,
    pub candidate_max_work: Option<u64>,
    pub candidate_work_by_worker: Option<Vec<u64>>,
    pub baseline_memory_estimate_by_worker: Vec<u64>,
    pub candidate_memory_estimate_by_worker: Option<Vec<u64>>,
    pub baseline_edge_cut: u64,
    pub candidate_edge_cut: Option<u64>,
    pub baseline_max_neighbors: u32,
    pub candidate_max_neighbors: Option<u32>,
    pub baseline_neighbors_by_worker: Vec<u32>,
    pub candidate_neighbors_by_worker: Option<Vec<u32>>,
    pub time_window_start_seconds: Vec<u64>,
    pub time_window_sidecar: Option<String>,
    pub time_window_source_population: Option<String>,
    pub time_window_units: Option<String>,
    pub legs_without_departure_time: Option<u64>,
    pub baseline_time_window_work_by_worker: Vec<Vec<u64>>,
    pub candidate_time_window_work_by_worker: Option<Vec<Vec<u64>>>,
    pub solver_evaluations: Option<u64>,
    pub solver_moves: Option<u64>,
    pub solver_sweeps: Option<u64>,
    pub solver_stopped_by: Option<String>,
    pub solver_violation: Option<i64>,
    pub solver_error: Option<String>,
    pub baseline_preparation_nanos: u64,
    pub assignment_solver_nanos: u64,
    pub total_preparation_nanos: u64,
}

struct LayoutMetrics {
    max_work: u64,
    work_by_worker: Vec<u64>,
    max_memory: u64,
    memory_by_worker: Vec<u64>,
    edge_cut: u64,
    max_neighbors: u32,
    neighbors_by_worker: Vec<u32>,
    connected: bool,
    max_time_work_by_window: Vec<u64>,
    time_work_by_window: Vec<Vec<u64>>,
}

struct NetworkTopology {
    adjacency: BTreeMap<u64, BTreeSet<u64>>,
    component_by_node: BTreeMap<u64, usize>,
    connected: bool,
}

impl NetworkTopology {
    fn new(network: &Network) -> Self {
        let mut adjacency = network
            .nodes()
            .into_iter()
            .map(|node| (node.id.internal(), BTreeSet::new()))
            .collect::<BTreeMap<_, _>>();
        for link in network.links() {
            let from = link.from.internal();
            let to = link.to.internal();
            if from != to {
                adjacency.entry(from).or_default().insert(to);
                adjacency.entry(to).or_default().insert(from);
            }
        }
        let mut component_by_node = BTreeMap::new();
        let mut component = 0;
        for &start in adjacency.keys() {
            if component_by_node.contains_key(&start) {
                continue;
            }
            component_by_node.insert(start, component);
            let mut queue = VecDeque::from([start]);
            while let Some(node) = queue.pop_front() {
                for &neighbor in &adjacency[&node] {
                    if let std::collections::btree_map::Entry::Vacant(entry) =
                        component_by_node.entry(neighbor)
                    {
                        entry.insert(component);
                        queue.push_back(neighbor);
                    }
                }
            }
            component += 1;
        }
        Self {
            adjacency,
            component_by_node,
            connected: component <= 1,
        }
    }
}

struct AssignmentSolverStats {
    evaluations: u64,
    moves: u64,
    sweeps: u64,
    stopped_by: StopReason,
    violation: i64,
}

#[derive(Clone)]
struct Region {
    representative: String,
    nodes: Vec<Id<Node>>,
    work: i64,
    memory: i64,
    time_work: Vec<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TimeWindowDemand {
    pub start_seconds: u64,
    pub node_work_by_external_id: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TimeWindowDemandFile {
    pub source_population: String,
    pub window_seconds: u64,
    pub units: String,
    pub legs_without_departure_time: u64,
    pub windows: Vec<TimeWindowDemand>,
}

/// Partition a preweighted network into coarse METIS regions, then assign those regions to QSim
/// workers. `cmp_weight` is a route-occurrence count, and memory is estimated as 64 bytes per node;
/// neither dimension is measured CPU time or resident memory. The input network is not mutated.
pub fn prepare(
    network: &Network,
    input_network: &str,
    worker_count: u32,
    region_count: u32,
    base_seed: u64,
    options: &MetisOptions,
) -> Result<PartitionExperimentResult, String> {
    prepare_with_time_windows(
        network,
        input_network,
        worker_count,
        region_count,
        base_seed,
        options,
        &[],
        None,
        None,
    )
}

pub fn prepare_with_time_windows(
    network: &Network,
    input_network: &str,
    worker_count: u32,
    region_count: u32,
    base_seed: u64,
    options: &MetisOptions,
    time_windows: &[TimeWindowDemand],
    time_window_sidecar: Option<&str>,
    legs_without_departure_time: Option<u64>,
) -> Result<PartitionExperimentResult, String> {
    let preparation_started = Instant::now();
    if worker_count == 0 || region_count <= worker_count {
        return Err("coarse regions must outnumber at least one worker".to_string());
    }
    validate_time_windows(network, time_windows)?;
    validate_objective_magnitude(network, time_windows, region_count)?;
    let topology = NetworkTopology::new(network);
    let nodes = network.nodes();
    if region_count as usize > nodes.len() {
        return Err(format!(
            "region count {region_count} exceeds network node count {}",
            nodes.len()
        ));
    }

    // METIS's contiguous mode cannot handle a disconnected input graph.
    let metis_options = options
        .clone()
        .add_vertex_weight(VertexWeight::PreComputed)
        .set_contiguous(options.contiguous && topology.connected);
    let baseline_started = Instant::now();
    let baseline_raw = metis_partitioning::try_partition(network, worker_count, &metis_options)?;
    let baseline = baseline_raw
        .into_iter()
        .map(|(node, part)| {
            (
                node,
                u32::try_from(part).expect("METIS part IDs are nonnegative"),
            )
        })
        .collect::<IntMap<_, _>>();
    let baseline_metrics = measure_layout(
        network,
        &baseline,
        worker_count,
        options,
        time_windows,
        &topology,
    )?;
    if options.contiguous && !baseline_metrics.connected {
        return Err("weighted METIS baseline violates component-wise connectivity".to_string());
    }
    let baseline_preparation_nanos = elapsed_nanos(baseline_started);

    let (regions, coarse_partition_error) =
        match metis_partitioning::try_partition(network, region_count, &metis_options) {
            Ok(coarse_raw) => {
                match canonical_regions_with_windows(network, coarse_raw, time_windows) {
                    Ok(regions) => (regions, None),
                    Err(error) => (Vec::new(), Some(error)),
                }
            }
            Err(error) => (Vec::new(), Some(error)),
        };
    let region_count_mismatch = regions.len() != region_count as usize;
    let baseline_layout_id = layout_id(network, &baseline)?;
    let (initial, demands) = if region_count_mismatch {
        (Vec::new(), Vec::new())
    } else {
        initial_assignment(&regions, worker_count)?
    };
    let mut initial_owners = IntMap::default();
    for (region, &worker) in regions.iter().zip(&initial) {
        for node in &region.nodes {
            initial_owners.insert(node.clone(), worker);
        }
    }
    let initial_layout_id = if region_count_mismatch {
        baseline_layout_id.clone()
    } else {
        layout_id(network, &initial_owners)?
    };
    let derived_seed = get_rng(
        base_seed,
        ASSIGNMENT_PURPOSE,
        &format!("workers={worker_count};regions={region_count}"),
    )
    .random::<u64>();
    let limits = Limits {
        max_evaluations: 250_000,
        max_accepted_moves: 25_000,
        max_sweeps: 250,
        time: None,
        ..Limits::default()
    };
    let (candidate, solver_stats, solver_error, assignment_solver_nanos) = if region_count_mismatch
    {
        (None, None, None, 0)
    } else {
        let work_capacity = i64::try_from(baseline_metrics.max_work)
            .map_err(|_| "baseline work capacity exceeds solver range".to_string())?;
        let memory_capacity = i64::try_from(baseline_metrics.max_memory)
            .map_err(|_| "baseline memory capacity exceeds solver range".to_string())?;
        let mut specs = vec![
            Spec::capacity(0, work_capacity),
            Spec::capacity(1, memory_capacity),
            Spec::Balance { dim: 0, weight: 4 },
            Spec::Balance { dim: 1, weight: 1 },
            Spec::MinimizeMovement { weight: 1 },
        ];
        for (index, window_max) in baseline_metrics.max_time_work_by_window.iter().enumerate() {
            let dim = 2 + index;
            specs.push(Spec::capacity(
                dim,
                i64::try_from(*window_max)
                    .map_err(|_| "time-window work capacity exceeds solver range".to_string())?,
            ));
            specs.push(Spec::Balance { dim, weight: 1 });
        }
        let problem = Problem {
            num_objects: regions.len(),
            num_containers: worker_count as usize,
            num_dims: 2 + time_windows.len(),
            demands,
            initial: initial.clone(),
            specs,
        };
        let solver_started = Instant::now();
        let solver_result = solve(&problem, Seed(derived_seed), &limits);
        let assignment_solver_nanos = elapsed_nanos(solver_started);
        let (candidate, solver_stats, solver_error) = match solver_result {
            Ok(solution) => {
                let mut owners = IntMap::default();
                for (region, &worker) in regions.iter().zip(&solution.assignment.containers) {
                    for node in &region.nodes {
                        owners.insert(node.clone(), worker);
                    }
                }
                (
                    Some(owners),
                    Some(AssignmentSolverStats {
                        evaluations: solution.moves_evaluated,
                        moves: solution.moves_accepted,
                        sweeps: solution.sweeps,
                        stopped_by: solution.stopped_by,
                        violation: solution.violation_root,
                    }),
                    None,
                )
            }
            Err(error) => (None, None, Some(error.to_string())),
        };
        (
            candidate,
            solver_stats,
            solver_error,
            assignment_solver_nanos,
        )
    };

    let candidate_metrics_result = candidate.as_ref().map(|owners| {
        measure_layout(
            network,
            owners,
            worker_count,
            options,
            time_windows,
            &topology,
        )
    });
    let candidate_validation_error = candidate_metrics_result
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .map(ToString::to_string);
    let candidate_metrics = candidate_metrics_result.and_then(Result::ok);
    let mut rejection_reasons = candidate_rejection_reasons(
        &baseline_metrics,
        candidate_metrics.as_ref(),
        !region_count_mismatch,
        candidate_validation_error.as_deref(),
        time_windows.len(),
    );
    if region_count_mismatch && coarse_partition_error.is_none() {
        rejection_reasons.push(format!(
            "candidate rejected because METIS produced {} nonempty regions, expected {region_count}",
            regions.len()
        ));
    }
    if let Some(error) = &coarse_partition_error {
        rejection_reasons.push(format!("coarse METIS partition rejected: {error}"));
    }
    if let Some(stats) = &solver_stats {
        if solver_budget_exhausted(stats.stopped_by) {
            rejection_reasons.push(format!(
                "assignment solver budget exhausted: {:?}",
                stats.stopped_by
            ));
        }
    }
    let candidate_layout_id = candidate
        .as_ref()
        .and_then(|owners| layout_id(network, owners).ok());
    if candidate_metrics.is_some() && candidate_layout_id.is_none() {
        rejection_reasons.push("candidate ownership has no stable layout identity".to_string());
    }
    let accepted = should_accept_candidate(candidate.is_some(), &rejection_reasons);
    let selected_layout_id = if accepted {
        candidate_layout_id
            .clone()
            .expect("accepted candidate has an identity")
    } else {
        baseline_layout_id.clone()
    };
    let selected = if accepted {
        candidate.expect("accepted candidate exists")
    } else {
        baseline.clone()
    };
    let metadata = PartitionExperimentMetadata {
        input_network: input_network.to_string(),
        source_revision: GIT_VERSION,
        assignment_revision: ASSIGNMENT_REVISION,
        worker_count,
        region_count,
        metis_contiguous: metis_options.contiguous,
        coarse_partition_error,
        base_seed,
        derived_seed,
        evaluations_limit: limits.max_evaluations,
        moves_limit: limits.max_accepted_moves,
        sweeps_limit: limits.max_sweeps,
        max_swap_source_containers: limits.max_swap_source_containers,
        sideways_budget: limits.sideways_budget,
        advisory_time_limit_enabled: limits.time.is_some(),
        workload_source: "network cmp_weight, accumulated route-link occurrences",
        input_ordering: "regions assigned by descending work then minimum external node ID; each goes to the lowest-work worker, ties by worker ID",
        objective_weights: "work balance=4, memory balance=1, hourly work balance=1 per window, movement penalty=1",
        memory_estimate_bytes_per_node: MEMORY_ESTIMATE_BYTES_PER_NODE,
        time_window_workload: if time_windows.is_empty() {
            "unavailable: no workload-window sidecar supplied"
        } else {
            "hourly route-link occurrences grouped by planned departure time"
        },
        selected_layout: if accepted { "candidate" } else { "baseline" },
        baseline_layout_id,
        baseline_network_path: None,
        baseline_network_fnv64: None,
        initial_layout_id,
        initial_region_representatives: regions
            .iter()
            .map(|region| region.representative.clone())
            .collect(),
        initial_region_assignment: initial,
        candidate_layout_id,
        selected_layout_id,
        selected_network_fnv64: None,
        rejection_reasons,
        baseline_max_work: baseline_metrics.max_work,
        baseline_work_by_worker: baseline_metrics.work_by_worker.clone(),
        candidate_max_work: candidate_metrics.as_ref().map(|metrics| metrics.max_work),
        candidate_work_by_worker: candidate_metrics
            .as_ref()
            .map(|metrics| metrics.work_by_worker.clone()),
        baseline_memory_estimate_by_worker: baseline_metrics.memory_by_worker.clone(),
        candidate_memory_estimate_by_worker: candidate_metrics
            .as_ref()
            .map(|metrics| metrics.memory_by_worker.clone()),
        baseline_edge_cut: baseline_metrics.edge_cut,
        candidate_edge_cut: candidate_metrics.as_ref().map(|metrics| metrics.edge_cut),
        baseline_max_neighbors: baseline_metrics.max_neighbors,
        candidate_max_neighbors: candidate_metrics
            .as_ref()
            .map(|metrics| metrics.max_neighbors),
        baseline_neighbors_by_worker: baseline_metrics.neighbors_by_worker.clone(),
        candidate_neighbors_by_worker: candidate_metrics
            .as_ref()
            .map(|metrics| metrics.neighbors_by_worker.clone()),
        time_window_start_seconds: time_windows
            .iter()
            .map(|window| window.start_seconds)
            .collect(),
        time_window_sidecar: time_window_sidecar.map(str::to_string),
        time_window_source_population: None,
        time_window_units: None,
        legs_without_departure_time,
        baseline_time_window_work_by_worker: transpose_window_loads(
            &baseline_metrics.time_work_by_window,
        ),
        candidate_time_window_work_by_worker: candidate_metrics
            .as_ref()
            .map(|metrics| transpose_window_loads(&metrics.time_work_by_window)),
        solver_evaluations: solver_stats.as_ref().map(|stats| stats.evaluations),
        solver_moves: solver_stats.as_ref().map(|stats| stats.moves),
        solver_sweeps: solver_stats.as_ref().map(|stats| stats.sweeps),
        solver_stopped_by: solver_stats
            .as_ref()
            .map(|stats| format!("{:?}", stats.stopped_by)),
        solver_violation: solver_stats.as_ref().map(|stats| stats.violation),
        solver_error,
        baseline_preparation_nanos,
        assignment_solver_nanos,
        total_preparation_nanos: elapsed_nanos(preparation_started),
    };
    Ok(PartitionExperimentResult {
        node_owners: selected,
        baseline_node_owners: baseline,
        metadata,
    })
}

fn candidate_rejection_reasons(
    baseline: &LayoutMetrics,
    candidate: Option<&LayoutMetrics>,
    solver_attempted: bool,
    validation_error: Option<&str>,
    time_window_count: usize,
) -> Vec<String> {
    let mut reasons = Vec::new();
    match (solver_attempted, candidate) {
        (false, _) => {}
        (true, None) => reasons.push(match validation_error {
            Some(error) => format!("candidate ownership failed validation: {error}"),
            None => "assignment solver rejected the problem".to_string(),
        }),
        (true, Some(candidate)) => {
            if candidate.max_work >= baseline.max_work {
                reasons.push("maximum estimated work did not improve".to_string());
            }
            if candidate.max_work > baseline.max_work
                || candidate.max_memory > baseline.max_memory
                || candidate
                    .max_time_work_by_window
                    .iter()
                    .zip(&baseline.max_time_work_by_window)
                    .take(time_window_count)
                    .any(|(candidate, baseline)| candidate > baseline)
            {
                reasons.push("candidate exceeds baseline load capacity".to_string());
            }
            if candidate.edge_cut > baseline.edge_cut {
                reasons.push("weighted edge cut is worse than baseline".to_string());
            }
            if candidate.max_neighbors > baseline.max_neighbors {
                reasons.push("maximum neighbor count is worse than baseline".to_string());
            }
            if !candidate.connected {
                reasons.push("candidate partitions are disconnected".to_string());
            }
        }
    }
    reasons
}

fn should_accept_candidate(candidate_exists: bool, rejection_reasons: &[String]) -> bool {
    candidate_exists && rejection_reasons.is_empty()
}

fn solver_budget_exhausted(stopped_by: StopReason) -> bool {
    stopped_by != StopReason::NoImprovingMove
}

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn layout_id(network: &Network, owners: &IntMap<Id<Node>, u32>) -> Result<String, String> {
    let mut nodes = network.nodes();
    if owners.len() != nodes.len() {
        return Err("layout identity requires complete node ownership".to_string());
    }
    nodes.sort_by_key(|node| node.id.external().to_string());
    let mut hasher = FnvHasher::default();
    hasher.write(b"matsim.partition-layout.v1");
    for node in nodes {
        let external_id = node.id.external().as_bytes();
        hasher.write(&(external_id.len() as u64).to_le_bytes());
        hasher.write(external_id);
        hasher.write(
            &owners
                .get(&node.id)
                .ok_or_else(|| "layout identity is missing a node owner".to_string())?
                .to_le_bytes(),
        );
    }
    Ok(format!("{:016x}", hasher.finish()))
}

fn validate_objective_magnitude(
    network: &Network,
    time_windows: &[TimeWindowDemand],
    region_count: u32,
) -> Result<(), String> {
    let mut totals = vec![0u128; 2 + time_windows.len()];
    totals[0] = network
        .nodes()
        .into_iter()
        .map(|node| node.cmp_weight as u128)
        .sum();
    totals[1] = network.nodes().len() as u128 * MEMORY_ESTIMATE_BYTES_PER_NODE as u128;
    for (index, window) in time_windows.iter().enumerate() {
        totals[index + 2] = window
            .node_work_by_external_id
            .values()
            .try_fold(0u128, |total, &work| total.checked_add(work as u128))
            .ok_or_else(|| "time-window workload exceeds solver range".to_string())?;
    }
    let total_capacity_violation = totals
        .iter()
        .try_fold(0u128, |total, &demand| total.checked_add(demand))
        .and_then(|total| total.checked_mul(katgpt_assign::FIX_IT_WEIGHT as u128))
        .ok_or_else(|| "assignment capacity objective exceeds solver range".to_string())?;
    let max_capacity_violation =
        totals.iter().copied().max().unwrap_or(0) * katgpt_assign::FIX_IT_WEIGHT as u128;
    let goal_bound = totals[0]
        .checked_mul(4)
        .and_then(|total| {
            totals
                .iter()
                .skip(1)
                .try_fold(total, |sum, &demand| sum.checked_add(demand))
        })
        .and_then(|total| total.checked_add(region_count as u128))
        .ok_or_else(|| "assignment objective exceeds solver range".to_string())?;
    let folded_bound = max_capacity_violation
        .checked_mul(NEVER_WORSE_GUARD as u128)
        .and_then(|guarded| guarded.checked_add(total_capacity_violation))
        .and_then(|total| total.checked_add(goal_bound))
        .ok_or_else(|| "assignment objective exceeds solver range".to_string())?;
    if folded_bound > i64::MAX as u128 {
        return Err("assignment objective magnitude exceeds solver range".to_string());
    }
    Ok(())
}

fn canonical_regions_with_windows(
    network: &Network,
    raw_owners: IntMap<Id<Node>, metis::Idx>,
    time_windows: &[TimeWindowDemand],
) -> Result<Vec<Region>, String> {
    let mut grouped = BTreeMap::<metis::Idx, Vec<Id<Node>>>::new();
    for (node, region) in raw_owners {
        grouped.entry(region).or_default().push(node);
    }
    let mut regions = grouped
        .into_values()
        .map(|mut nodes| {
            nodes.sort_by_key(|id| id.external().to_string());
            let representative = nodes[0].external().to_string();
            let mut work = 0i64;
            for node in &nodes {
                work = work
                    .checked_add(i64::from(network.get_node(node).cmp_weight))
                    .ok_or_else(|| "region work exceeds solver range".to_string())?;
            }
            let memory = i64::try_from(nodes.len())
                .ok()
                .and_then(|count| count.checked_mul(MEMORY_ESTIMATE_BYTES_PER_NODE))
                .ok_or_else(|| "region memory estimate exceeds solver range".to_string())?;
            let mut time_work = Vec::with_capacity(time_windows.len());
            for window in time_windows {
                let mut work = 0u64;
                for node in &nodes {
                    work = work
                        .checked_add(
                            window
                                .node_work_by_external_id
                                .get(&node.external().to_string())
                                .copied()
                                .unwrap_or_default(),
                        )
                        .ok_or_else(|| "region time-window work overflow".to_string())?;
                }
                time_work.push(
                    i64::try_from(work)
                        .map_err(|_| "region time-window work exceeds solver range".to_string())?,
                );
            }
            Ok(Region {
                representative,
                nodes,
                work,
                memory,
                time_work,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    regions.sort_by(|left, right| left.representative.cmp(&right.representative));
    Ok(regions)
}

fn initial_assignment(regions: &[Region], workers: u32) -> Result<(Vec<u32>, Vec<i64>), String> {
    let mut order = (0..regions.len()).collect::<Vec<_>>();
    order.sort_by_key(|&region| {
        (
            std::cmp::Reverse(regions[region].work),
            regions[region].representative.clone(),
        )
    });
    let mut assignments = vec![0; regions.len()];
    let mut loads = vec![0i64; workers as usize];
    for region in order {
        let worker = loads
            .iter()
            .enumerate()
            .min_by_key(|(worker, load)| (**load, *worker))
            .map(|(worker, _)| worker)
            .expect("worker count is validated as positive");
        assignments[region] = worker as u32;
        loads[worker] = loads[worker]
            .checked_add(regions[region].work)
            .ok_or_else(|| "initial work load exceeds solver range".to_string())?;
    }
    let window_count = regions.first().map_or(0, |region| region.time_work.len());
    let mut demands = Vec::with_capacity(regions.len() * (2 + window_count));
    for region in regions {
        demands.extend([region.work, region.memory]);
        demands.extend(region.time_work.iter().copied());
    }
    Ok((assignments, demands))
}

fn measure_layout(
    network: &Network,
    owners: &IntMap<Id<Node>, u32>,
    workers: u32,
    options: &MetisOptions,
    time_windows: &[TimeWindowDemand],
    topology: &NetworkTopology,
) -> Result<LayoutMetrics, String> {
    if owners.len() != network.nodes().len() {
        return Err("ownership does not cover every network node".to_string());
    }
    let mut loads = vec![0u64; workers as usize];
    let mut memory = vec![0u64; workers as usize];
    let mut time_work = vec![vec![0u64; workers as usize]; time_windows.len()];
    let mut members = vec![BTreeSet::<u64>::new(); workers as usize];
    let mut owner_by_internal = BTreeMap::new();
    for node in network.nodes() {
        let worker = usize::try_from(*owners.get(&node.id).ok_or("node has no owner")?)
            .map_err(|_| "worker ID does not fit usize".to_string())?;
        if worker >= workers as usize {
            return Err("node owner is outside the worker range".to_string());
        }
        loads[worker] = loads[worker]
            .checked_add(u64::from(node.cmp_weight))
            .ok_or_else(|| "worker work load overflow".to_string())?;
        memory[worker] = memory[worker]
            .checked_add(MEMORY_ESTIMATE_BYTES_PER_NODE as u64)
            .ok_or_else(|| "worker memory estimate overflow".to_string())?;
        let external_id = node.id.external().to_string();
        for (index, window) in time_windows.iter().enumerate() {
            time_work[index][worker] = time_work[index][worker]
                .checked_add(
                    window
                        .node_work_by_external_id
                        .get(&external_id)
                        .copied()
                        .unwrap_or_default(),
                )
                .ok_or_else(|| "worker time-window work overflow".to_string())?;
        }
        members[worker].insert(node.id.internal());
        owner_by_internal.insert(node.id.internal(), worker as u32);
    }

    let mut edge_weights = BTreeMap::<(u64, u64), u64>::new();
    for link in network.links() {
        let from = link.from.internal();
        let to = link.to.internal();
        if from == to {
            continue;
        }
        let edge = if from < to { (from, to) } else { (to, from) };
        let weight = u64::try_from(get_adjwgt(options, link)?)
            .map_err(|_| "METIS edge weight is negative".to_string())?;
        let edge_weight = edge_weights.entry(edge).or_default();
        *edge_weight = edge_weight
            .checked_add(weight)
            .ok_or_else(|| "weighted edge cut overflow".to_string())?;
    }

    let mut cut = 0u64;
    let mut neighbors = vec![BTreeSet::new(); workers as usize];
    for (&(from, to), &weight) in &edge_weights {
        let from_owner = *owner_by_internal
            .get(&from)
            .ok_or("edge source has no owner")?;
        let to_owner = *owner_by_internal
            .get(&to)
            .ok_or("edge destination has no owner")?;
        if from_owner != to_owner {
            cut = cut
                .checked_add(weight)
                .ok_or_else(|| "weighted edge cut overflow".to_string())?;
            neighbors[from_owner as usize].insert(to_owner);
            neighbors[to_owner as usize].insert(from_owner);
        }
    }

    let connected = !options.contiguous
        || members.iter().all(|partition| {
            let mut members_by_component = BTreeMap::<usize, BTreeSet<u64>>::new();
            for &node in partition {
                members_by_component
                    .entry(topology.component_by_node[&node])
                    .or_default()
                    .insert(node);
            }
            members_by_component.values().all(|component_members| {
                let Some(&start) = component_members.first() else {
                    return true;
                };
                let mut visited = BTreeSet::from([start]);
                let mut queue = VecDeque::from([start]);
                while let Some(node) = queue.pop_front() {
                    for &neighbor in &topology.adjacency[&node] {
                        if component_members.contains(&neighbor) && visited.insert(neighbor) {
                            queue.push_back(neighbor);
                        }
                    }
                }
                visited.len() == component_members.len()
            })
        });

    let neighbors_by_worker = neighbors
        .iter()
        .map(|set| {
            u32::try_from(set.len())
                .map_err(|_| "worker neighbor count exceeds u32::MAX".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let max_neighbors = neighbors_by_worker.iter().copied().max().unwrap_or(0);
    Ok(LayoutMetrics {
        max_work: loads.iter().copied().max().unwrap_or(0),
        work_by_worker: loads,
        max_memory: memory.iter().copied().max().unwrap_or(0),
        memory_by_worker: memory,
        edge_cut: cut,
        max_neighbors,
        neighbors_by_worker,
        connected,
        max_time_work_by_window: time_work
            .iter()
            .map(|loads| loads.iter().copied().max().unwrap_or(0))
            .collect(),
        time_work_by_window: time_work,
    })
}

fn validate_time_windows(network: &Network, windows: &[TimeWindowDemand]) -> Result<(), String> {
    let node_ids = network
        .nodes()
        .into_iter()
        .map(|node| node.id.external().to_string())
        .collect::<BTreeSet<_>>();
    let mut previous = None;
    for window in windows {
        if window.start_seconds % TIME_WINDOW_SECONDS != 0
            || previous.is_some_and(|time| time >= window.start_seconds)
        {
            return Err("time windows must be unique and sorted hourly start times".to_string());
        }
        previous = Some(window.start_seconds);
        for (node, work) in &window.node_work_by_external_id {
            if !node_ids.contains(node) {
                return Err(format!("time-window demand references unknown node {node}"));
            }
            i64::try_from(*work)
                .map_err(|_| format!("time-window demand for node {node} exceeds solver range"))?;
        }
    }
    Ok(())
}

fn transpose_window_loads(windows: &[Vec<u64>]) -> Vec<Vec<u64>> {
    let workers = windows.first().map_or(0, Vec::len);
    (0..workers)
        .map(|worker| windows.iter().map(|window| window[worker]).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        LayoutMetrics, NetworkTopology, Region, TimeWindowDemand, candidate_rejection_reasons,
        canonical_regions_with_windows, initial_assignment, measure_layout, prepare,
        should_accept_candidate, solver_budget_exhausted, validate_objective_magnitude,
        validate_time_windows,
    };
    use crate::simulation::config::{EdgeWeight, MetisOptions};
    use crate::simulation::id::Id;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Network, Node};
    use ahash::HashMapExt;
    use katgpt_assign::StopReason;
    use macros::deterministic_id_test;
    use nohash_hasher::IntMap;
    use std::collections::BTreeMap;

    #[deterministic_id_test]
    fn canonical_regions_ignore_metis_region_labels_and_map_iteration_order() {
        let mut network = Network::new();
        let nodes = [
            (Id::create("region-a"), 3),
            (Id::create("region-b"), 8),
            (Id::create("region-c"), 4),
        ];
        for (index, (id, work)) in nodes.iter().enumerate() {
            network.add_node(Node::new(
                id.clone(),
                Coordinate::new_2d(index as f64, 0.),
                0,
                *work,
            ));
        }
        let mut first_labels = IntMap::new();
        first_labels.insert(nodes[0].0.clone(), 1);
        first_labels.insert(nodes[1].0.clone(), 0);
        first_labels.insert(nodes[2].0.clone(), 1);
        let mut renamed_labels = IntMap::new();
        renamed_labels.insert(nodes[2].0.clone(), 9);
        renamed_labels.insert(nodes[1].0.clone(), 8);
        renamed_labels.insert(nodes[0].0.clone(), 9);

        let window = TimeWindowDemand {
            start_seconds: 0,
            node_work_by_external_id: BTreeMap::from([
                ("region-a".to_string(), 2),
                ("region-b".to_string(), 8),
                ("region-c".to_string(), 5),
            ]),
        };
        let first =
            canonical_regions_with_windows(&network, first_labels, &[window.clone()]).unwrap();
        let renamed = canonical_regions_with_windows(&network, renamed_labels, &[window]).unwrap();

        assert_eq!(first[0].representative, renamed[0].representative);
        assert_eq!(first[1].representative, renamed[1].representative);
        assert_eq!(first[0].work, 7);
        assert_eq!(first[0].memory, 128);
        assert_eq!(first[0].time_work, vec![7]);
        assert_eq!(first[1].time_work, vec![8]);
        assert_eq!(
            first
                .iter()
                .map(|region| region.representative.clone())
                .collect::<Vec<_>>(),
            renamed
                .iter()
                .map(|region| region.representative.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn initial_region_assignment_is_stable_when_region_input_order_changes() {
        let regions = vec![
            Region {
                representative: "node-g".to_string(),
                nodes: Vec::new(),
                work: 9,
                memory: 64,
                time_work: Vec::new(),
            },
            Region {
                representative: "node-b".to_string(),
                nodes: Vec::new(),
                work: 9,
                memory: 64,
                time_work: Vec::new(),
            },
            Region {
                representative: "node-e".to_string(),
                nodes: Vec::new(),
                work: 2,
                memory: 64,
                time_work: Vec::new(),
            },
        ];
        let first = initial_assignment(&regions, 2).unwrap().0;
        let reordered = vec![regions[2].clone(), regions[0].clone(), regions[1].clone()];
        let second = initial_assignment(&reordered, 2).unwrap().0;
        let assignments = regions
            .iter()
            .zip(first)
            .map(|(region, worker)| (region.representative.clone(), worker))
            .collect::<BTreeMap<_, _>>();
        let reordered_assignments = reordered
            .iter()
            .zip(second)
            .map(|(region, worker)| (region.representative.clone(), worker))
            .collect::<BTreeMap<_, _>>();

        assert_eq!(assignments, reordered_assignments);
    }

    #[test]
    fn region_count_must_exceed_a_positive_worker_count() {
        let network = Network::new();

        assert!(
            prepare(
                &network,
                "network.xml",
                0,
                1,
                4711,
                &MetisOptions::default()
            )
            .is_err()
        );
        assert!(
            prepare(
                &network,
                "network.xml",
                2,
                2,
                4711,
                &MetisOptions::default()
            )
            .is_err()
        );
    }

    #[deterministic_id_test]
    fn disconnected_network_uses_baseline_and_records_contiguity_fallback() {
        let mut network = Network::new();
        let nodes = ["a", "b", "c", "d"].map(|name| {
            let id = Id::create(name);
            network.add_node(Node::new(id.clone(), Coordinate::new_2d(0., 0.), 0, 1));
            id
        });
        for (index, (from, to)) in [(0, 1), (2, 3)].into_iter().enumerate() {
            let link_id = Id::create(["disconnected-link-0", "disconnected-link-1"][index]);
            network.add_link(Link::new_with_default(
                link_id,
                network.get_node(&nodes[from]),
                network.get_node(&nodes[to]),
            ));
        }

        let result = prepare(
            &network,
            "disconnected.xml",
            1,
            2,
            4711,
            &MetisOptions::default(),
        )
        .unwrap();

        assert!(!result.metadata.metis_contiguous);
        assert_eq!(result.metadata.selected_layout, "baseline");
        assert!(
            result
                .metadata
                .rejection_reasons
                .iter()
                .any(|reason| reason == "maximum estimated work did not improve")
        );

        let multiple_workers = prepare(
            &network,
            "disconnected.xml",
            2,
            3,
            4711,
            &MetisOptions::default(),
        )
        .unwrap();
        assert!(!multiple_workers.metadata.metis_contiguous);
        assert_eq!(multiple_workers.baseline_node_owners.len(), 4);
    }

    #[deterministic_id_test]
    fn time_window_input_rejects_unknown_nodes_and_unaligned_windows() {
        let mut network = Network::new();
        let node = Id::create("window-node");
        network.add_node(Node::new(node, Coordinate::new_2d(0., 0.), 0, 1));
        let unknown_node = TimeWindowDemand {
            start_seconds: 0,
            node_work_by_external_id: BTreeMap::from([("missing".to_string(), 1)]),
        };
        assert!(validate_time_windows(&network, &[unknown_node]).is_err());
        let unaligned = TimeWindowDemand {
            start_seconds: 1,
            node_work_by_external_id: BTreeMap::new(),
        };
        assert!(validate_time_windows(&network, &[unaligned]).is_err());
    }

    #[test]
    fn objective_bound_rejects_large_time_window_inputs_before_metis() {
        let network = Network::new();
        let window = TimeWindowDemand {
            start_seconds: 0,
            node_work_by_external_id: BTreeMap::from([("node".to_string(), u64::MAX)]),
        };
        assert!(validate_objective_magnitude(&network, &[window], 2).is_err());
    }

    #[test]
    fn candidate_gate_accepts_improvement_and_falls_back_when_constraints_fail() {
        let baseline = LayoutMetrics {
            max_work: 100,
            work_by_worker: vec![100, 90],
            max_memory: 640,
            memory_by_worker: vec![640, 512],
            edge_cut: 20,
            max_neighbors: 3,
            neighbors_by_worker: vec![2, 3],
            connected: true,
            max_time_work_by_window: vec![12],
            time_work_by_window: vec![vec![12, 8]],
        };
        let improved = LayoutMetrics {
            max_work: 90,
            work_by_worker: vec![90, 85],
            max_memory: 600,
            memory_by_worker: vec![600, 512],
            edge_cut: 18,
            max_neighbors: 3,
            neighbors_by_worker: vec![2, 3],
            connected: true,
            max_time_work_by_window: vec![10],
            time_work_by_window: vec![vec![10, 9]],
        };
        let reasons = candidate_rejection_reasons(&baseline, Some(&improved), true, None, 1);
        assert!(reasons.is_empty());
        assert!(should_accept_candidate(true, &reasons));

        let worse = LayoutMetrics {
            max_work: 100,
            connected: false,
            max_time_work_by_window: vec![13],
            ..improved
        };
        let reasons = candidate_rejection_reasons(&baseline, Some(&worse), true, None, 1);
        assert!(
            reasons
                .iter()
                .any(|reason| reason == "maximum estimated work did not improve")
        );
        assert!(
            reasons
                .iter()
                .any(|reason| reason == "candidate partitions are disconnected")
        );
        assert!(
            reasons
                .iter()
                .any(|reason| reason == "candidate exceeds baseline load capacity")
        );
        assert!(!should_accept_candidate(true, &reasons));
        assert!(!should_accept_candidate(false, &[]));
    }

    #[deterministic_id_test]
    fn disconnected_partitions_check_connectivity_within_each_network_component() {
        let mut network = Network::new();
        let nodes = ["component-a", "component-b", "component-c", "component-d"].map(|name| {
            let id = Id::create(name);
            network.add_node(Node::new(id.clone(), Coordinate::new_2d(0., 0.), 0, 1));
            id
        });
        for (link_name, from, to) in [("component-link-a", 0, 1), ("component-link-b", 2, 3)] {
            network.add_link(Link::new_with_default(
                Id::create(link_name),
                network.get_node(&nodes[from]),
                network.get_node(&nodes[to]),
            ));
        }
        let mut owners = IntMap::default();
        for (node, owner) in nodes.iter().zip([0, 1, 1, 0]) {
            owners.insert(node.clone(), owner);
        }

        let topology = NetworkTopology::new(&network);
        let metrics = measure_layout(
            &network,
            &owners,
            2,
            &MetisOptions::default(),
            &[],
            &topology,
        )
        .unwrap();

        assert!(metrics.connected);
    }

    #[deterministic_id_test]
    fn capacity_edge_metrics_reject_nonfinite_input() {
        let mut network = Network::new();
        let from = Id::create("metrics-capacity-from");
        let to = Id::create("metrics-capacity-to");
        network.add_node(Node::new(from.clone(), Coordinate::new_2d(0., 0.), 0, 1));
        network.add_node(Node::new(to.clone(), Coordinate::new_2d(1., 0.), 0, 1));
        let link = Id::create("metrics-capacity-link");
        network.add_link(Link::new_with_default(
            link.clone(),
            network.get_node(&from),
            network.get_node(&to),
        ));
        network.get_link_mut(&link).capacity = f64::NAN;
        let owners = IntMap::from_iter([(from, 0), (to, 1)]);
        let topology = NetworkTopology::new(&network);

        assert!(
            measure_layout(
                &network,
                &owners,
                2,
                &MetisOptions::default().set_edge_weight(EdgeWeight::Capacity),
                &[],
                &topology,
            )
            .is_err()
        );
    }

    #[test]
    fn assignment_budget_stops_reject_the_candidate() {
        for reason in [
            StopReason::MoveLimit,
            StopReason::EvalLimit,
            StopReason::SweepLimit,
            StopReason::TimeLimit,
        ] {
            assert!(solver_budget_exhausted(reason));
        }
        assert!(!solver_budget_exhausted(StopReason::NoImprovingMove));
    }
}
