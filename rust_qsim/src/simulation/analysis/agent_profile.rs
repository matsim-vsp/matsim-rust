//! Observed in-trip person counts by analysis interval.

use crate::simulation::analysis::{AnalysisError, hour_start_seconds, io_error, table_writer};
use crate::simulation::events::{
    EventTrait, PersonArrivalEvent, PersonDepartureEvent, PersonStuckEvent,
};
use crate::simulation::time::SimTime;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

#[derive(Clone, Copy, Default)]
struct Profile {
    departures: u64,
    arrivals: u64,
    stuck: u64,
    active_at_start: u64,
    peak_active: u64,
    person_seconds: f64,
    initialized: bool,
}

pub(super) struct AgentProfileCollector {
    interval: u32,
    active: BTreeSet<String>,
    profiles: BTreeMap<u64, Profile>,
    last_nanos: u64,
}

impl AgentProfileCollector {
    pub(super) fn new(interval: u32) -> Self {
        Self {
            interval,
            active: BTreeSet::new(),
            profiles: BTreeMap::new(),
            last_nanos: 0,
        }
    }

    pub(super) fn process_timestamp(&mut self, events: &[Box<dyn EventTrait>], time: SimTime) {
        let now = time.as_nanos();
        self.accrue(self.last_nanos, now);
        let hour = hour_start_seconds(now, self.interval);
        let profile = self.profiles.entry(hour).or_default();
        if !profile.initialized {
            profile.active_at_start = self.active.len() as u64;
            profile.peak_active = profile.active_at_start;
            profile.initialized = true;
        }
        let mut arrivals = Vec::new();
        let mut stuck = Vec::new();
        let mut departures = Vec::new();
        for event in events {
            let any = event.as_any();
            if let Some(event) = any.downcast_ref::<PersonArrivalEvent>() {
                arrivals.push(event.person.external().to_owned());
            } else if let Some(event) = any.downcast_ref::<PersonDepartureEvent>() {
                departures.push(event.person.external().to_owned());
            } else if let Some(event) = any.downcast_ref::<PersonStuckEvent>() {
                stuck.push(event.person.external().to_owned());
            }
        }
        profile.arrivals += arrivals.len() as u64;
        profile.stuck += stuck.len() as u64;
        profile.departures += departures.len() as u64;
        for person in arrivals {
            self.active.remove(&person);
        }
        for person in stuck {
            self.active.remove(&person);
        }
        for person in departures {
            self.active.insert(person);
        }
        profile.peak_active = profile.peak_active.max(self.active.len() as u64);
        self.last_nanos = now;
    }

    fn accrue(&mut self, start: u64, end: u64) {
        if end <= start {
            return;
        }
        let mut cursor = start;
        while cursor < end {
            let hour = hour_start_seconds(cursor, self.interval);
            let end_of_hour = (hour + u64::from(self.interval)) * 1_000_000_000;
            let segment_end = end.min(end_of_hour);
            let profile = self.profiles.entry(hour).or_default();
            if !profile.initialized {
                profile.active_at_start = self.active.len() as u64;
                profile.peak_active = profile.active_at_start;
                profile.initialized = true;
            }
            profile.person_seconds +=
                self.active.len() as f64 * (segment_end - cursor) as f64 / 1_000_000_000.0;
            cursor = segment_end;
        }
    }

    pub(super) fn write_table(
        &self,
        path: &Path,
        interval: u32,
        simulation_end_time: u32,
    ) -> Result<(), AnalysisError> {
        let mut profiles = self.profiles.clone();
        let mut cursor = self.last_nanos;
        let end = u64::from(simulation_end_time) * 1_000_000_000;
        while cursor < end {
            let hour = hour_start_seconds(cursor, interval);
            let segment_end = end.min((hour + u64::from(interval)) * 1_000_000_000);
            let profile = profiles.entry(hour).or_default();
            if !profile.initialized {
                profile.active_at_start = self.active.len() as u64;
                profile.peak_active = profile.active_at_start;
                profile.initialized = true;
            }
            profile.person_seconds +=
                self.active.len() as f64 * (segment_end - cursor) as f64 / 1_000_000_000.0;
            cursor = segment_end;
        }
        let mut writer = table_writer(path, "en_route_agents.csv")?;
        writeln!(writer, "hour_start_seconds,departures,arrivals,stuck,agents_at_interval_start,peak_agents,en_route_person_seconds").map_err(io_error)?;
        let mut hours: BTreeSet<_> = profiles.keys().copied().collect();
        hours.extend((0..u64::from(simulation_end_time)).step_by(interval as usize));
        for hour in hours {
            let profile = profiles.entry(hour).or_default();
            writeln!(
                writer,
                "{hour},{},{},{},{},{},{:.6}",
                profile.departures,
                profile.arrivals,
                profile.stuck,
                profile.active_at_start,
                profile.peak_active,
                profile.person_seconds
            )
            .map_err(io_error)?;
        }
        Ok(())
    }
}
