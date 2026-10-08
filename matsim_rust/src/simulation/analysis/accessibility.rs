//! Accessibility to supplied opportunities, measured from zones and from people.
//!
//! The module answers one question: how many jobs, schools or services can each origin
//! reach within a declared travel-time threshold. It answers it from three supplied files
//! and nothing else.
//!
//! - an opportunity file gives each location a coordinate, a category and a weight,
//! - a zone file gives zone centroids, which is the explicit coordinate/zone
//!   correspondence used to place both opportunity locations and person home locations,
//! - a travel-cost file gives *potential-destination* costs between zones, by mode and
//!   departure period.
//!
//! Realized trips are not potential destinations. A journey table says how long one person
//! actually took, which says nothing about how long anyone else could take, so the observed
//! leg and journey tables of this report are never a substitute for the supplied costs. An
//! origin with no supplied cost from it reports no measure at all rather than a value
//! derived from what happened to be travelled.
//!
//! The declared measure is [`MEASURE`]. It is cumulative and threshold-based: the summed
//! weight of every opportunity whose potential cost from the origin is **at or below** the
//! threshold. The threshold is inclusive, so a destination costing exactly the threshold
//! counts as reachable. A location whose cost is missing is excluded and counted
//! separately, because reading "unknown" as "too far" would understate accessibility and
//! reading it as "close" would overstate it.

use super::{
    AnalysisError, PersonExpectedTravel, csv, io_error, label, number_opt, quantile, table_writer,
    xml_escape,
};
use crate::simulation::config::Accessibility;
use csv::Reader as CsvReader;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// Name of the measure this module reports. Every exported row carries it, so a consumer
/// never has to guess which definition produced a number.
pub(super) const MEASURE: &str = "cumulative_opportunities_within_threshold";

/// The origin has a supplied cost to every location of the category.
const STATUS_AVAILABLE: &str = "available";
/// The origin has a supplied cost, but some locations of the category have none.
const STATUS_MISSING_COSTS: &str = "available_missing_costs";
/// No cost of this mode and period leaves the origin, so nothing can be reached from it.
const STATUS_NO_ORIGIN_COSTS: &str = "unavailable:no_origin_costs";
/// The cost file supplies no table for this mode and departure period.
const STATUS_NO_COST_TABLE: &str = "unavailable:no_travel_costs";
/// The person has no home activity, so no zone can be assigned.
const STATUS_NO_HOME_ZONE: &str = "unavailable:no_home_zone";
/// The weights of a category summed to something that is not a number. Every input is checked
/// individually, so this needs a category large enough for the sum itself to overflow.
const STATUS_NON_FINITE_MEASURE: &str = "unavailable:non_finite_measure";

/// Panels in the accessibility map. Each panel is one category, mode, period and threshold,
/// and a run with many of those would otherwise produce an SVG no browser wants to open, so
/// the map shows the first panels in sorted order and the diagnostics count the rest.
const MAP_PANEL_LIMIT: usize = 24;
const MAP_COLUMNS: usize = 3;
const MAP_PANEL_WIDTH: f64 = 270.0;
const MAP_PANEL_HEIGHT: f64 = 230.0;
const MAP_MARGIN: f64 = 8.0;

/// Single-hue ramp, light to dark. A magnitude is not a diverging quantity, so a sequential
/// ramp is used and each panel prints its own minimum and maximum.
const RAMP: [&str; 5] = ["#eff3ff", "#bdd7e7", "#6baed6", "#3182bd", "#08519c"];
/// Zones with no supplied cost are drawn outside the ramp so an unavailable origin is never
/// mistaken for a low-accessibility one.
const UNAVAILABLE_FILL: &str = "#d9d9d9";

#[derive(Deserialize)]
struct OpportunityRow {
    opportunity_id: String,
    category: String,
    x: f64,
    y: f64,
    count: f64,
}

#[derive(Deserialize)]
struct ZoneRow {
    zone_id: String,
    x: f64,
    y: f64,
}

#[derive(Deserialize)]
struct TravelCostRow {
    origin_zone: String,
    destination_zone: String,
    mode: String,
    period_start_seconds: u64,
    travel_time_seconds: f64,
}

/// A zone centroid. Zones are kept sorted by id, which makes both the exported row order
/// and the nearest-centroid tie-break independent of the input file's row order.
struct Zone {
    id: String,
    x: f64,
    y: f64,
}

/// One opportunity location, already assigned to the zone whose centroid is nearest.
struct Opportunity {
    id: String,
    category: String,
    x: f64,
    y: f64,
    count: f64,
    zone: usize,
}

/// Travel costs of one mode and departure period.
struct CostTable {
    mode: String,
    period_start_seconds: u64,
    /// Sparse on purpose. A cost file that covers only part of the zone set stays cheap to
    /// hold, and the cells it does not cover are exactly the reportable "no cost for this
    /// destination" case.
    costs: BTreeMap<(usize, usize), f64>,
    /// The origins this table has a cost for. Precomputed because the report asks the
    /// question once per origin, category and threshold, and answering it by scanning `costs`
    /// each time would be quadratic in the number of cells.
    origins_with_costs: BTreeSet<usize>,
}

