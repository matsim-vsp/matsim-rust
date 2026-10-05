//! Final-iteration distance, time and free-flow delay from observed link visits.

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

#[derive(Clone, Copy)]
struct OpenVisit {
    link_index: usize,
    entry_position: f64,
    entry_nanos: u64,
}

#[derive(Clone, Copy, Default)]
struct Totals {
    traversals: u64,
    partial_traversals: u64,
    distance_meters: f64,
    vehicle_seconds: f64,
    free_flow_delay_seconds: f64,
    positive_excess_delay_seconds: f64,
    reference_time_seconds: f64,
    reference_observed_seconds: f64,
    delay_observations: u64,
}

#[derive(Clone, Copy, Default)]
struct Diagnostics {
    unfinished_traversals: u64,
    unmatched_leave_events: u64,
    invalid_positions: u64,
    invalid_link_lengths: u64,
    non_positive_durations: u64,
    invalid_reference_speeds: u64,
}

#[derive(Ord, PartialOrd, Eq, PartialEq)]
struct Key {
    hour: u64,
    link_index: usize,
}

pub(super) struct NetworkDistanceCollector<'a> {
    interval: u32,
    links: &'a [&'a Link],
    link_indices: BTreeMap<&'a str, usize>,
    open: IntMap<u64, Vec<OpenVisit>>,
    totals: BTreeMap<Key, Totals>,
    diagnostics: Diagnostics,
}

