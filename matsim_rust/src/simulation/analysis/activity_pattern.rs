//! Observed activity patterns: daily activity sequences, mode chains, and the
//! reconciliation of a person's day into activity and travel time.
//!
//! The recording window opens at the run's start time. An activity that is already in
//! progress when the window opens was going before it, so its total duration is only a lower
//! bound: it is **left-censored**. An activity that has no observed end by the time the run
//! shuts down is **right-censored** for the same reason. Both are reported as explicit flags
//! rather than being silently folded into a duration average, and a censored interval leaves
//! `duration_seconds` blank.
//!
//! Censoring is distinct from what is *observable inside* the window. A censored activity
//! still contributes the seconds it was observed to last, so `in_window_seconds` is reported
//! alongside the bounded `duration_seconds`. That is what lets activity time and travel time
//! be reconciled against the observed span of the day.
//!
//! Stage activities — transit access, egress and transfer waits, whose type contains
//! `interaction` — are recorded but flagged. A journey spans consecutive *substantive*
//! activities, so the plan counts those waits differently from the observed stream; the flag
//! is what keeps the two counts comparable. See [`is_stage`].

use super::{
    AnalysisError, ClassifiedLink, JourneyRow, LinkClassifications, ObservedLeg, csv, io_error,
    number_opt, table_writer,
};
use crate::simulation::config::ZoneSystem;
use crate::simulation::events::{
    ActivityEndEvent, ActivityStartEvent, EventTrait, PersonStuckEvent,
};
use crate::simulation::scenario::population::is_interaction_type;
use crate::simulation::time::SimTime;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

/// Grouping of a per-person day, reported as one row in `activity_patterns.csv`.
#[derive(Debug, Clone)]
pub(super) struct PatternRow {
    pub person_id: String,
    /// Supplied person geography; `unmapped` when the zone system does not cover the person.
    pub person_zone: String,
    /// Substantive activities the recorded selected plan contains, the denominator that
    /// decides whether the observed day covers the plan.
    pub planned_activities: usize,
    /// Activities actually observed in the window.
    pub observed_activities: usize,
    /// `home|work|home`: the activity types in the order they were observed.
    pub activity_chain: String,
    /// Observed leg modes in departure order, e.g. `walk|pt|walk`.
    pub mode_chain: String,
    /// Main modes of the observed journeys in order, folding access walks and transit
    /// transfers the way `main_mode` does elsewhere in the report.
    pub journey_mode_chain: String,
    pub planned_journeys: usize,
    pub observed_journeys: usize,
    pub completed_journeys: usize,
    pub legs: usize,
    /// Earliest moment the person was observed doing anything, from an activity start or a
    /// leg departure.
    pub first_start_seconds: Option<f64>,
    /// Latest moment the person was observed doing anything, from an activity end, a leg
    /// arrival or a stuck event.
    pub last_end_seconds: Option<f64>,
    /// `last_end_seconds - first_start_seconds`, the wall-clock the observation covers.
    pub observed_span_seconds: Option<f64>,
    /// Seconds spent in activities, counted inside the window.
    pub activity_seconds: f64,
    /// Seconds spent travelling, summed over legs that reached an arrival.
    pub travel_seconds: f64,
    /// `observed_span_seconds - activity_seconds - travel_seconds`. Zero for a day that
    /// tiles the window; a positive value is time the recorded events do not account for.
    pub timeline_gap_seconds: Option<f64>,
    pub left_censored_activities: usize,
    pub right_censored_activities: usize,
    /// `complete`, `truncated`, `stuck` or `not_observed`; see [`pattern_status`].
    pub status: &'static str,
}

/// One observed activity interval.
#[derive(Debug, Clone)]
pub(super) struct ActivityRow {
    pub person_id: String,
    pub activity_sequence: usize,
    pub act_type: String,
    /// A transit access, egress or transfer wait rather than a substantive activity. Recorded
    /// so the day reconciles against every observed interval, but excluded from the count
    /// that [`pattern_status`] compares against the plan.
    pub stage: bool,
    pub link_id: String,
    pub zone: String,
    pub urban_area: String,
    /// Observed start, absent only when the activity ended before the window opened and its
    /// start was never recorded.
    pub start_seconds: Option<f64>,
    /// Observed end, absent when the activity never ended inside the window.
    pub end_seconds: Option<f64>,
    /// `end - start`, absent whenever either side is censored: a censored interval is only a
    /// lower bound, so it must not reach a duration mean.
    pub duration_seconds: Option<f64>,
    /// The part of the interval that lies inside the window, reported even when the total
    /// duration is censored.
    pub in_window_seconds: Option<f64>,
    pub start_censored: bool,
    pub end_censored: bool,
}

