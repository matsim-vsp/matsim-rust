//! Opt-in, fixed-time-window QSim partition diagnostics.

use crate::simulation::time::SimTime;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Default)]
pub struct PartitionDiagnostics {
    windows: Mutex<BTreeMap<(u32, u32, u64), Window>>,
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
    pub fn record_step(&self, worker: u32, iteration: u32, time: SimTime, step_nanos: u64) {
        let mut windows = self.windows.lock().unwrap();
        let entry = windows
            .entry((iteration, worker, time.as_secs() / 3600))
            .or_default();
        entry.work_nanos = entry.work_nanos.saturating_add(step_nanos);
    }

    pub fn record_network_event(
        &self,
        worker: u32,
        iteration: u32,
        time: SimTime,
        transition: bool,
    ) {
        let mut windows = self.windows.lock().unwrap();
        let entry = windows
            .entry((iteration, worker, time.as_secs() / 3600))
            .or_default();
        if transition {
            entry.vehicle_transitions = entry.vehicle_transitions.saturating_add(1);
        } else {
            entry.active_network_work = entry.active_network_work.saturating_add(1);
        }
    }

    pub fn record_migration(&self, worker: u32, iteration: u32, time: SimTime) {
        let mut windows = self.windows.lock().unwrap();
        let entry = windows
            .entry((iteration, worker, time.as_secs() / 3600))
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
        let mut windows = self.windows.lock().unwrap();
        let entry = windows
            .entry((iteration, worker, time.as_secs() / 3600))
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
        let windows = self.windows.lock().unwrap();
        let mut file = std::fs::File::create(path)?;
        writeln!(
            file,
            "iteration,worker,time_window_start_seconds,work_nanos,wait_nanos,active_network_work,vehicle_transitions,migrations,inter_partition_messages"
        )?;
        for ((iteration, worker, hour), row) in windows.iter() {
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::PartitionDiagnostics;
    use crate::simulation::time::SimTime;

    #[test]
    fn aggregates_fixed_hour_windows_by_worker_and_iteration() {
        let diagnostics = PartitionDiagnostics::default();
        diagnostics.record_step(0, 2, SimTime::from_secs(3599), 100);
        diagnostics.record_step(0, 2, SimTime::from_secs(3600), 200);
        diagnostics.record_communication(0, 2, SimTime::from_secs(3599), 0, 30);
        diagnostics.record_network_event(0, 2, SimTime::from_secs(3599), false);
        diagnostics.record_network_event(0, 2, SimTime::from_secs(3600), true);
        diagnostics.record_migration(0, 2, SimTime::from_secs(3600));
        diagnostics.record_communication(0, 2, SimTime::from_secs(3600), 3, 50);

        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("diagnostics.csv");
        diagnostics.write_csv(&output).unwrap();
        let contents = std::fs::read_to_string(output).unwrap();
        assert!(contents.contains("2,0,0,70,30,1,0,0,0"));
        assert!(contents.contains("2,0,3600,150,50,0,1,1,3"));
    }
}
