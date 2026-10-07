//! Zone-based origin-destination reporting: mode and time OD matrices, boundary-crossing
//! flows, and the urban-area summaries built from the supplied geography.
//!
//! The zone system is supplied, not inferred, and the report never silently drops what it
//! does not cover. Two geographies are consumed independently:
//!
//! - link geography (`ZoneSystem::link_zones`) locates the links journey origins,
//!   destinations and activities are reported on, plus the per-zone link counts;
//! - person geography (`ZoneSystem::person_zones`) locates the people themselves, which is
//!   what the per-zone resident counts and the per-zone pattern rows are built from.
//!
//! A link with no zone entry and a person with no zone entry both land in
//! [`activity_pattern::UNMAPPED`]. They still form OD rows and boundary crossings, so a
//! partial zone system accounts for every observed journey rather than reporting a matrix
//! that quietly adds up to less than `journeys.csv` does.
//!
//! The urban-area summary is a separate table rather than part of this module: it is derived
//! from the link classification the report already computes, so it stays available for a run
//! that supplies link labels but no zone system.

use super::activity_pattern::{ActivityPatterns, total};
use super::{AnalysisError, JourneyRow, LinkClassifications, csv, io_error, table_writer};
use crate::simulation::config::ZoneSystem;
use crate::simulation::scenario::network::Link;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};

/// Zone of a location the supplied zone system does not cover. Unmapped locations are
/// aggregated into this value so a partial zone system still accounts for every observed
/// journey instead of dropping it.
pub(super) const UNMAPPED: &str = "unmapped";

/// The three zone tables: file name and published header row.
///
/// [`write_od_tables`] and [`write_empty_tables`] both read this list, so the tables a run
/// without a zone system publishes are the same tables a run with one does — the headers
/// cannot drift apart and make an empty table look like a different metric.
const OD_TABLES: [(&str, &str); 3] = [
    (
        "zone_od.csv",
        "departure_hour_seconds,mode,origin_zone,destination_zone,crosses_zone_boundary,journeys,persons",
    ),
    (
        "zone_flows.csv",
        "boundary,from_zone,to_zone,mode,departure_hour_seconds,journeys,persons",
    ),
    (
        "zone_summary.csv",
        "zone,links,resident_persons,unmapped_persons,observing_persons,activities,in_window_activity_seconds,left_censored_activities,right_censored_activities,journeys_origin,journeys_destination",
    ),
];

/// The published header row of one zone table, looked up by file name.
///
/// A name that is not in the constant panics on the lookup rather than silently writing a
/// different table's header, so reordering [`OD_TABLES`] cannot pair the wrong header with a
/// file and the two writers cannot drift apart unnoticed.
fn header(file: &str) -> &'static str {
    OD_TABLES
        .iter()
        .find(|(name, _)| *name == file)
        .map(|(_, header)| *header)
        .unwrap_or_else(|| panic!("{file} is one of OD_TABLES"))
}

/// Open one zone table for writing, with its header row already written.
fn od_table(path: &std::path::Path, file: &str) -> Result<BufWriter<File>, AnalysisError> {
    let mut table = table_writer(path, file)?;
    writeln!(table, "{}", header(file)).map_err(io_error)?;
    Ok(table)
}

/// Publish the zone tables with their headers only, for a run that supplies no zone system.
///
/// The report links to these files unconditionally, so they exist either way; writing headers
/// rather than skipping the tables means a reader cannot mistake "no zone system" for "a run
/// with no journeys".
pub(super) fn write_empty_tables(path: &std::path::Path) -> Result<(), AnalysisError> {
    for (file, header) in OD_TABLES {
        fs::write(path.join(file), format!("{header}\n")).map_err(io_error)?;
    }
    Ok(())
}

/// Cells of one zonal OD matrix, with the distinct people behind them.
#[derive(Default)]
struct OdCell {
    journeys: u64,
    persons: BTreeSet<String>,
}

/// A directed crossing between two zones, aggregated over the whole day.
#[derive(Default)]
struct BoundaryFlow {
    journeys: u64,
    persons: BTreeSet<String>,
}