/// Collected activity patterns, keyed by person for the exporters.
pub(super) struct ActivityPatterns {
    pub rows: Vec<PatternRow>,
    pub activities: Vec<ActivityRow>,
}

/// MATSim's stage-activity rule, applied to a recorded activity type.
///
/// A journey spans consecutive substantive activities, so the plan counts an access walk or a
/// transit transfer as part of a journey rather than as a destination. The observed stream
/// records every `actstart` and `actend`, including those waits, so the same rule has to be
/// applied here for the two counts to be comparable.
pub(super) fn is_stage(act_type: &str) -> bool {
    is_interaction_type(act_type)
}

/// Classify how completely a person's observed day covers their plan.
///
/// - `not_observed`: the person emitted no activity event at all in the window, so there is
///   no observed day to classify. A non-traveler whose plan is empty is still `not_observed`;
///   the plan and the observed events are separate sources and neither is assumed.
/// - `stuck`: the person emitted a stuck event, so the rest of the day never happened.
/// - `truncated`: fewer *substantive* activities were observed than the plan contains. Both
///   counts exclude stage activities, so a transit transfer that happened cannot stand in for
///   a destination that did not.
/// - `complete`: every planned activity was observed. A last activity without an observed
///   end is still `complete`; that is ordinary end-of-day right-censoring, reported by the
///   censoring flags rather than by downgrading the pattern.
///
/// `observed_activities` is the substantive count. The plan's count excludes stage activities,
/// and a stage activity that was observed is not evidence that a planned destination was
/// reached, so the two are compared like for like.
pub(super) fn pattern_status(
    planned_activities: usize,
    observed_activities: usize,
    stuck: bool,
) -> &'static str {
    if observed_activities == 0 {
        "not_observed"
    } else if stuck {
        "stuck"
    } else if observed_activities < planned_activities {
        "truncated"
    } else {
        "complete"
    }
}

/// An activity that has started but not yet been seen to end.
struct OpenActivity {
    person_id: String,
    activity_sequence: usize,
    act_type: String,
    link_id: String,
    /// `None` when no start was observed inside the window, which is left-censoring.
    start_seconds: Option<f64>,
    start_censored: bool,
}

/// Reconstructs observed activity intervals from the event stream.
///
/// Activity ends are processed before activity starts within one timestamp batch, so a
/// person who ends one activity and starts the next at the same instant pairs correctly
/// regardless of which partition delivered either event first.
#[derive(Default)]
pub(super) struct ActivityCollector {
    /// Number of activities already recorded per person, which makes the sequence number
    /// independent of the order in which persons appear.
    sequences: BTreeMap<String, usize>,
    open: BTreeMap<String, OpenActivity>,
    pub activities: Vec<ActivityRow>,
    /// Time each stuck person was recorded, which is the last thing observed about their day.
    stuck_at_seconds: BTreeMap<String, f64>,
    /// The run's start time, which bounds how far a left-censored activity extends into the
    /// window.
    window_start_seconds: f64,
}

impl ActivityCollector {
    pub(super) fn new(window_start_seconds: f64) -> Self {
        Self {
            window_start_seconds,
            ..Self::default()
        }
    }

