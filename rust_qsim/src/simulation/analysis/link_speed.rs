//! Final-iteration link speed reconstruction from replayed event files.
//!
//! A speed observation needs a *full-link* traversal: the vehicle has to enter a link at its start
//! and leave it at its end. QSim records the first link of a network leg with
//! `vehicle enters traffic` and the last one with `vehicle leaves traffic` instead of `entered link`
//! and `left link`, and both of those events carry a relative position along the link. A vehicle
//! that is inserted at the end of its start link therefore covers no distance, and a vehicle that
//! arrives in the middle of its end link covers only part of it. Such traversals cannot produce a
//! full-link speed, so they are reported as partial records instead of being folded into the
//! statistics. This is a deliberate deviation from the MATSim link-speed analysis, which averages
//! over every recorded visit, and it matches the travel-time collector of the simulation itself,
//! which only pairs `entered link` with `left link` and therefore never observes the first and
//! the last link of a leg.
//!
//! Observations are assigned to the hour in which the vehicle entered its link, which keeps a
//! traversal that crosses an hour boundary completely inside its entry hour.

use crate::simulation::analysis::{AnalysisError, LinkVisit};
use crate::simulation::events::EventTrait;
use crate::simulation::scenario::network::Link;
use crate::simulation::time::SimTime;
use nohash_hasher::IntMap;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use super::{csv, hour_start_seconds, io_error, link_visit, table_writer};

const NANOS_PER_SECOND: f64 = 1_000_000_000.0;
/// Width of one fixed link-speed histogram bin in m/s.
const SPEED_BIN_WIDTH_MPS: f64 = 5.0;
/// Number of fixed histogram bins; the additional overflow bin collects every faster link.
const SPEED_BIN_COUNT: usize = 10;

/// Welford accumulators, which keep the mean and the population variance stable for long sums.
#[derive(Clone, Copy, Default)]
struct Moments {
    count: u64,
    mean: f64,
    squared_deviations: f64,
}

impl Moments {
    fn push(&mut self, value: f64) {
        self.count += 1;
        let delta = value - self.mean;
        self.mean += delta / self.count as f64;
        self.squared_deviations += delta * (value - self.mean);
    }

    /// Population (not sample) standard deviation; zero for a single or missing observation.
    fn population_std(&self) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        (self.squared_deviations / self.count as f64)
            .max(0.0)
            .sqrt()
    }
}

/// All full-link traversals of one link within one analysis interval.
#[derive(Clone, Copy, Default)]
struct SpeedGroup {
    observations: u64,
    /// Summed traversal durations. Integer nanoseconds keep this total exact and independent of the
    /// order in which the partitions are replayed, while the speed moments below follow the replay
    /// order of the single traversals, which can move the exported mean and deviation in the last
    /// digits.
    total_duration_nanos: u64,
    vehicle_speeds: Moments,
}

impl SpeedGroup {
    fn observe(&mut self, speed: f64, duration_nanos: u64) {
        self.observations += 1;
        self.total_duration_nanos = self.total_duration_nanos.saturating_add(duration_nanos);
        self.vehicle_speeds.push(speed);
    }

    /// The representative speed is the total travelled distance divided by the total travel time.
    /// It stays unavailable for a group without observations and whenever the aggregate itself is
    /// not finite.
    fn representative_speed(&self, length: f64) -> Option<f64> {
        let speed = (self.observations > 0 && self.total_duration_nanos > 0).then(|| {
            self.observations as f64 * length
                / (self.total_duration_nanos as f64 / NANOS_PER_SECOND)
        })?;
        speed.is_finite().then_some(speed)
    }

    fn mean_speed(&self) -> Option<f64> {
        (self.observations > 0).then_some(self.vehicle_speeds.mean)
    }
}

/// The full-link traversals of one link within one analysis interval.
#[derive(Ord, PartialOrd, Eq, PartialEq)]
struct SpeedKey {
    hour_start_seconds: u64,
    link_index: usize,
}

impl SpeedKey {
    fn new(hour_start_seconds: u64, link_index: usize) -> Self {
        Self {
            hour_start_seconds,
            link_index,
        }
    }
}

/// One link traversal that has been entered but not yet left.
#[derive(Clone, Copy)]
struct OpenTraversal {
    link_index: usize,
    entry_position: f64,
    entry_nanos: u64,
}