/// Per-zone totals, with counts of everything the zone system could not place.
#[derive(Default)]
struct ZoneTotals {
    links: u64,
    resident_persons: u64,
    /// Persons whose observed day has no zone: the person geography did not cover them.
    unmapped_persons: u64,
    observing_persons: BTreeSet<String>,
    activities: u64,
    in_window_activity_seconds: f64,
    left_censored_activities: u64,
    right_censored_activities: u64,
    journeys_origin: u64,
    journeys_destination: u64,
}

/// Per-urban-area totals, derived from the link classification the report already computes.
#[derive(Default)]
struct UrbanAreaTotals {
    links: u64,
    /// Persons the supplied person geography places in this area.
    residents: u64,
    /// Persons the person geography does not cover, reported under `unmapped` so a partial
    /// geography still accounts for the whole population.
    unmapped_residents: u64,
    activities: u64,
    /// Activities whose link the report does not classify, kept so the counts still reconcile
    /// with `activity_durations.csv` when the label set is partial.
    unclassified_activities: u64,
    in_window_activity_seconds: f64,
    left_censored_activities: u64,
    right_censored_activities: u64,
    journeys_origin: u64,
    journeys_destination: u64,
}

/// `zone_od.csv` and `zone_flows.csv` for a supplied zone system.
///
/// A journey that never departed has no time to assign to a matrix cell. It keeps its zone
/// counts in `zone_summary.csv` and its row in `journeys.csv`, but claiming an hour for it
/// would invent a movement the recording does not contain.
pub(super) fn write_od_tables(
    path: &std::path::Path,
    journeys: &[JourneyRow],
    patterns: &ActivityPatterns,
    ordered_links: &[&Link],
    zone_system: &ZoneSystem,
) -> Result<(), AnalysisError> {
    let zone_of_link = |link_id: &str| zone_of(zone_system, link_id);

    let mut od = od_table(path, "zone_od.csv")?;
    let mut flows = od_table(path, "zone_flows.csv")?;
    // Both aggregations are keyed so a row of one table describes the same journeys as a row
    // of the other: the OD matrix keeps its two zone axes, while the flow table collapses the
    // unordered pair into one named boundary.
    let mut cells: BTreeMap<(u64, String, String, String, bool), OdCell> = BTreeMap::new();
    let mut boundary_flows: BTreeMap<(String, String, String, String, u64), BoundaryFlow> =
        BTreeMap::new();
    for journey in journeys {
        let (Some(hour), origin, destination) = (
            journey.departure_hour,
            zone_of_link(&journey.origin_link),
            zone_of_link(&journey.destination_link),
        ) else {
            continue;
        };
        let crosses = origin != destination;
        let cell = cells
            .entry((
                hour,
                journey.main_mode.clone(),
                origin.clone(),
                destination.clone(),
                crosses,
            ))
            .or_default();
        cell.journeys += 1;
        cell.persons.insert(journey.person_id.clone());
        if !crosses {
            continue;
        }
        let flow = boundary_flows
            .entry((
                boundary_of(&origin, &destination),
                origin,
                destination,
                journey.main_mode.clone(),
                hour,
            ))
            .or_default();
        flow.journeys += 1;
        flow.persons.insert(journey.person_id.clone());
    }
    for ((hour, mode, origin, destination, crosses), cell) in &cells {
        writeln!(
            od,
            "{hour},{},{},{},{crosses},{},{}",
            csv(mode),
            csv(origin),
            csv(destination),
            cell.journeys,
            cell.persons.len(),
        )
        .map_err(io_error)?;
    }
    for ((boundary, from, to, mode, hour), flow) in &boundary_flows {
        writeln!(
            flows,
            "{},{},{},{},{hour},{},{}",
            csv(boundary),
            csv(from),
            csv(to),
            csv(mode),
            flow.journeys,
            flow.persons.len(),
        )
        .map_err(io_error)?;
    }

    let mut summary = od_table(path, "zone_summary.csv")?;
    for (zone, totals) in zone_totals(journeys, patterns, ordered_links, zone_system) {
        writeln!(
            summary,
            "{},{},{},{},{},{},{:.6},{},{},{},{}",
            csv(&zone),
            totals.links,
            totals.resident_persons,
            totals.unmapped_persons,
            totals.observing_persons.len(),
            totals.activities,
            totals.in_window_activity_seconds,
            totals.left_censored_activities,
            totals.right_censored_activities,
            totals.journeys_origin,
            totals.journeys_destination,
        )
        .map_err(io_error)?;
    }
    Ok(())
}

