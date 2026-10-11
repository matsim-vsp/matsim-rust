//! Opt-in, fixed-time-window QSim partition diagnostics.

use crate::simulation::time::SimTime;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Default)]
pub struct PartitionDiagnostics {
    // Each partition writes only its own shard, so diagnostics do not serialize QSim workers.
    windows: Vec<Mutex<BTreeMap<(u32, u64), Window>>>,
}

#[derive(Debug, Default)]
struct Window {
    work_nanos: u64,
    wait_nanos: u64,
    active_network_work: u64,
    vehicle_transitions: u64,
    migrations: u64,
    inter_partition_messages: u64,
}

impl PartitionDiagnostics {
    pub fn new(workers: u32) -> Self {
        Self {
            windows: (0..workers).map(|_| Mutex::new(BTreeMap::new())).collect(),
        }
    }

    pub fn record_step(&self, worker: u32, iteration: u32, time: SimTime, step_nanos: u64) {
        let mut windows = self.windows[worker as usize].lock().unwrap();
        let entry = windows
            .entry((iteration, time.as_secs() / 3600))
            .or_default();
        entry.work_nanos = entry.work_nanos.saturating_add(step_nanos);
    }

    pub fn record_vehicle_transitions(
        &self,
        worker: u32,
        iteration: u32,
        time: SimTime,
        transitions: u64,
    ) {
        if transitions == 0 {
            return;
        }
        let mut windows = self.windows[worker as usize].lock().unwrap();
        let entry = windows
            .entry((iteration, time.as_secs() / 3600))
            .or_default();
        entry.vehicle_transitions = entry.vehicle_transitions.saturating_add(transitions);
    }

    pub fn record_active_network_work(
        &self,
        worker: u32,
        iteration: u32,
        time: SimTime,
        active_vehicles: usize,
    ) {
        let mut windows = self.windows[worker as usize].lock().unwrap();
        let entry = windows
            .entry((iteration, time.as_secs() / 3600))
            .or_default();
        entry.active_network_work = entry
            .active_network_work
            .saturating_add(u64::try_from(active_vehicles).unwrap_or(u64::MAX));
    }

    pub fn record_migration(&self, worker: u32, iteration: u32, time: SimTime) {
        let mut windows = self.windows[worker as usize].lock().unwrap();
        let entry = windows
            .entry((iteration, time.as_secs() / 3600))
            .or_default();
        entry.migrations = entry.migrations.saturating_add(1);
    }

    pub fn record_communication(
        &self,
        worker: u32,
        iteration: u32,
        time: SimTime,
        messages: u64,
        wait_nanos: u64,
    ) {
        let mut windows = self.windows[worker as usize].lock().unwrap();
        let entry = windows
            .entry((iteration, time.as_secs() / 3600))
            .or_default();
        entry.inter_partition_messages = entry.inter_partition_messages.saturating_add(messages);
        entry.wait_nanos = entry.wait_nanos.saturating_add(wait_nanos);
    }

    pub fn write_csv(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(path)?;
        writeln!(
            file,
            "iteration,worker,time_window_start_seconds,work_nanos,wait_nanos,active_network_work,vehicle_transitions,migrations,inter_partition_messages"
        )?;
        for (worker, worker_windows) in self.windows.iter().enumerate() {
            let windows = worker_windows.lock().unwrap();
            for ((iteration, hour), row) in windows.iter() {
                writeln!(
                    file,
                    "{iteration},{worker},{},{},{},{},{},{},{}",
                    hour * 3600,
                    row.work_nanos.saturating_sub(row.wait_nanos),
                    row.wait_nanos,
                    row.active_network_work,
                    row.vehicle_transitions,
                    row.migrations,
                    row.inter_partition_messages
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::PartitionDiagnostics;
    use crate::simulation::time::SimTime;

    #[test]
    fn aggregates_fixed_hour_windows_by_worker_and_iteration() {
        let diagnostics = PartitionDiagnostics::new(1);
        diagnostics.record_step(0, 2, SimTime::from_secs(3599), 100);
        diagnostics.record_step(0, 2, SimTime::from_secs(3600), 200);
        diagnostics.record_communication(0, 2, SimTime::from_secs(3599), 0, 30);
        diagnostics.record_active_network_work(0, 2, SimTime::from_secs(3599), 3);
        diagnostics.record_vehicle_transitions(0, 2, SimTime::from_secs(3600), 1);
        diagnostics.record_migration(0, 2, SimTime::from_secs(3600));
        diagnostics.record_communication(0, 2, SimTime::from_secs(3600), 3, 50);

        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("diagnostics.csv");
        diagnostics.write_csv(&output).unwrap();
        let contents = std::fs::read_to_string(output).unwrap();
        assert!(contents.contains("2,0,0,70,30,3,0,0,0"));
        assert!(contents.contains("2,0,3600,150,50,0,1,1,3"));
    }
}