/// Records that cannot contribute a full-link speed, reported next to the speeds themselves.
#[derive(Clone, Copy, Default)]
pub(super) struct SpeedDiagnostics {
    full_link_traversals: u64,
    partial_link_traversals: u64,
    unfinished_traversals: u64,
    unmatched_leave_events: u64,
    non_positive_duration_traversals: u64,
    invalid_link_length_traversals: u64,
    non_finite_speed_traversals: u64,
}

impl SpeedDiagnostics {
    /// The exported counters, in the order in which they are reported.
    pub(super) const METRICS: [&'static str; 7] = [
        "full_link_traversals",
        "partial_link_traversals",
        "unfinished_traversals",
        "unmatched_leave_events",
        "non_positive_duration_traversals",
        "invalid_link_length_traversals",
        "non_finite_speed_traversals",
    ];

    /// The exported counters with their values.
    fn rows(&self) -> Vec<(&'static str, u64)> {
        let counts = [
            self.full_link_traversals,
            self.partial_link_traversals,
            self.unfinished_traversals,
            self.unmatched_leave_events,
            self.non_positive_duration_traversals,
            self.invalid_link_length_traversals,
            self.non_finite_speed_traversals,
        ];
        Self::METRICS.iter().copied().zip(counts).collect()
    }
}

/// Reconstructs link traversals while the final-iteration event partitions are replayed.
pub(super) struct LinkSpeedCollector<'a> {
    interval_seconds: u32,
    /// The reported links in the order of the report; an index into this slice identifies a link.
    links: &'a [&'a Link],
    link_index: BTreeMap<&'a str, usize>,
    /// Traversals are kept per vehicle, oldest first, and matched by link. A leave therefore finds
    /// its enter even when the enter of the next link was already seen at the same timestamp, as it
    /// happens when the partitions of a hand-over are replayed in the opposite rank order, and even
    /// when a vehicle visits the same link several times.
    open_traversals: IntMap<u64, Vec<OpenTraversal>>,
    groups: BTreeMap<SpeedKey, SpeedGroup>,
    diagnostics: SpeedDiagnostics,
}