/// Zone totals for every zone the system names and every zone an observed location resolved
/// to, sorted so the export order never depends on iteration.
fn zone_totals(
    journeys: &[JourneyRow],
    patterns: &ActivityPatterns,
    ordered_links: &[&Link],
    zone_system: &ZoneSystem,
) -> BTreeMap<String, ZoneTotals> {
    let mut zones: BTreeMap<String, ZoneTotals> = BTreeMap::new();
    for link in ordered_links {
        zones
            .entry(zone_of(zone_system, link.id.external()))
            .or_default()
            .links += 1;
    }
    for zone in zone_system.person_zones.values() {
        zones.entry(zone.clone()).or_default().resident_persons += 1;
    }
    for activity in &patterns.activities {
        let totals = zones.entry(activity.zone.clone()).or_default();
        totals.activities += 1;
        totals.observing_persons.insert(activity.person_id.clone());
        totals.in_window_activity_seconds = total(
            std::iter::once(totals.in_window_activity_seconds).chain(activity.in_window_seconds),
        );
        totals.left_censored_activities += u64::from(activity.start_censored);
        totals.right_censored_activities += u64::from(activity.end_censored);
    }
    for row in &patterns.rows {
        if row.person_zone == UNMAPPED {
            zones
                .entry(UNMAPPED.to_owned())
                .or_default()
                .unmapped_persons += 1;
        }
    }
    // A journey with no observed departure is still an origin and a destination of a zone; it
    // simply has no interval to place in the matrix.
    for journey in journeys {
        zones
            .entry(zone_of(zone_system, &journey.origin_link))
            .or_default()
            .journeys_origin += 1;
        zones
            .entry(zone_of(zone_system, &journey.destination_link))
            .or_default()
            .journeys_destination += 1;
    }
    zones
}

/// `urban_area_summary.csv`, grouped by the geography the run supplied.
///
/// A supplied zone system is the report's own definition of an urban area, so when one is
/// configured the summary is keyed by it and the person geography contributes the residents.
/// Without a zone system the report falls back to the link classification it already computes,
/// which is the only urban-area grouping a run can supply without one.
///
/// The `geography` column names which of the two produced a row, so a saved table cannot be
/// read as the other. Locations the grouping does not cover are reported as `unmapped` or
/// `unknown` and are never dropped: a partial geography still accounts for every observed
/// journey.
pub(super) fn write_urban_area_table(
    path: &std::path::Path,
    journeys: &[JourneyRow],
    patterns: &ActivityPatterns,
    ordered_links: &[&Link],
    classifications: &LinkClassifications,
    zone_system: &ZoneSystem,
) -> Result<(), AnalysisError> {
    let from_zone_system = !zone_system.is_empty();
    let (geography, mut areas) = if from_zone_system {
        let mut areas: BTreeMap<String, UrbanAreaTotals> = BTreeMap::new();
        for link in ordered_links {
            areas
                .entry(zone_of(zone_system, link.id.external()))
                .or_default()
                .links += 1;
        }
        for zone in zone_system.person_zones.values() {
            areas.entry(zone.clone()).or_default().residents += 1;
        }
        for row in &patterns.rows {
            if row.person_zone == UNMAPPED {
                areas
                    .entry(UNMAPPED.to_owned())
                    .or_default()
                    .unmapped_residents += 1;
            }
        }
        ("zone_system", areas)
    } else {
        let mut areas: BTreeMap<String, UrbanAreaTotals> = BTreeMap::new();
        for link in ordered_links {
            if let Some(classified) = classifications.get(link.id.external()) {
                areas
                    .entry(classified.urban_area.clone())
                    .or_default()
                    .links += 1;
            }
        }
        ("link_classification", areas)
    };
    // The zone path has no per-activity urban area, so the activity's own zone is the key; the
    // classification path has one.
    let area_of_activity = |activity: &super::activity_pattern::ActivityRow| {
        if from_zone_system {
            activity.zone.clone()
        } else {
            activity.urban_area.clone()
        }
    };
    let area_of_journey = |link_id: &str| {
        if from_zone_system {
            Some(zone_of(zone_system, link_id))
        } else {
            classifications
                .get(link_id)
                .map(|classified| classified.urban_area.clone())
        }
    };

    for activity in patterns.activities.iter() {
        let totals = areas.entry(area_of_activity(activity)).or_default();
        if from_zone_system {
            // The zone system may cover a link the network does not, so an activity the
            // report cannot place is reported rather than attributed to an area.
            if !classifications.contains_key(activity.link_id.as_str()) {
                totals.unclassified_activities += 1;
                continue;
            }
        }
        totals.activities += 1;
        totals.in_window_activity_seconds = total(
            std::iter::once(totals.in_window_activity_seconds).chain(activity.in_window_seconds),
        );
        totals.left_censored_activities += u64::from(activity.start_censored);
        totals.right_censored_activities += u64::from(activity.end_censored);
    }
    for journey in journeys {
        for (link_id, is_origin) in [
            (&journey.origin_link, true),
            (&journey.destination_link, false),
        ] {
            let Some(area) = area_of_journey(link_id) else {
                continue;
            };
            let totals = areas.entry(area).or_default();
            if is_origin {
                totals.journeys_origin += 1;
            } else {
                totals.journeys_destination += 1;
            }
        }
    }

    let mut table = table_writer(path, "urban_area_summary.csv")?;
    writeln!(
        table,
        "urban_area,geography,links,residents,unmapped_residents,activities,unclassified_activities,in_window_activity_seconds,left_censored_activities,right_censored_activities,journeys_origin,journeys_destination"
    )
    .map_err(io_error)?;
    for (area, totals) in &areas {
        writeln!(
            table,
            "{},{},{},{},{},{},{},{:.6},{},{},{},{}",
            csv(area),
            geography,
            totals.links,
            totals.residents,
            totals.unmapped_residents,
            totals.activities,
            totals.unclassified_activities,
            totals.in_window_activity_seconds,
            totals.left_censored_activities,
            totals.right_censored_activities,
            totals.journeys_origin,
            totals.journeys_destination,
        )
        .map_err(io_error)?;
    }
    Ok(())
}