impl<'a> NetworkDistanceCollector<'a> {
    pub(super) fn new(interval: u32, links: &'a [&'a Link]) -> Self {
        Self {
            interval,
            links,
            link_indices: links
                .iter()
                .enumerate()
                .map(|(i, link)| (link.id.external(), i))
                .collect(),
            open: IntMap::default(),
            totals: BTreeMap::new(),
            diagnostics: Diagnostics::default(),
        }
    }

    pub(super) fn observe(&mut self, event: &dyn EventTrait, time: SimTime) {
        match link_visit(event) {
            Some(LinkVisit::Enter {
                vehicle,
                link,
                entry_position,
            }) => {
                if let Some(&link_index) = self.link_indices.get(link.external()) {
                    self.open
                        .entry(vehicle.internal())
                        .or_default()
                        .push(OpenVisit {
                            link_index,
                            entry_position,
                            entry_nanos: time.as_nanos(),
                        });
                }
            }
            Some(LinkVisit::Leave {
                vehicle,
                link,
                exit_position,
            }) => {
                let Some(&link_index) = self.link_indices.get(link.external()) else {
                    return;
                };
                let Some(visits) = self.open.get_mut(&vehicle.internal()) else {
                    self.diagnostics.unmatched_leave_events += 1;
                    return;
                };
                let Some(offset) = visits
                    .iter()
                    .position(|visit| visit.link_index == link_index)
                else {
                    self.diagnostics.unmatched_leave_events += 1;
                    return;
                };
                let visit = visits.remove(offset);
                if visits.is_empty() {
                    self.open.remove(&vehicle.internal());
                }
                self.record(visit, exit_position, time.as_nanos());
            }
            None => {}
        }
    }

    fn record(&mut self, visit: OpenVisit, exit_position: f64, exit_nanos: u64) {
        let link = self.links[visit.link_index];
        if !link.length.is_finite() || link.length < 0.0 {
            self.diagnostics.invalid_link_lengths += 1;
            return;
        }
        if !visit.entry_position.is_finite()
            || !exit_position.is_finite()
            || !(0.0..=1.0).contains(&visit.entry_position)
            || !(0.0..=1.0).contains(&exit_position)
            || exit_position < visit.entry_position
        {
            self.diagnostics.invalid_positions += 1;
            return;
        }
        let elapsed_nanos = exit_nanos.saturating_sub(visit.entry_nanos);
        if elapsed_nanos == 0 {
            self.diagnostics.non_positive_durations += 1;
            return;
        }
        let distance = link.length * (exit_position - visit.entry_position);
        let partial = visit.entry_position != 0.0 || exit_position != 1.0;
        let delay = if link.freespeed.is_finite() && link.freespeed > 0.0 {
            Some(elapsed_nanos as f64 / NANOS_PER_SECOND - distance / link.freespeed)
        } else {
            self.diagnostics.invalid_reference_speeds += 1;
            None
        };

        // Keep a whole traversal in its entry interval. Splitting it would assume how the vehicle
        // moved within the link, which these boundary events do not record.
        let hour = hour_start_seconds(visit.entry_nanos, self.interval);
        let totals = self
            .totals
            .entry(Key {
                hour,
                link_index: visit.link_index,
            })
            .or_default();
        let elapsed_seconds = elapsed_nanos as f64 / NANOS_PER_SECOND;
        totals.traversals += 1;
        totals.partial_traversals += u64::from(partial);
        totals.distance_meters += distance;
        totals.vehicle_seconds += elapsed_seconds;
        if let Some(delay) = delay {
            totals.free_flow_delay_seconds += delay;
            totals.positive_excess_delay_seconds += delay.max(0.0);
            totals.reference_time_seconds += elapsed_seconds - delay;
            totals.reference_observed_seconds += elapsed_seconds;
            totals.delay_observations += 1;
        }
    }

    pub(super) fn finish(&mut self) {
        self.diagnostics.unfinished_traversals =
            self.open.values().map(|visits| visits.len() as u64).sum();
        self.open.clear();
    }

    pub(super) fn write_tables(
        &self,
        path: &Path,
        hours: &[u64],
        clip_delay: Option<f64>,
    ) -> Result<(), AnalysisError> {
        let mut per_link = table_writer(path, "network_distance_time.csv")?;
        let mut per_link_header = "link_id,hour_start_seconds,vehicle_traversals,partial_traversals,vehicle_distance_meters,vehicle_time_seconds,free_flow_relative_delay_seconds,relative_speed_ratio".to_owned();
        if clip_delay.is_some() {
            per_link_header.push_str(",clipped_excess_delay_seconds");
        }
        per_link_header
            .push_str(",passenger_distance_meters,passenger_time_seconds,passenger_data_status");
        writeln!(per_link, "{per_link_header}").map_err(io_error)?;
        let mut summary = table_writer(path, "network_distance_time_summary.csv")?;
        let mut summary_header = "hour_start_seconds,vehicle_traversals,vehicle_distance_meters,vehicle_time_seconds,free_flow_relative_delay_seconds,relative_speed_ratio".to_owned();
        if clip_delay.is_some() {
            summary_header.push_str(",network_clipped_excess_delay_seconds");
        }
        summary_header
            .push_str(",passenger_distance_meters,passenger_time_seconds,passenger_data_status");
        writeln!(summary, "{summary_header}").map_err(io_error)?;
        for &hour in hours {
            let mut network = Totals::default();
            let mut network_clipped = 0.0;
            let mut network_has_delay = false;
            for (link_index, link) in self.links.iter().enumerate() {
                let value = self
                    .totals
                    .get(&Key { hour, link_index })
                    .copied()
                    .unwrap_or_default();
                network.traversals += value.traversals;
                network.distance_meters += value.distance_meters;
                network.vehicle_seconds += value.vehicle_seconds;
                network.free_flow_delay_seconds += value.free_flow_delay_seconds;
                network.reference_time_seconds += value.reference_time_seconds;
                network.reference_observed_seconds += value.reference_observed_seconds;
                network.delay_observations += value.delay_observations;
                if value.delay_observations > 0 {
                    network_has_delay = true;
                    if let Some(clip) = clip_delay {
                        network_clipped += value.positive_excess_delay_seconds.min(clip);
                    }
                }
                let clipped = (value.delay_observations > 0)
                    .then(|| clip_delay.map(|clip| value.positive_excess_delay_seconds.min(clip)))
                    .flatten();
                let relative_speed = ratio(
                    value.reference_time_seconds,
                    value.reference_observed_seconds,
                    value.delay_observations,
                );
                let mut row = format!(
                    "{},{hour},{},{},{:.6},{:.6},{},{}",
                    csv(link.id.external()),
                    value.traversals,
                    value.partial_traversals,
                    value.distance_meters,
                    value.vehicle_seconds,
                    optional_sum(value.delay_observations, value.free_flow_delay_seconds),
                    relative_speed
                );
                if clip_delay.is_some() {
                    row.push(',');
                    row.push_str(&optional_option(clipped));
                }
                row.push_str(",,,unavailable");
                writeln!(per_link, "{row}").map_err(io_error)?;
            }
            let clipped = (network_has_delay && clip_delay.is_some()).then_some(network_clipped);
            let relative_speed = ratio(
                network.reference_time_seconds,
                network.reference_observed_seconds,
                network.delay_observations,
            );
            let mut row = format!(
                "{hour},{},{:.6},{:.6},{},{}",
                network.traversals,
                network.distance_meters,
                network.vehicle_seconds,
                optional_sum(network.delay_observations, network.free_flow_delay_seconds),
                relative_speed
            );
            if clip_delay.is_some() {
                row.push(',');
                row.push_str(&optional_option(clipped));
            }
            row.push_str(",,,unavailable");
            writeln!(summary, "{row}").map_err(io_error)?;
        }
        let mut diagnostics = table_writer(path, "network_distance_time_diagnostics.csv")?;
        writeln!(diagnostics, "metric,count\nunfinished_traversals,{}\nunmatched_leave_events,{}\ninvalid_positions,{}\ninvalid_link_lengths,{}\nnon_positive_durations,{}\ninvalid_reference_speeds,{}", self.diagnostics.unfinished_traversals, self.diagnostics.unmatched_leave_events, self.diagnostics.invalid_positions, self.diagnostics.invalid_link_lengths, self.diagnostics.non_positive_durations, self.diagnostics.invalid_reference_speeds).map_err(io_error)?;
        Ok(())
    }
}

fn optional_sum(count: u64, value: f64) -> String {
    if count == 0 {
        String::new()
    } else {
        format!("{value:.6}")
    }
}
fn optional_option(value: Option<f64>) -> String {
    value.map_or_else(String::new, |value| format!("{value:.6}"))
}
fn ratio(reference_time: f64, observed_time: f64, count: u64) -> String {
    let value = reference_time / observed_time;
    if count == 0 || !value.is_finite() {
        String::new()
    } else {
        format!("{value:.6}")
    }
}