impl<'a> LinkSpeedCollector<'a> {
    pub(super) fn new(interval_seconds: u32, links: &'a [&'a Link]) -> Self {
        Self {
            interval_seconds,
            links,
            link_index: links
                .iter()
                .enumerate()
                .map(|(index, link)| (link.id.external(), index))
                .collect(),
            open_traversals: IntMap::default(),
            groups: BTreeMap::new(),
            diagnostics: SpeedDiagnostics::default(),
        }
    }

    /// Feeds one replayed event. Events for links outside the reported network are ignored, the
    /// same way link volumes ignore them.
    pub(super) fn observe(&mut self, event: &dyn EventTrait, time: SimTime) {
        match link_visit(event) {
            Some(LinkVisit::Enter {
                vehicle,
                link,
                entry_position,
            }) => self.enter(vehicle.internal(), link.external(), entry_position, time),
            Some(LinkVisit::Leave {
                vehicle,
                link,
                exit_position,
            }) => self.leave(vehicle.internal(), link.external(), exit_position, time),
            None => {}
        }
    }

    /// Closes the bookkeeping once the replay is done; every traversal that is still open means
    /// the vehicle never left the link within the recorded events.
    pub(super) fn finish(&mut self) {
        self.diagnostics.unfinished_traversals = self
            .open_traversals
            .values()
            .map(|traversals| traversals.len() as u64)
            .sum();
        self.open_traversals.clear();
    }

    pub(super) fn write_tables(&self, path: &Path, hours: &[u64]) -> Result<(), AnalysisError> {
        // The across-link statistics and the histogram of an interval describe the same groups, so
        // both are built from one pass over the reported links.
        let summaries: Vec<_> = hours.iter().map(|hour| self.hour_summary(*hour)).collect();
        self.write_hourly_speeds(path, hours)?;
        self.write_hourly_summary(path, &summaries)?;
        self.write_speed_histogram(path, &summaries)?;
        self.write_diagnostics(path)?;
        Ok(())
    }

    fn enter(&mut self, vehicle: u64, link: &str, position: f64, time: SimTime) {
        let Some(link_index) = self.link_index(link) else {
            return;
        };
        self.open_traversals
            .entry(vehicle)
            .or_default()
            .push(OpenTraversal {
                link_index,
                entry_position: position,
                entry_nanos: time.as_nanos(),
            });
    }

    fn leave(&mut self, vehicle: u64, link: &str, position: f64, time: SimTime) {
        let Some(link_index) = self.link_index(link) else {
            return;
        };
        let Some(traversal) = self.take_open_traversal(vehicle, link_index) else {
            self.diagnostics.unmatched_leave_events += 1;
            return;
        };
        self.record(traversal, position, time);
    }

    /// Removes the oldest open traversal of `link_index`, so that a leave pairs up with the
    /// matching enter even when several visits of the same link are open at once.
    fn take_open_traversal(&mut self, vehicle: u64, link_index: usize) -> Option<OpenTraversal> {
        let traversals = self.open_traversals.get_mut(&vehicle)?;
        let offset = traversals
            .iter()
            .position(|traversal| traversal.link_index == link_index)?;
        let traversal = traversals.remove(offset);
        if traversals.is_empty() {
            self.open_traversals.remove(&vehicle);
        }
        Some(traversal)
    }

    fn link_index(&self, link: &str) -> Option<usize> {
        self.link_index.get(link).copied()
    }

    fn record(&mut self, traversal: OpenTraversal, exit_position: f64, time: SimTime) {
        // The checks below classify a completed traversal in a fixed order, so that a record is
        // only counted once: unusable input first, then a traversal that did not cover the whole
        // link, then a full-link traversal whose duration cannot describe a speed.
        let length = self.links[traversal.link_index].length;
        if !length.is_finite() || length < 0.0 {
            self.diagnostics.invalid_link_length_traversals += 1;
            return;
        }
        if !traversal.entry_position.is_finite() || !exit_position.is_finite() {
            self.diagnostics.non_finite_speed_traversals += 1;
            return;
        }
        // The event writer prints the relative position with the shortest representation that
        // round-trips, so the positions of a full-link traversal are exactly the link ends. An
        // exact comparison is the contract here: a tolerance would silently accept traversals
        // that did not cover the whole link.
        if traversal.entry_position != 0.0 || exit_position != 1.0 {
            self.diagnostics.partial_link_traversals += 1;
            return;
        }
        let duration_nanos = time.as_nanos().saturating_sub(traversal.entry_nanos);
        if duration_nanos == 0 {
            self.diagnostics.non_positive_duration_traversals += 1;
            return;
        }
        let speed = length / (duration_nanos as f64 / NANOS_PER_SECOND);
        if !speed.is_finite() {
            self.diagnostics.non_finite_speed_traversals += 1;
            return;
        }
        self.diagnostics.full_link_traversals += 1;
        let key = SpeedKey::new(
            hour_start_seconds(traversal.entry_nanos, self.interval_seconds),
            traversal.link_index,
        );
        self.groups
            .entry(key)
            .or_default()
            .observe(speed, duration_nanos);
    }

    fn write_hourly_speeds(&self, path: &Path, hours: &[u64]) -> Result<(), AnalysisError> {
        let mut writer = table_writer(path, "link_speed_hourly.csv")?;
        writeln!(
            writer,
            "link_id,hour_start_seconds,observations,total_distance_meters,total_duration_seconds,representative_speed_mps,vehicle_speed_mean_mps,vehicle_speed_population_std_mps"
        )
        .map_err(io_error)?;
        for hour in hours {
            for (link_index, link) in self.links.iter().enumerate() {
                let group = self.groups.get(&SpeedKey::new(*hour, link_index)).copied();
                let length = link.length;
                // Counts and totals stay zero for a group without observations, while the speeds
                // themselves remain unavailable.
                let observations = group.map_or(0, |group| group.observations);
                let distance = group.map_or(0.0, |group| group.observations as f64 * length);
                let duration = group.map_or(0.0, |group| {
                    group.total_duration_nanos as f64 / NANOS_PER_SECOND
                });
                let representative = group.and_then(|group| group.representative_speed(length));
                let mean = group.and_then(|group| group.mean_speed());
                let deviation = group.map(|group| group.vehicle_speeds.population_std());
                writeln!(
                    writer,
                    "{},{hour},{observations},{distance:.6},{duration:.6},{representative},{mean},{deviation}",
                    csv(link.id.external()),
                    representative = optional_number(representative),
                    mean = optional_number(mean),
                    deviation = optional_number(deviation),
                )
                .map_err(io_error)?;
            }
        }
        Ok(())
    }

    fn write_hourly_summary(
        &self,
        path: &Path,
        summaries: &[HourSummary],
    ) -> Result<(), AnalysisError> {
        let mut writer = table_writer(path, "link_speed_summary.csv")?;
        writeln!(
            writer,
            "hour_start_seconds,links_with_speed,observations,mean_link_speed_mps,population_std_link_speed_mps"
        )
        .map_err(io_error)?;
        for summary in summaries {
            let hour = summary.hour_start_seconds;
            writeln!(
                writer,
                "{hour},{links},{observations},{mean},{deviation}",
                links = summary.links_with_speed,
                observations = summary.observations,
                mean = optional_number(summary.mean_speed()),
                deviation = optional_number(summary.population_std()),
            )
            .map_err(io_error)?;
        }
        Ok(())
    }

    fn write_speed_histogram(
        &self,
        path: &Path,
        summaries: &[HourSummary],
    ) -> Result<(), AnalysisError> {
        let mut writer = table_writer(path, "link_speed_histogram.csv")?;
        writeln!(
            writer,
            "hour_start_seconds,bin_index,bin_lower_mps,bin_upper_mps,link_count,observation_count"
        )
        .map_err(io_error)?;
        for summary in summaries {
            let hour = summary.hour_start_seconds;
            for bin in 0..=SPEED_BIN_COUNT {
                let lower = bin as f64 * SPEED_BIN_WIDTH_MPS;
                let (links, observations) = summary.bins[bin];
                if bin < SPEED_BIN_COUNT {
                    writeln!(
                        writer,
                        "{hour},{bin},{lower:.6},{:.6},{links},{observations}",
                        lower + SPEED_BIN_WIDTH_MPS
                    )
                    .map_err(io_error)?;
                } else {
                    // The overflow bin has no upper edge.
                    writeln!(writer, "{hour},{bin},{lower:.6},,{links},{observations}")
                        .map_err(io_error)?;
                }
            }
        }
        Ok(())
    }

    fn write_diagnostics(&self, path: &Path) -> Result<(), AnalysisError> {
        let mut writer = table_writer(path, "link_speed_diagnostics.csv")?;
        writeln!(writer, "metric,count").map_err(io_error)?;
        for (metric, count) in self.diagnostics.rows() {
            writeln!(writer, "{metric},{count}").map_err(io_error)?;
        }
        Ok(())
    }

    fn hour_summary(&self, hour: u64) -> HourSummary {
        let mut summary = HourSummary::new(hour);
        for (link_index, link) in self.links.iter().enumerate() {
            let Some(group) = self.groups.get(&SpeedKey::new(hour, link_index)) else {
                continue;
            };
            let Some(speed) = group.representative_speed(link.length) else {
                continue;
            };
            summary.links_with_speed += 1;
            summary.observations += group.observations;
            summary.link_speeds.push(speed);
            let (links, observations) = &mut summary.bins[speed_bin(speed)];
            *links += 1;
            *observations += group.observations;
        }
        summary
    }
}