    pub(super) fn process_timestamp(&mut self, events: &[Box<dyn EventTrait>], time: SimTime) {
        let seconds = time.as_nanos() as f64 / 1_000_000_000.0;
        for event in events {
            if let Some(event) = event.as_any().downcast_ref::<ActivityEndEvent>() {
                let person = event.person.external().to_owned();
                if let Some(open) = self.open.remove(&person) {
                    self.record(open, Some(seconds));
                }
            }
        }
        for event in events {
            if let Some(event) = event.as_any().downcast_ref::<ActivityStartEvent>() {
                let person = event.person.external().to_owned();
                // An activity that is still open when the next one starts never had its end
                // observed, so it is right-censored rather than closed at the new start.
                if let Some(open) = self.open.remove(&person) {
                    self.record(open, None);
                }
                let sequence = self.sequences.entry(person.clone()).or_default();
                let activity_sequence = *sequence;
                *sequence += 1;
                self.open.insert(
                    person,
                    OpenActivity {
                        person_id: event.person.external().to_owned(),
                        activity_sequence,
                        act_type: event.act_type.external().to_owned(),
                        link_id: event.link.external().to_owned(),
                        // The recording opens at the simulation start, so an activity that
                        // starts exactly there was already in progress when the window did.
                        start_seconds: Some(seconds),
                        start_censored: seconds <= self.window_start_seconds,
                    },
                );
            }
        }
        for event in events {
            if let Some(event) = event.as_any().downcast_ref::<PersonStuckEvent>() {
                self.stuck_at_seconds
                    .insert(event.person.external().to_owned(), seconds);
            }
        }
    }

    /// Close any activity still open at the end of the run. Right-censoring is the
    /// simulation ending mid-activity, not a missing event, so the end is left unobserved.
    pub(super) fn finish(&mut self) {
        for (_, open) in std::mem::take(&mut self.open) {
            self.record(open, None);
        }
        self.activities.sort_by(|a, b| {
            (&a.person_id, a.activity_sequence).cmp(&(&b.person_id, b.activity_sequence))
        });
    }

    fn record(&mut self, open: OpenActivity, end_seconds: Option<f64>) {
        // A censored side means the interval is only a lower bound, so the total duration is
        // left unreported rather than feeding a mean a bound it never reached.
        let duration_seconds = match (open.start_seconds, end_seconds) {
            (Some(start), Some(end)) if !open.start_censored => Some(end - start),
            _ => None,
        };
        // The in-window share is known whenever the end was observed, even if the start was
        // censored: the activity was running for that whole stretch of the window.
        let in_window_seconds = end_seconds
            .map(|end| (end - open.start_seconds.unwrap_or(self.window_start_seconds)).max(0.0));
        self.activities.push(ActivityRow {
            person_id: open.person_id,
            activity_sequence: open.activity_sequence,
            stage: is_stage(&open.act_type),
            act_type: open.act_type,
            link_id: open.link_id,
            zone: String::new(),
            urban_area: String::new(),
            start_seconds: open.start_seconds,
            end_seconds,
            duration_seconds,
            in_window_seconds,
            start_censored: open.start_censored,
            end_censored: end_seconds.is_none(),
        });
    }
}