/// Canonical name of the unordered zone pair a journey crosses.
///
/// Sorted, so both directions of the same boundary share one name; that is what makes the flow
/// table a boundary report rather than a second copy of the OD matrix. An unmapped zone is
/// named like any other, so a crossing into unknown territory stays visible as one.
pub(super) fn boundary_of(from: &str, to: &str) -> String {
    if from <= to {
        format!("{from}|{to}")
    } else {
        format!("{to}|{from}")
    }
}

/// Zone of an external link ID, or `unmapped` when the supplied system does not cover it.
pub(super) fn zone_of(zone_system: &ZoneSystem, link_id: &str) -> String {
    zone_system
        .link_zones
        .get(link_id)
        .map_or_else(|| UNMAPPED.to_owned(), Clone::clone)
}

/// Zone of an external person ID, from the supplied person geography.
pub(super) fn zone_of_person(zone_system: &ZoneSystem, person_id: &str) -> String {
    zone_system
        .person_zones
        .get(person_id)
        .map_or_else(|| UNMAPPED.to_owned(), Clone::clone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn a_boundary_name_is_shared_by_both_directions() {
        assert_eq!(boundary_of("zone-a", "zone-b"), "zone-a|zone-b");
        assert_eq!(boundary_of("zone-b", "zone-a"), "zone-a|zone-b");
    }

    #[test]
    fn a_link_the_system_does_not_cover_stays_unmapped() {
        let zone_system = ZoneSystem {
            name: Some("test".to_owned()),
            link_zones: BTreeMap::from([("known".to_owned(), "zone-a".to_owned())]),
            person_zones: BTreeMap::new(),
        };
        assert_eq!(zone_of(&zone_system, "known"), "zone-a");
        assert_eq!(zone_of(&zone_system, "absent"), UNMAPPED);
        // A boundary into unknown territory is still named, so it is not lost.
        assert_eq!(
            boundary_of(UNMAPPED, "zone-a"),
            format!("{UNMAPPED}|zone-a")
        );
    }
}