impl CostTable {
    fn cost(&self, origin: usize, destination: usize) -> Option<f64> {
        self.costs.get(&(origin, destination)).copied()
    }

    /// Whether any cost leaves this origin, which decides between a measured value and
    /// `STATUS_NO_ORIGIN_COSTS`.
    fn has_costs_from(&self, origin: usize) -> bool {
        self.origins_with_costs.contains(&origin)
    }
}

/// Every cost table in the supplied file, sorted by mode and then departure period.
struct Skim {
    tables: Vec<CostTable>,
    /// Rows naming a zone the zone file does not list. A cost matrix is often rectangular
    /// and wider than the zones under study; such a row can name neither an origin nor a
    /// destination here, so it is counted instead of being read as a missing pair.
    unknown_zone_rows: usize,
    /// Rows read, including the ones with unknown zones.
    rows: usize,
}

impl Skim {
    fn find(&self, mode: &str, period_start_seconds: u64) -> Option<&CostTable> {
        self.tables
            .iter()
            .find(|table| table.mode == mode && table.period_start_seconds == period_start_seconds)
    }

    fn modes(&self) -> Vec<&str> {
        self.tables
            .iter()
            .map(|table| table.mode.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn periods(&self) -> Vec<u64> {
        self.tables
            .iter()
            .map(|table| table.period_start_seconds)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn cells(&self) -> usize {
        self.tables
            .iter()
            .map(|table| table.costs.len())
            .sum::<usize>()
    }
}

/// One reported combination of category, mode, departure period and threshold.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Cell {
    category: String,
    mode: String,
    period_start_seconds: u64,
    /// Index into the sorted threshold list. `f64` is not `Ord`, and two identical
    /// configured thresholds have to collapse into one row instead of being reported twice.
    threshold_index: usize,
}

/// The measure for one origin zone.
struct ZoneAccessibility {
    status: &'static str,
    /// `None` for every unavailable status.
    value: Option<MeasureValue>,
}

/// Cumulative opportunities of one origin zone, category and threshold.
struct MeasureValue {
    /// Summed weight of the locations at or below the threshold.
    opportunities: f64,
    reachable_locations: usize,
    unreachable_locations: usize,
    /// Locations the cost file does not cover, excluded from the measure.
    locations_without_cost: usize,
}

/// Totals of one category, independent of any origin.
struct CategoryTotals {
    opportunities: f64,
}

/// The computed report, shared by every table and the map.
struct Report {
    zones: Vec<Zone>,
    /// Opportunity locations per category, sorted by category then location id.
    categories: BTreeMap<String, Vec<Opportunity>>,
    totals: BTreeMap<String, CategoryTotals>,
    skim: Skim,
    thresholds: Vec<f64>,
    /// Persons per zone index.
    persons_per_zone: Vec<usize>,
    persons_without_home_zone: usize,
    /// The measure per cell and origin zone.
    accessibility: BTreeMap<Cell, BTreeMap<usize, ZoneAccessibility>>,
}

/// Read the three supplied files and write the accessibility tables, map and diagnostics.
///
/// `output_dir` resolves the relative paths the configuration may carry, the same way the
/// observed-data path and the comparison runs are resolved.
pub(super) fn write(
    staging: &Path,
    output_dir: &Path,
    settings: &Accessibility,
    expected_travel: &[PersonExpectedTravel],
    sample_size: f64,
) -> Result<(), AnalysisError> {
    let opportunities_path = resolve(output_dir, settings.opportunities.as_deref());
    let zones_path = resolve(output_dir, settings.zones.as_deref());
    let costs_path = resolve(output_dir, settings.travel_costs.as_deref());

    let zones = read_zones(&zones_path)?;
    let zone_index: BTreeMap<&str, usize> = zones
        .iter()
        .enumerate()
        .map(|(index, zone)| (zone.id.as_str(), index))
        .collect();
    let opportunities = read_opportunities(&opportunities_path, &zones)?;
    let skim = read_skim(&costs_path, &zone_index)?;
    let (persons_per_zone, persons_without_home_zone) = place_persons(expected_travel, &zones);

    let report = build_report(
        zones,
        opportunities,
        skim,
        settings.thresholds_seconds.clone(),
        persons_per_zone,
        persons_without_home_zone,
    );
    report.write_tables(staging, expected_travel, sample_size)
}

fn resolve(output_dir: &Path, path: Option<&Path>) -> PathBuf {
    match path {
        Some(path) if path.is_absolute() => path.to_owned(),
        Some(path) => output_dir.join(path),
        None => PathBuf::new(),
    }
}

/// Report a malformed supplied row with the line it came from, because the row number is
/// the only way a researcher can find the offending line in their own file.
fn invalid_row<T>(
    record: Result<T, csv::Error>,
    path: &Path,
    row_index: usize,
) -> Result<T, AnalysisError> {
    record.map_err(|error| {
        AnalysisError::new(format!(
            "invalid row {} in {}: {error}",
            row_index + 2,
            path.display()
        ))
    })
}

fn open(path: &Path) -> Result<CsvReader<File>, AnalysisError> {
    CsvReader::from_path(path)
        .map_err(|error| AnalysisError::new(format!("cannot read {}: {error}", path.display())))
}

fn read_zones(path: &Path) -> Result<Vec<Zone>, AnalysisError> {
    let mut reader = open(path)?;
    let mut zones: Vec<Zone> = Vec::new();
    for (row_index, record) in reader.deserialize::<ZoneRow>().enumerate() {
        let row = invalid_row(record, path, row_index)?;
        if !row.x.is_finite() || !row.y.is_finite() {
            return Err(AnalysisError::new(format!(
                "zone '{}' at row {} of {} has a non-finite coordinate",
                row.zone_id,
                row_index + 2,
                path.display()
            )));
        }
        zones.push(Zone {
            id: row.zone_id,
            x: row.x,
            y: row.y,
        });
    }
    if zones.is_empty() {
        return Err(AnalysisError::new(format!(
            "{} lists no zone, so neither an origin nor an opportunity could be placed",
            path.display()
        )));
    }
    zones.sort_by(|left, right| left.id.cmp(&right.id));
    for pair in zones.windows(2) {
        if pair[0].id == pair[1].id {
            return Err(AnalysisError::new(format!(
                "zone '{}' appears more than once in {}",
                pair[0].id,
                path.display()
            )));
        }
    }
    Ok(zones)
}

fn read_opportunities(path: &Path, zones: &[Zone]) -> Result<Vec<Opportunity>, AnalysisError> {
    let mut reader = open(path)?;
    let mut opportunities: Vec<Opportunity> = Vec::new();
    let mut seen = BTreeSet::new();
    for (row_index, record) in reader.deserialize::<OpportunityRow>().enumerate() {
        let row = invalid_row(record, path, row_index)?;
        if !row.x.is_finite() || !row.y.is_finite() {
            return Err(AnalysisError::new(format!(
                "opportunity '{}' at row {} of {} has a non-finite coordinate",
                row.opportunity_id,
                row_index + 2,
                path.display()
            )));
        }
        if !row.count.is_finite() || row.count < 0.0 {
            return Err(AnalysisError::new(format!(
                "opportunity '{}' at row {} of {} has an invalid weight {}. Weights must be finite and non-negative",
                row.opportunity_id,
                row_index + 2,
                path.display(),
                row.count
            )));
        }
        if !seen.insert(row.opportunity_id.clone()) {
            return Err(AnalysisError::new(format!(
                "opportunity '{}' appears more than once in {}",
                row.opportunity_id,
                path.display()
            )));
        }
        opportunities.push(Opportunity {
            id: row.opportunity_id,
            // A blank category is reported as its own group instead of failing the module,
            // matching how a missing link label stays visible as `unknown`.
            category: label(Some(row.category.as_str())),
            x: row.x,
            y: row.y,
            count: row.count,
            zone: nearest_zone(zones, row.x, row.y),
        });
    }
    // Group by category and then by location id, so the exported rows and the map panels do
    // not depend on the order the researcher's file happened to use.
    opportunities
        .sort_by(|left, right| (&left.category, &left.id).cmp(&(&right.category, &right.id)));
    Ok(opportunities)
}

fn read_skim(path: &Path, zone_index: &BTreeMap<&str, usize>) -> Result<Skim, AnalysisError> {
    let mut reader = open(path)?;
    let mut tables: BTreeMap<(String, u64), BTreeMap<(usize, usize), f64>> = BTreeMap::new();
    let mut unknown_zone_rows = 0;
    let mut rows = 0;
    for (row_index, record) in reader.deserialize::<TravelCostRow>().enumerate() {
        let row = invalid_row(record, path, row_index)?;
        rows += 1;
        if !row.travel_time_seconds.is_finite() || row.travel_time_seconds < 0.0 {
            return Err(AnalysisError::new(format!(
                "travel cost from zone '{}' to zone '{}' at row {} of {} is {}. Potential travel times must be finite and non-negative",
                row.origin_zone,
                row.destination_zone,
                row_index + 2,
                path.display(),
                row.travel_time_seconds
            )));
        }
        let (Some(origin), Some(destination)) = (
            zone_index.get(row.origin_zone.as_str()).copied(),
            zone_index.get(row.destination_zone.as_str()).copied(),
        ) else {
            unknown_zone_rows += 1;
            continue;
        };
        let costs = tables
            .entry((row.mode.clone(), row.period_start_seconds))
            .or_default();
        if costs
            .insert((origin, destination), row.travel_time_seconds)
            .is_some()
        {
            return Err(AnalysisError::new(format!(
                "more than one travel cost from zone '{}' to zone '{}' for mode '{}' at departure period {} s in {}",
                row.origin_zone,
                row.destination_zone,
                row.mode,
                row.period_start_seconds,
                path.display()
            )));
        }
    }
    Ok(Skim {
        tables: tables
            .into_iter()
            .map(|((mode, period_start_seconds), costs)| {
                let origins_with_costs = costs.keys().map(|(origin, _)| *origin).collect();
                CostTable {
                    mode,
                    period_start_seconds,
                    costs,
                    origins_with_costs,
                }
            })
            .collect(),
        unknown_zone_rows,
        rows,
    })
}

/// Assign each person to the zone nearest their home activity.
///
/// A person with no placeable home activity keeps a row of their own with
/// `STATUS_NO_HOME_ZONE`, so a person the analysis cannot place is counted rather than
/// dropped from the population.
fn place_persons(expected_travel: &[PersonExpectedTravel], zones: &[Zone]) -> (Vec<usize>, usize) {
    let mut per_zone = vec![0; zones.len()];
    let mut without_home_zone = 0;
    for person in expected_travel {
        match person.home_coord {
            Some([x, y]) if x.is_finite() && y.is_finite() => {
                per_zone[nearest_zone(zones, x, y)] += 1;
            }
            _ => without_home_zone += 1,
        }
    }
    (per_zone, without_home_zone)
}

/// Index of the zone whose centroid is nearest a coordinate.
///
/// The comparison is horizontal: the supplied files carry two-dimensional centroids, and
/// mixing a third dimension into it would move an assignment for a reason the researcher
/// never configured. Ties break on the zone id, because the zone list is sorted by id.
fn nearest_zone(zones: &[Zone], x: f64, y: f64) -> usize {
    zones
        .iter()
        .enumerate()
        .map(|(index, zone)| {
            let dx = x - zone.x;
            let dy = y - zone.y;
            (dx * dx + dy * dy, index)
        })
        .min_by(|left, right| {
            left.0
                .total_cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
        })
        .map(|(_, index)| index)
        .expect("the zone file has to hold at least one zone")
}

fn build_report(
    zones: Vec<Zone>,
    opportunities: Vec<Opportunity>,
    skim: Skim,
    thresholds: Vec<f64>,
    persons_per_zone: Vec<usize>,
    persons_without_home_zone: usize,
) -> Report {
    let mut categories: BTreeMap<String, Vec<Opportunity>> = BTreeMap::new();
    for opportunity in opportunities {
        categories
            .entry(opportunity.category.clone())
            .or_default()
            .push(opportunity);
    }
    let totals = categories
        .iter()
        .map(|(category, opportunities)| {
            (
                category.clone(),
                CategoryTotals {
                    opportunities: opportunities
                        .iter()
                        .map(|opportunity| opportunity.count)
                        .sum(),
                },
            )
        })
        .collect();

    // Sorted and deduplicated so one configured threshold yields exactly one set of rows and
    // the report reads from the smallest threshold to the largest.
    let mut thresholds = thresholds;
    thresholds.sort_by(f64::total_cmp);
    thresholds.dedup_by(|left, right| left.total_cmp(right).is_eq());

    let modes = skim.modes();
    let periods = skim.periods();
    let mut accessibility: BTreeMap<Cell, BTreeMap<usize, ZoneAccessibility>> = BTreeMap::new();
    // Every reported combination is materialised, including a mode and period the cost file
    // never pairs, because a gap in the skim is a reportable outcome rather than a silent one.
    // That makes the row count the full cross product, and `write_persons` repeats each cell
    // per person.
    // ponytail: the per-person table is cells x persons and is not capped. A wide skim over a
    // large population can reach tens of millions of rows. Add a person cap and a
    // `persons_omitted` diagnostic, or emit the per-person table only on request, if a run ever
    // needs it.
    for (category, locations) in &categories {
        for mode in &modes {
            for &period_start_seconds in &periods {
                let table = skim.find(mode, period_start_seconds);
                for (threshold_index, &threshold) in thresholds.iter().enumerate() {
                    let cell = Cell {
                        category: category.clone(),
                        mode: (*mode).to_owned(),
                        period_start_seconds,
                        threshold_index,
                    };
                    let by_zone = accessibility.entry(cell).or_default();
                    for origin in 0..zones.len() {
                        by_zone.insert(origin, measure(locations, table, origin, threshold));
                    }
                }
            }
        }
    }
    Report {
        zones,
        categories,
        totals,
        skim,
        thresholds,
        persons_per_zone,
        persons_without_home_zone,
        accessibility,
    }
}

/// Cumulative opportunities reachable from one origin zone within `threshold`.
///
/// `table` is the cost table of the cell's mode and departure period. A missing table, an
/// origin with no outgoing cost, and a location with no cost from the origin are three
/// different failures, and each is reported as its own status so an incomplete cost matrix
/// cannot be read as a low-accessibility zone.
fn measure(
    locations: &[Opportunity],
    table: Option<&CostTable>,
    origin: usize,
    threshold: f64,
) -> ZoneAccessibility {
    let Some(table) = table else {
        return unavailable(STATUS_NO_COST_TABLE);
    };
    if !table.has_costs_from(origin) {
        return unavailable(STATUS_NO_ORIGIN_COSTS);
    }
    let mut value = MeasureValue {
        opportunities: 0.0,
        reachable_locations: 0,
        unreachable_locations: 0,
        locations_without_cost: 0,
    };
    for location in locations {
        match table.cost(origin, location.zone) {
            // The threshold is inclusive, so a destination costing exactly the threshold is
            // part of the measure. The value it adds is a non-negative finite weight and
            // `threshold` is finite, so the sum cannot overflow to infinity in practice; a
            // single non-finite result is still refused rather than exported as a number.
            Some(cost) if cost <= threshold => {
                value.opportunities += location.count;
                value.reachable_locations += 1;
            }
            Some(_) => value.unreachable_locations += 1,
            None => value.locations_without_cost += 1,
        }
    }
    if !value.opportunities.is_finite() {
        return unavailable(STATUS_NON_FINITE_MEASURE);
    }
    ZoneAccessibility {
        status: if value.locations_without_cost > 0 {
            STATUS_MISSING_COSTS
        } else {
            STATUS_AVAILABLE
        },
        value: Some(value),
    }
}

fn unavailable(status: &'static str) -> ZoneAccessibility {
    ZoneAccessibility {
        status,
        value: None,
    }
}

impl Report {
    fn write_tables(
        &self,
        staging: &Path,
        expected_travel: &[PersonExpectedTravel],
        sample_size: f64,
    ) -> Result<(), AnalysisError> {
        self.write_zones(staging)?;
        self.write_persons(staging, expected_travel)?;
        self.write_summary(staging, sample_size)?;
        let rendered_panels = self.write_map(staging)?;
        self.write_diagnostics(staging, rendered_panels)
    }

    fn write_zones(&self, staging: &Path) -> Result<(), AnalysisError> {
        let mut writer = table_writer(staging, "accessibility_zones.csv")?;
        writeln!(writer, "{ZONES_HEADER}").map_err(io_error)?;
        for (cell, by_zone) in &self.accessibility {
            let total = self.totals[&cell.category].opportunities;
            for (origin, zone) in self.zones.iter().enumerate() {
                let accessibility = &by_zone[&origin];
                let value = accessibility.value.as_ref();
                writeln!(
                    writer,
                    "{},{},{},{},{},{},{},{},{},{},{},{},{}",
                    csv(&zone.id),
                    csv(&cell.category),
                    csv(&cell.mode),
                    cell.period_start_seconds,
                    number_opt(Some(self.thresholds[cell.threshold_index])),
                    MEASURE,
                    accessibility.status,
                    number_opt(value.map(|value| value.opportunities)),
                    count_of(value, |value| value.reachable_locations),
                    count_of(value, |value| value.unreachable_locations),
                    count_of(value, |value| value.locations_without_cost),
                    number_opt(total.is_finite().then_some(total)),
                    share(value.map(|value| value.opportunities), total),
                )
                .map_err(io_error)?;
            }
        }
        Ok(())
    }

    /// One row per person and cell, carrying the value of the person's own origin zone.
    ///
    /// The value is repeated per person on purpose: an equity analysis needs it per person,
    /// and a consumer that only wants zone values reads `accessibility_zones.csv`.
    fn write_persons(
        &self,
        staging: &Path,
        expected_travel: &[PersonExpectedTravel],
    ) -> Result<(), AnalysisError> {
        let mut writer = table_writer(staging, "accessibility_persons.csv")?;
        writeln!(writer, "{PERSONS_HEADER}").map_err(io_error)?;
        for person in expected_travel {
            let home = person
                .home_coord
                .filter(|[x, y]| x.is_finite() && y.is_finite());
            let origin = home.map(|[x, y]| nearest_zone(&self.zones, x, y));
            for (cell, by_zone) in &self.accessibility {
                let missing_home;
                let accessibility = match origin {
                    Some(origin) => &by_zone[&origin],
                    None => {
                        missing_home = missing_home_accessibility();
                        &missing_home
                    }
                };
                let value = accessibility.value.as_ref();
                writeln!(
                    writer,
                    "{},{},{},{},{},{},{},{},{},{},{}",
                    csv(&person.person_id),
                    number_opt(home.map(|[x, _]| x)),
                    number_opt(home.map(|[_, y]| y)),
                    origin.map_or_else(String::new, |origin| csv(&self.zones[origin].id)),
                    csv(&cell.category),
                    csv(&cell.mode),
                    cell.period_start_seconds,
                    number_opt(Some(self.thresholds[cell.threshold_index])),
                    MEASURE,
                    accessibility.status,
                    number_opt(value.map(|value| value.opportunities)),
                )
                .map_err(io_error)?;
            }
        }
        Ok(())
    }

    /// Zone totals, and the equity comparison between the zone mean and the mean a person
    /// actually experiences. The two differ exactly when opportunities are unevenly
    /// distributed over the population, which is the point of reporting both.
    fn write_summary(&self, staging: &Path, sample_size: f64) -> Result<(), AnalysisError> {
        let mut writer = table_writer(staging, "accessibility_summary.csv")?;
        writeln!(writer, "{SUMMARY_HEADER}").map_err(io_error)?;
        for (cell, by_zone) in &self.accessibility {
            let mut values: Vec<f64> = Vec::new();
            let mut persons_included = 0;
            let mut weighted_sum = 0.0;
            let mut without_costs = 0;
            for (origin, accessibility) in by_zone {
                match accessibility.value.as_ref() {
                    Some(value) => {
                        values.push(value.opportunities);
                        let persons = self.persons_per_zone[*origin];
                        persons_included += persons;
                        weighted_sum += value.opportunities * persons as f64;
                    }
                    None => without_costs += 1,
                }
            }
            values.sort_by(f64::total_cmp);
            // The mean and the quantiles describe the zones that have a supplied cost. A zone
            // without one is not an accessible zone holding zero opportunities, so folding it
            // in as a zero would bias every mean downwards.
            writeln!(
                writer,
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                csv(&cell.category),
                csv(&cell.mode),
                cell.period_start_seconds,
                number_opt(Some(self.thresholds[cell.threshold_index])),
                MEASURE,
                self.zones.len(),
                without_costs,
                persons_included,
                number_opt(Some(sample_size)),
                number_opt(finite(self.totals[&cell.category].opportunities)),
                number_opt(mean_of(&values)),
                number_opt(quantile(&values, 0.5)),
                number_opt(values.first().copied()),
                number_opt(values.last().copied()),
                number_opt(
                    (persons_included > 0).then_some(weighted_sum / persons_included as f64)
                ),
            )
            .map_err(io_error)?;
        }
        Ok(())
    }

    /// One small map per reported cell, drawn on a shared projection so the panels can be
    /// read against each other. Returns the number of panels drawn.
    fn write_map(&self, staging: &Path) -> Result<usize, AnalysisError> {
        let cells: Vec<&Cell> = self.accessibility.keys().collect();
        let rendered = cells.len().min(MAP_PANEL_LIMIT);
        let omitted = cells.len() - rendered;
        let columns = rendered.clamp(1, MAP_COLUMNS);
        let rows = rendered.div_ceil(columns);
        let width = columns as f64 * MAP_PANEL_WIDTH + MAP_MARGIN;
        // The truncation note needs its own strip below the last panel. Without one it lands
        // on the same baseline as that panel's own legend and the two print over each other.
        let note_height = if omitted > 0 { MAP_MARGIN + 16.0 } else { 0.0 };
        let height = rows as f64 * MAP_PANEL_HEIGHT + MAP_MARGIN + note_height;
        let bounds = self.bounds();
        let mut file =
            BufWriter::new(File::create(staging.join("accessibility_map.svg")).map_err(io_error)?);
        writeln!(
            file,
            "<svg id=\"accessibility-map\" xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {width:.0} {height:.0}\" role=\"img\" aria-label=\"Accessibility to supplied opportunities\" style=\"width:100%;height:auto;max-height:2400px\"><rect width=\"{width:.0}\" height=\"{height:.0}\" fill=\"white\"/>"
        )
        .map_err(io_error)?;
        for (panel, cell) in cells.iter().take(rendered).enumerate() {
            let column = panel % columns;
            let row = panel / columns;
            let origin_x = MAP_MARGIN + column as f64 * MAP_PANEL_WIDTH;
            let origin_y = MAP_MARGIN + row as f64 * MAP_PANEL_HEIGHT;
            self.write_panel(&mut file, cell, origin_x, origin_y, &bounds)?;
        }
        if omitted > 0 {
            writeln!(
                file,
                "<text x=\"{:.0}\" y=\"{:.0}\" font-size=\"12\" fill=\"#555\">{omitted} further combinations are in the CSV tables.</text>",
                MAP_MARGIN,
                rows as f64 * MAP_PANEL_HEIGHT + MAP_MARGIN + 12.0
            )
            .map_err(io_error)?;
        }
        writeln!(file, "</svg>").map_err(io_error)?;
        Ok(rendered)
    }

    fn write_panel(
        &self,
        file: &mut BufWriter<File>,
        cell: &Cell,
        origin_x: f64,
        origin_y: f64,
        bounds: &Bounds,
    ) -> Result<(), AnalysisError> {
        let title = format!(
            "{} | {} | departure {} s | within {} s",
            cell.category,
            cell.mode,
            cell.period_start_seconds,
            self.thresholds[cell.threshold_index]
        );
        writeln!(
            file,
            "<text x=\"{:.0}\" y=\"{:.0}\" font-size=\"11\" fill=\"#17212b\">{}</text>",
            origin_x,
            origin_y + 12.0,
            xml_escape(&title)
        )
        .map_err(io_error)?;
        let by_zone = &self.accessibility[cell];
        let measured: Vec<f64> = by_zone
            .values()
            .filter_map(|accessibility| {
                accessibility
                    .value
                    .as_ref()
                    .map(|value| value.opportunities)
            })
            .collect();
        let (minimum, maximum) = measured
            .iter()
            .fold((f64::INFINITY, 0.0f64), |(minimum, maximum), value| {
                (minimum.min(*value), maximum.max(*value))
            });
        let span = maximum - minimum;
        let project = |x: f64, y: f64| {
            (
                origin_x + 8.0 + (x - bounds.min_x) / bounds.width() * (MAP_PANEL_WIDTH - 16.0),
                origin_y + MAP_PANEL_HEIGHT
                    - 26.0
                    - (y - bounds.min_y) / bounds.height() * (MAP_PANEL_HEIGHT - 44.0),
            )
        };
        // The category's opportunity weight per destination zone, drawn as a ring around the
        // zone so the map shows what is being reached and not only how much of it.
        let mut destination_weights: BTreeMap<usize, f64> = BTreeMap::new();
        for location in &self.categories[&cell.category] {
            *destination_weights.entry(location.zone).or_default() += location.count;
        }
        let largest_weight = destination_weights.values().copied().fold(0.0, f64::max);
        for (zone_index, zone) in self.zones.iter().enumerate() {
            let (x, y) = project(zone.x, zone.y);
            let accessibility = &by_zone[&zone_index];
            let (fill, title) = match accessibility.value.as_ref() {
                Some(value) => {
                    // A single-valued cell has no span to normalise by, so its one value
                    // takes the middle step instead of dividing by zero.
                    let fraction = if span > 0.0 && span.is_finite() {
                        (value.opportunities - minimum) / span
                    } else {
                        0.5
                    };
                    (
                        ramp_color(fraction),
                        format!(
                            "{} | {} opportunities of {} within {} s",
                            zone.id,
                            number_opt(Some(value.opportunities)),
                            cell.category,
                            self.thresholds[cell.threshold_index]
                        ),
                    )
                }
                None => (
                    UNAVAILABLE_FILL,
                    format!("{} | {} | {}", zone.id, MEASURE, accessibility.status),
                ),
            };
            writeln!(
                file,
                "<circle cx=\"{x:.2}\" cy=\"{y:.2}\" r=\"6\" fill=\"{fill}\" stroke=\"#555\" stroke-width=\"0.5\" data-zone=\"{}\" data-status=\"{}\"><title>{}</title></circle>",
                xml_escape(&zone.id),
                accessibility.status,
                xml_escape(&title),
            )
            .map_err(io_error)?;
            if let Some(weight) = destination_weights.get(&zone_index) {
                let radius = if largest_weight > 0.0 {
                    7.0 + 9.0 * (weight / largest_weight).sqrt()
                } else {
                    7.0
                };
                writeln!(
                    file,
                    "<circle cx=\"{x:.2}\" cy=\"{y:.2}\" r=\"{radius:.2}\" fill=\"none\" stroke=\"#238b45\" stroke-width=\"1.5\"><title>{} holds {} opportunities of {}</title></circle>",
                    xml_escape(&zone.id),
                    number_opt(Some(*weight)),
                    xml_escape(&cell.category),
                )
                .map_err(io_error)?;
            }
        }
        writeln!(
            file,
            "<text x=\"{:.0}\" y=\"{:.0}\" font-size=\"10\" fill=\"#555\">{:.6} to {:.6} opportunities; gray is unavailable, green rings hold opportunities</text>",
            origin_x,
            origin_y + MAP_PANEL_HEIGHT - 6.0,
            if minimum.is_finite() { minimum } else { 0.0 },
            maximum,
        )
        .map_err(io_error)
    }

    /// Projection bounds over every zone centroid and opportunity location, so a panel shows
    /// the whole study area rather than only the part that happens to be reachable.
    fn bounds(&self) -> Bounds {
        let mut bounds = Bounds {
            min_x: f64::INFINITY,
            min_y: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            max_y: f64::NEG_INFINITY,
        };
        for zone in &self.zones {
            bounds.include(zone.x, zone.y);
        }
        for locations in self.categories.values() {
            for location in locations {
                bounds.include(location.x, location.y);
            }
        }
        bounds
    }

    fn write_diagnostics(
        &self,
        staging: &Path,
        rendered_panels: usize,
    ) -> Result<(), AnalysisError> {
        let mut writer = table_writer(staging, "accessibility_diagnostics.csv")?;
        writeln!(writer, "metric,value").map_err(io_error)?;
        let locations: usize = self.categories.values().map(Vec::len).sum();
        let weights: f64 = self
            .categories
            .values()
            .flatten()
            .map(|location| location.count)
            .sum();
        let measured_cells: usize = self
            .accessibility
            .values()
            .map(|by_zone| by_zone.values().filter(|zone| zone.value.is_some()).count())
            .sum();
        let unavailable_cells: usize = self
            .accessibility
            .values()
            .map(|by_zone| by_zone.values().filter(|zone| zone.value.is_none()).count())
            .sum();
        let rows: [(&str, String); 17] = [
            ("opportunity_locations", locations.to_string()),
            ("opportunity_weights", number_opt(finite(weights))),
            ("categories", self.categories.len().to_string()),
            ("zones", self.zones.len().to_string()),
            (
                "persons",
                (self.persons_per_zone.iter().sum::<usize>() + self.persons_without_home_zone)
                    .to_string(),
            ),
            (
                "persons_without_home_zone",
                self.persons_without_home_zone.to_string(),
            ),
            ("thresholds", self.thresholds.len().to_string()),
            ("modes", self.skim.modes().len().to_string()),
            ("departure_periods", self.skim.periods().len().to_string()),
            ("cost_tables", self.skim.tables.len().to_string()),
            ("cost_rows", self.skim.rows.to_string()),
            (
                "cost_rows_with_unknown_zones",
                self.skim.unknown_zone_rows.to_string(),
            ),
            ("cost_cells", self.skim.cells().to_string()),
            ("measured_zone_cells", measured_cells.to_string()),
            ("unavailable_zone_cells", unavailable_cells.to_string()),
            ("map_panels_rendered", rendered_panels.to_string()),
            (
                "map_panels_omitted",
                (self.accessibility.len() - rendered_panels).to_string(),
            ),
        ];
        for (metric, value) in rows {
            writeln!(writer, "{metric},{value}").map_err(io_error)?;
        }
        Ok(())
    }
}

/// The accessibility of a person with no placeable home activity. The same for every one of
/// that person's cells, because the person cannot be placed regardless of which cell is read.
fn missing_home_accessibility() -> ZoneAccessibility {
    unavailable(STATUS_NO_HOME_ZONE)
}

/// One count column of a zone or person row, blank when the origin has no measure.
///
/// Reading this from the measure rather than from the status is what keeps a blank cell from
/// meaning "zero opportunities" in an unavailable row.
fn count_of(value: Option<&MeasureValue>, count: impl Fn(&MeasureValue) -> usize) -> String {
    value.map_or_else(String::new, |value| count(value).to_string())
}

struct Bounds {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
}

impl Bounds {
    fn include(&mut self, x: f64, y: f64) {
        self.min_x = self.min_x.min(x);
        self.min_y = self.min_y.min(y);
        self.max_x = self.max_x.max(x);
        self.max_y = self.max_y.max(y);
    }

    /// Width and height never collapse to zero, so a study area on a single point or a
    /// straight line still projects instead of dividing by zero.
    fn width(&self) -> f64 {
        (self.max_x - self.min_x).max(1.0)
    }

    fn height(&self) -> f64 {
        (self.max_y - self.min_y).max(1.0)
    }
}

/// A weight sum, dropped when it is not a number.
///
/// Every input weight is checked, but a sum of individually finite weights can still overflow,
/// and `number_opt` formats a non-finite value as a literal `inf`. A blank cell is the
/// report's way of saying "not computable", which is what an overflowed sum is.
fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn mean_of(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    mean.is_finite().then_some(mean)
}

/// Reachable share of a category, blank when either side is unusable. A category with no weight
/// at all has no meaningful share, so it is left blank rather than reported as 0; a non-finite
/// total or quotient is left blank too, because printing a finite `0` or an `inf` would claim a
/// share the inputs do not support.
fn share(opportunities: Option<f64>, total: f64) -> String {
    match opportunities {
        Some(opportunities) if total.is_finite() && total > 0.0 => {
            number_opt(Some(opportunities / total))
        }
        _ => String::new(),
    }
}

fn ramp_color(fraction: f64) -> &'static str {
    if !fraction.is_finite() {
        return RAMP[0];
    }
    let bucket = (fraction.clamp(0.0, 1.0) * RAMP.len() as f64).floor() as usize;
    RAMP[bucket.min(RAMP.len() - 1)]
}

/// Header rows of the published tables, shared by the writer and by the empty writer so a
/// schema change cannot land in one and miss the other.
const ZONES_HEADER: &str = "origin_zone,category,mode,departure_period_start_seconds,threshold_seconds,measure,status,opportunities,reachable_opportunity_locations,unreachable_opportunity_locations,opportunity_locations_without_cost,total_opportunities,reachable_opportunity_share";
const PERSONS_HEADER: &str = "person_id,home_x,home_y,origin_zone,category,mode,departure_period_start_seconds,threshold_seconds,measure,status,opportunities";
const SUMMARY_HEADER: &str = "category,mode,departure_period_start_seconds,threshold_seconds,measure,origin_zones,zones_without_costs,persons_included,sample_size,total_opportunities,mean_opportunities,median_opportunities,min_opportunities,max_opportunities,population_weighted_opportunities";

/// Header-only tables for a run that configured no accessibility input, or one whose inputs
/// could not be read. The module status carries the reason; the diagnostics repeat it here so
/// the published report explains the empty tables on its own.
pub(super) fn write_empty(staging: &Path, reason: Option<&str>) -> Result<(), AnalysisError> {
    for (name, header) in [
        ("accessibility_zones.csv", ZONES_HEADER),
        ("accessibility_persons.csv", PERSONS_HEADER),
        ("accessibility_summary.csv", SUMMARY_HEADER),
    ] {
        fs::write(staging.join(name), format!("{header}\n")).map_err(io_error)?;
    }
    let mut diagnostics =
        File::create(staging.join("accessibility_diagnostics.csv")).map_err(io_error)?;
    writeln!(diagnostics, "metric,value").map_err(io_error)?;
    // `csv` quotes a value that needs it, and a reason is a sentence, so it usually does.
    writeln!(
        diagnostics,
        "module_error,{}",
        csv(reason.unwrap_or("not_configured"))
    )
    .map_err(io_error)?;
    writeln!(diagnostics, "measure,{}", csv(MEASURE)).map_err(io_error)?;
    fs::write(
        staging.join("accessibility_map.svg"),
        format!(
            "<svg id=\"accessibility-map\" xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 700 120\" role=\"img\" aria-label=\"Accessibility to supplied opportunities\"><rect width=\"700\" height=\"120\" fill=\"white\"/><text x=\"20\" y=\"50\" font-size=\"14\" fill=\"#17212b\">{}</text></svg>",
            if reason.is_some() {
                "Accessibility inputs could not be read; see accessibility_diagnostics.csv"
            } else {
                "No accessibility inputs configured"
            }
        ),
    )
    .map_err(io_error)
}