/// Group the observed activities into one row per person, joining the journey and leg records
/// that describe the same day.
pub(super) fn build_patterns(
    collector: &mut ActivityCollector,
    journeys: &[JourneyRow],
    observed_legs: &[ObservedLeg],
    planned: &BTreeMap<String, PlannedDay>,
    classifications: &LinkClassifications,
    zone_system: &ZoneSystem,
) -> ActivityPatterns {
    // The run has finished and never reads its activity rows again, so they are taken rather
    // than copied: each row owns several identifiers, and a population of any size makes that
    // copy the most expensive thing in the report.
    let mut activities = std::mem::take(&mut collector.activities);
    for activity in &mut activities {
        activity.zone = super::zones::zone_of(zone_system, &activity.link_id);
        activity.urban_area = urban_area_of(classifications, &activity.link_id);
    }
    let stuck_at_seconds = std::mem::take(&mut collector.stuck_at_seconds);
    let mut rows = Vec::new();
    for (person_id, bucket) in group_by_person(&activities, journeys, observed_legs, planned) {
        let person_activities: Vec<&ActivityRow> =
            bucket.activities.iter().map(|&i| &activities[i]).collect();
        let person_journeys: Vec<&JourneyRow> =
            bucket.journeys.iter().map(|&i| &journeys[i]).collect();
        let person_legs: Vec<&ObservedLeg> =
            bucket.legs.iter().map(|&i| &observed_legs[i]).collect();
        let planned_activities = planned.get(person_id).map_or(0, |day| day.activities);
        let planned_journeys = planned.get(person_id).map_or(0, |day| day.journeys);
        // Stage activities are recorded so the day reconciles against every observed interval,
        // but the plan does not count them as destinations, so neither does the observed count
        // that the status compares against it.
        let observed_activities = person_activities
            .iter()
            .filter(|activity| !activity.stage)
            .count();
        let activity_seconds = total(
            person_activities
                .iter()
                .filter_map(|activity| activity.in_window_seconds),
        );
        let travel_seconds = total(
            person_legs
                .iter()
                .filter_map(|leg| leg.completion.duration(leg.departure_seconds)),
        );
        // The observed span runs between the first and the last thing the recording caught the
        // person doing. It covers legs as well as activities, because a right-censored final
        // activity contributes no time while the leg that reached it does.
        let first_start_seconds = person_activities
            .iter()
            .filter_map(|activity| activity.start_seconds)
            .chain(person_legs.iter().map(|leg| leg.departure_seconds))
            .min_by(f64::total_cmp);
        // The span's end is the last thing the recording caught the person doing, which
        // includes a stuck event: a person who was recorded mid-leg and aborted was observed at
        // that instant, and the unaccounted travel up to it belongs in the timeline gap rather
        // than being lost from the day entirely.
        let last_end_seconds = person_activities
            .iter()
            .filter_map(|activity| activity.end_seconds)
            .chain(
                person_legs
                    .iter()
                    .filter_map(|leg| leg.completion.arrival_seconds()),
            )
            .chain(stuck_at_seconds.get(person_id).copied())
            .max_by(f64::total_cmp);
        let observed_span_seconds = first_start_seconds
            .zip(last_end_seconds)
            .map(|(first, last)| last - first);
        let timeline_gap_seconds = observed_span_seconds
            .map(|span| span - activity_seconds - travel_seconds)
            .filter(|gap| gap.is_finite());
        rows.push(PatternRow {
            person_id: person_id.to_owned(),
            person_zone: super::zones::zone_of_person(zone_system, person_id),
            planned_activities,
            activity_chain: join(&person_activities, |activity| activity.act_type.as_str()),
            mode_chain: person_legs
                .iter()
                .map(|leg| leg.mode.as_str())
                .collect::<Vec<_>>()
                .join("|"),
            journey_mode_chain: person_journeys
                .iter()
                .filter(|journey| journey.departure_seconds.is_some())
                .map(|journey| journey.main_mode.as_str())
                .collect::<Vec<_>>()
                .join("|"),
            planned_journeys,
            observed_journeys: person_journeys
                .iter()
                .filter(|journey| journey.departure_seconds.is_some())
                .count(),
            completed_journeys: person_journeys
                .iter()
                .filter(|journey| journey.duration_seconds.is_some())
                .count(),
            legs: person_legs.len(),
            first_start_seconds,
            last_end_seconds,
            observed_span_seconds,
            activity_seconds,
            travel_seconds,
            timeline_gap_seconds,
            left_censored_activities: person_activities
                .iter()
                .filter(|activity| activity.start_censored)
                .count(),
            right_censored_activities: person_activities
                .iter()
                .filter(|activity| activity.end_censored)
                .count(),
            status: pattern_status(
                planned_activities,
                observed_activities,
                stuck_at_seconds.contains_key(person_id),
            ),
            observed_activities,
        });
    }
    ActivityPatterns { rows, activities }
}

/// What the recorded selected plan says a person's day contains, used as the denominator of
/// [`pattern_status`].
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct PlannedDay {
    pub activities: usize,
    pub journeys: usize,
}

/// One person's observed records, held as indices into the vectors they came from.
#[derive(Default)]
struct PersonBucket {
    activities: Vec<usize>,
    journeys: Vec<usize>,
    legs: Vec<usize>,
}

/// Index every observed record by person in one pass over each vector.
///
/// Rescanning the three vectors per person would be quadratic in the population, so the
/// grouping is done once and each person then reads only their own indices.
fn group_by_person<'a>(
    activities: &'a [ActivityRow],
    journeys: &'a [JourneyRow],
    observed_legs: &'a [ObservedLeg],
    planned: &'a BTreeMap<String, PlannedDay>,
) -> BTreeMap<&'a str, PersonBucket> {
    let mut buckets: BTreeMap<&'a str, PersonBucket> = BTreeMap::new();
    for (index, activity) in activities.iter().enumerate() {
        buckets
            .entry(activity.person_id.as_str())
            .or_default()
            .activities
            .push(index);
    }
    for (index, journey) in journeys.iter().enumerate() {
        buckets
            .entry(journey.person_id.as_str())
            .or_default()
            .journeys
            .push(index);
    }
    for (index, leg) in observed_legs.iter().enumerate() {
        buckets
            .entry(leg.person_id.as_str())
            .or_default()
            .legs
            .push(index);
    }
    // A person the run never observed still needs a row, so their plan is reported as
    // `not_observed` rather than vanishing from the report.
    for person in planned.keys().map(String::as_str) {
        buckets.entry(person).or_default();
    }
    buckets
}