/// Link counts and observation counts of one analysis interval, plus the across-link statistics.
struct HourSummary {
    hour_start_seconds: u64,
    links_with_speed: u64,
    observations: u64,
    link_speeds: Moments,
    bins: Vec<(u64, u64)>,
}

impl HourSummary {
    fn new(hour_start_seconds: u64) -> Self {
        Self {
            hour_start_seconds,
            links_with_speed: 0,
            observations: 0,
            link_speeds: Moments::default(),
            bins: vec![(0, 0); SPEED_BIN_COUNT + 1],
        }
    }

    fn mean_speed(&self) -> Option<f64> {
        (self.links_with_speed > 0).then_some(self.link_speeds.mean)
    }

    fn population_std(&self) -> Option<f64> {
        (self.links_with_speed > 0).then(|| self.link_speeds.population_std())
    }
}

/// Maps a speed onto its fixed bin; the last bin is the overflow bin for every faster speed.
///
/// Only finite, non-negative speeds reach the histogram, and the saturating float-to-integer cast
/// keeps the bin index in range for any other value.
fn speed_bin(speed: f64) -> usize {
    ((speed / SPEED_BIN_WIDTH_MPS).floor() as usize).min(SPEED_BIN_COUNT)
}

/// Formats an optional metric value; an unavailable value stays empty instead of being zero.
fn optional_number(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:.6}"),
        None => String::new(),
    }
}