fn join<T>(values: &[&T], value: impl Fn(&T) -> &str) -> String {
    values
        .iter()
        .map(|item| value(item))
        .collect::<Vec<_>>()
        .join("|")
}

/// Sum of seconds, printed as a non-negative zero when there is nothing to add.
///
/// `f64`'s `Sum` seeds with `-0.0`, because that is the identity of float addition, so an
/// empty iterator sums to negative zero. The extra `+ 0.0` is what turns it back into the
/// `0.000000` a report should show for a total of nothing.
pub(super) fn total(seconds: impl Iterator<Item = f64>) -> f64 {
    seconds.sum::<f64>() + 0.0
}

pub(super) fn urban_area_of(classifications: &LinkClassifications, link_id: &str) -> String {
    classifications.get(link_id).map_or_else(
        || super::UNKNOWN.to_owned(),
        |ClassifiedLink { urban_area, .. }| urban_area.clone(),
    )
}

/// Per-activity-type totals. Uncensored durations only feed the duration columns, so a
/// censored activity never pulls a mean towards a lower bound it never finished reaching.
pub(super) fn write_activity_tables(
    path: &std::path::Path,
    patterns: &ActivityPatterns,
) -> Result<(), AnalysisError> {
    let mut durations = table_writer(path, "activity_durations.csv")?;
    writeln!(
        durations,
        "person_id,activity_sequence,act_type,stage,link_id,zone,urban_area,start_seconds,end_seconds,duration_seconds,in_window_seconds,start_censored,end_censored"
    )
    .map_err(io_error)?;
    for activity in &patterns.activities {
        writeln!(
            durations,
            "{},{},{},{},{},{},{},{},{},{},{},{},{}",
            csv(&activity.person_id),
            activity.activity_sequence,
            csv(&activity.act_type),
            activity.stage,
            csv(&activity.link_id),
            csv(&activity.zone),
            csv(&activity.urban_area),
            number_opt(activity.start_seconds),
            number_opt(activity.end_seconds),
            number_opt(activity.duration_seconds),
            number_opt(activity.in_window_seconds),
            activity.start_censored,
            activity.end_censored,
        )
        .map_err(io_error)?;
    }

    let mut chains = table_writer(path, "activity_patterns.csv")?;
    writeln!(
        chains,
        "person_id,person_zone,status,planned_activities,observed_activities,planned_journeys,observed_journeys,completed_journeys,legs,activity_chain,mode_chain,journey_mode_chain,first_start_seconds,last_end_seconds,observed_span_seconds,activity_seconds,travel_seconds,timeline_gap_seconds,left_censored_activities,right_censored_activities"
    )
    .map_err(io_error)?;
    for row in &patterns.rows {
        writeln!(
            chains,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{:.6},{:.6},{},{},{}",
            csv(&row.person_id),
            csv(&row.person_zone),
            row.status,
            row.planned_activities,
            row.observed_activities,
            row.planned_journeys,
            row.observed_journeys,
            row.completed_journeys,
            row.legs,
            csv(&row.activity_chain),
            csv(&row.mode_chain),
            csv(&row.journey_mode_chain),
            number_opt(row.first_start_seconds),
            number_opt(row.last_end_seconds),
            number_opt(row.observed_span_seconds),
            row.activity_seconds,
            row.travel_seconds,
            number_opt(row.timeline_gap_seconds),
            row.left_censored_activities,
            row.right_censored_activities,
        )
        .map_err(io_error)?;
    }
    Ok(())
}

/// Write the aggregated activity-pattern tables: one row per activity type, and one row per
/// pattern status, zone and total.
pub(super) fn write_summary_tables(
    path: &std::path::Path,
    patterns: &ActivityPatterns,
) -> Result<(), AnalysisError> {
    let mut types = table_writer(path, "activity_type_summary.csv")?;
    writeln!(
        types,
        "act_type,activities,persons,uncensored_activities,left_censored_activities,right_censored_activities,mean_duration_seconds,median_activity_duration_seconds,in_window_activity_seconds"
    )
    .map_err(io_error)?;
    let mut by_type: BTreeMap<&str, TypeGroup> = BTreeMap::new();
    for activity in &patterns.activities {
        by_type
            .entry(activity.act_type.as_str())
            .or_default()
            .observe(activity);
    }
    for (act_type, group) in &by_type {
        let mut durations = group.durations.clone();
        durations.sort_by(f64::total_cmp);
        writeln!(
            types,
            "{},{},{},{},{},{},{},{},{:.6}",
            csv(act_type),
            group.activities,
            group.persons.len(),
            group.uncensored,
            group.left_censored,
            group.right_censored,
            number_opt(super::mean(&durations)),
            number_opt(super::quantile(&durations, 0.5)),
            group.in_window_seconds,
        )
        .map_err(io_error)?;
    }

    let mut summary = table_writer(path, "activity_pattern_summary.csv")?;
    writeln!(
        summary,
        "group,category,persons,journeys,mean_journeys_per_person,activity_seconds,travel_seconds,mean_activity_seconds,mean_travel_seconds,left_censored_activities,right_censored_activities"
    )
    .map_err(io_error)?;
    // One long-format table covers the three groupings the report presents side by side, so a
    // cross-run comparison can concatenate it without knowing which grouping it is reading.
    let groupings: [(&str, fn(&PatternRow) -> String); 3] = [
        ("all", |_| "all_persons".to_owned()),
        ("person_zone", |row| row.person_zone.clone()),
        ("status", |row| row.status.to_owned()),
    ];
    for (group, key_of) in groupings {
        let mut buckets: BTreeMap<String, Vec<&PatternRow>> = BTreeMap::new();
        for row in &patterns.rows {
            buckets.entry(key_of(row)).or_default().push(row);
        }
        for (category, rows) in &buckets {
            writeln!(
                summary,
                // Every seconds column goes through `number_opt`, so a total and a mean are
                // written with the same six decimals a reader expects from the other tables.
                "{group},{},{},{},{},{},{},{},{},{},{}",
                csv(category),
                rows.len(),
                rows.iter().map(|row| row.observed_journeys).sum::<usize>(),
                number_opt(super::mean(&means(rows, |row| row.observed_journeys as f64))),
                number_opt(Some(total(rows.iter().map(|row| row.activity_seconds)))),
                number_opt(Some(total(rows.iter().map(|row| row.travel_seconds)))),
                number_opt(super::mean(&means(rows, |row| row.activity_seconds))),
                number_opt(super::mean(&means(rows, |row| row.travel_seconds))),
                rows.iter()
                    .map(|row| row.left_censored_activities)
                    .sum::<usize>(),
                rows.iter()
                    .map(|row| row.right_censored_activities)
                    .sum::<usize>(),
            )
            .map_err(io_error)?;
        }
    }
    Ok(())
}

fn means(rows: &[&PatternRow], value: impl Fn(&PatternRow) -> f64) -> Vec<f64> {
    rows.iter().copied().map(value).collect()
}

#[derive(Default)]
struct TypeGroup {
    activities: usize,
    persons: BTreeSet<String>,
    uncensored: usize,
    left_censored: usize,
    right_censored: usize,
    in_window_seconds: f64,
    durations: Vec<f64>,
}

impl TypeGroup {
    fn observe(&mut self, activity: &ActivityRow) {
        self.activities += 1;
        self.persons.insert(activity.person_id.clone());
        self.left_censored += usize::from(activity.start_censored);
        self.right_censored += usize::from(activity.end_censored);
        self.in_window_seconds =
            total(std::iter::once(self.in_window_seconds).chain(activity.in_window_seconds));
        // `duration_seconds` is already absent for a censored interval, so a duration that
        // survives here is one that was observed in full.
        if let Some(duration) = activity.duration_seconds {
            self.uncensored += 1;
            self.durations.push(duration);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::id::Id;
    use macros::deterministic_id_test;
    use std::collections::BTreeMap;

    fn start(seconds: f64, person: &str, act_type: &str, link: &str) -> Box<dyn EventTrait> {
        Box::new(ActivityStartEvent {
            time: SimTime::from_nanos((seconds * 1e9) as u64),
            person: Id::create(person),
            link: Id::create(link),
            coordinate: crate::simulation::scenario::Coordinate::new_2d(0.0, 0.0),
            act_type: Id::create(act_type),
            attributes: crate::simulation::InternalAttributes::default(),
        })
    }

    fn end(seconds: f64, person: &str, act_type: &str, link: &str) -> Box<dyn EventTrait> {
        Box::new(ActivityEndEvent {
            time: SimTime::from_nanos((seconds * 1e9) as u64),
            person: Id::create(person),
            link: Id::create(link),
            coordinate: crate::simulation::scenario::Coordinate::new_2d(0.0, 0.0),
            act_type: Id::create(act_type),
            attributes: crate::simulation::InternalAttributes::default(),
        })
    }

    #[deterministic_id_test]
    fn censors_the_first_and_last_activity_of_a_day_and_keeps_the_observed_share() {
        let mut collector = ActivityCollector::new(0.0);
        collector.process_timestamp(
            &[start(0.0, "p", "home", "home-link")],
            SimTime::from_secs(0),
        );
        collector.process_timestamp(
            &[end(3600.0, "p", "home", "home-link")],
            SimTime::from_secs(3600),
        );
        collector.process_timestamp(
            &[start(7200.0, "p", "work", "work-link")],
            SimTime::from_secs(7200),
        );
        collector.finish();

        assert_eq!(collector.activities.len(), 2);
        let home = &collector.activities[0];
        assert!(
            home.start_censored,
            "an activity starting at the window start was in progress before it"
        );
        // Left-censored, so the total duration is unknown, but its in-window share is not.
        assert_eq!(home.duration_seconds, None);
        assert_eq!(home.in_window_seconds, Some(3600.0));
        let work = &collector.activities[1];
        assert!(work.end_censored, "the run ended before this activity did");
        assert_eq!(work.in_window_seconds, None);
    }

    #[test]
    fn pattern_status_separates_truncation_from_ordinary_end_of_day_censoring() {
        assert_eq!(pattern_status(2, 0, false), "not_observed");
        assert_eq!(pattern_status(2, 2, true), "stuck");
        assert_eq!(pattern_status(3, 2, false), "truncated");
        // A right-censored final activity does not make the day incomplete.
        assert_eq!(pattern_status(2, 2, false), "complete");
    }

    #[deterministic_id_test]
    fn same_timestamp_activity_end_and_start_pair_independently_of_order() {
        for ends_first in [true, false] {
            let mut collector = ActivityCollector::new(0.0);
            collector.process_timestamp(
                &[start(0.0, "p", "home", "home-link")],
                SimTime::from_secs(0),
            );
            // A person who ends one activity and starts the next at the same instant is the
            // case where a batch's internal order decides the pairing if it is not handled.
            let batch: Vec<Box<dyn EventTrait>> = if ends_first {
                vec![
                    end(3600.0, "p", "home", "home-link"),
                    start(3600.0, "p", "work", "work-link"),
                ]
            } else {
                vec![
                    start(3600.0, "p", "work", "work-link"),
                    end(3600.0, "p", "home", "home-link"),
                ]
            };
            collector.process_timestamp(&batch, SimTime::from_secs(3600));
            collector.process_timestamp(
                &[end(7200.0, "p", "work", "work-link")],
                SimTime::from_secs(7200),
            );
            collector.finish();

            let types: Vec<_> = collector
                .activities
                .iter()
                .map(|activity| (activity.act_type.as_str(), activity.end_seconds))
                .collect();
            assert_eq!(
                types,
                [("home", Some(3600.0)), ("work", Some(7200.0))],
                "order of the batch changed the pairing"
            );
        }
    }

    #[test]
    fn an_unclassified_link_falls_back_to_unknown_not_to_a_neighbouring_area() {
        assert_eq!(
            urban_area_of(&BTreeMap::new(), "known"),
            super::super::UNKNOWN
        );
    }
}
