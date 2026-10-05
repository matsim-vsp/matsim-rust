//! PCE-weighted traffic volumes and hourly volume/capacity (V/C) ratios.
//!
//! The conventions below mirror what the network model actually enforces, so the
//! exported utilization can be compared with the volumes and capacities the
//! simulation itself applied:
//!
//! * A link's network `capacity` is the capacity of the *whole* link per hour. It
//!   already accounts for the number of lanes, so it is never multiplied by
//!   `permlanes` a second time here.
//! * The flow cap of a link consumes `vehicle.pce()` and is built as
//!   `capacity * qsim.sample_size` per hour. Capacity is therefore expressed in
//!   PCE per hour, and the observed volumes are weighted by PCE as well.
//! * Only `qsim.sample_size` of the population is simulated, so the observed
//!   volumes are expanded to the unsampled network by dividing by that fraction.
//!   Raw vehicle counts, PCE-weighted sample volumes and expanded volumes are
//!   three distinct quantities and are exported as distinct columns.
//! * Intervals are left-closed and right-open, like the hourly volume table, and
//!   the interval width enters the ratio as `capacity * interval_hours`.

use crate::simulation::scenario::network::Link;

/// Lower edges of the fixed V/C histogram bins in utilization units. The last
/// edge starts the unbounded overflow bin, so the array describes
/// `VC_BIN_EDGES.len() - 1` bins plus one overflow bin.
pub const VC_BIN_EDGES: [f64; 12] = [0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.2];

/// Number of histogram bins: one per pair of adjacent edges plus the overflow bin.
pub const VC_BIN_COUNT: usize = VC_BIN_EDGES.len();

/// Why a volume/capacity ratio is or is not available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageStatus {
    Available,
    /// The link capacity is not a positive, finite number.
    InvalidCapacity,
    /// At least one vehicle in the interval has no usable PCE.
    MissingPce,
    /// The reported interval has no positive, finite width, so no rate or
    /// interval capacity can be derived from it.
    InvalidInterval,
    /// The ratio is not a finite number, e.g. an overflowing expansion.
    InvalidRatio,
}

impl UsageStatus {
    pub fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::InvalidCapacity => "unavailable:invalid_capacity",
            Self::MissingPce => "unavailable:missing_pce",
            Self::InvalidInterval => "unavailable:invalid_interval",
            Self::InvalidRatio => "unavailable:invalid_ratio",
        }
    }
}

/// Side of a link whose volumes are being weighed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowSide {
    Entry,
    Exit,
}

impl FlowSide {
    /// Name this side's ratio is known by, in the histogram and the metric catalog.
    pub fn metric_name(self) -> &'static str {
        match self {
            Self::Entry => "entry_vc",
            Self::Exit => "exit_vc",
        }
    }
}

/// Fixed-point scale used when accumulating PCE volumes.
///
/// PCE sums are kept as exact integers at this scale rather than summed as
/// binary floating point, because floating-point addition does not commute: the
/// same vehicles entering a link simultaneously could otherwise produce sums
/// that differ in the last bit, and a ratio sitting exactly on a bin edge would
/// then land in different histogram bins depending on the order in which
/// partitions happened to be replayed. Integer addition is exact and
/// order-independent, so the exported volumes and bins are reproducible.
const PCE_UNITS_PER_PCE: f64 = 1_000_000.0;

/// PCE-weighted volumes accumulated for one link in one reported interval.
///
/// Vehicle counts and PCE sums are kept apart: the counts are always complete,
/// while a PCE sum is only usable when no vehicle in the interval was left
/// unresolved.
#[derive(Clone, Copy, Default, Debug)]
pub struct IntervalVolumes {
    pub entries: u64,
    pub exits: u64,
    entry_pce_units: i128,
    exit_pce_units: i128,
    pub entry_unresolved_pce: u64,
    pub exit_unresolved_pce: u64,
}

impl IntervalVolumes {
    /// Record one observed link crossing on `side`.
    ///
    /// `pce` is `None` when the vehicle could not be resolved to a usable PCE.
    /// Such a crossing still counts as a vehicle, but it leaves the PCE total for
    /// the interval unusable instead of silently contributing zero. A PCE that
    /// cannot be represented exactly at [`PCE_UNITS_PER_PCE`] is treated the
    /// same way, because a total built from it would not be reproducible.
    pub fn record(&mut self, side: FlowSide, pce: Option<f64>) {
        let units = pce.and_then(|pce| Self::to_units(pce));
        let (vehicles, pce_units, unresolved) = match side {
            FlowSide::Entry => (
                &mut self.entries,
                &mut self.entry_pce_units,
                &mut self.entry_unresolved_pce,
            ),
            FlowSide::Exit => (
                &mut self.exits,
                &mut self.exit_pce_units,
                &mut self.exit_unresolved_pce,
            ),
        };
        *vehicles += 1;
        match units {
            Some(units) => *pce_units += units,
            None => *unresolved += 1,
        }
    }

    /// Exact PCE total of the interval, or `None` if any vehicle was unresolved.
    pub fn pce(&self, side: FlowSide) -> Option<f64> {
        let (units, unresolved) = match side {
            FlowSide::Entry => (self.entry_pce_units, self.entry_unresolved_pce),
            FlowSide::Exit => (self.exit_pce_units, self.exit_unresolved_pce),
        };
        if unresolved > 0 {
            return None;
        }
        Some(units as f64 / PCE_UNITS_PER_PCE)
    }

    fn to_units(pce: f64) -> Option<i128> {
        if !pce.is_finite() || pce <= 0.0 {
            return None;
        }
        let scaled = pce * PCE_UNITS_PER_PCE;
        if !scaled.is_finite() || scaled >= i64::MAX as f64 {
            return None;
        }
        let units = scaled.round();
        // A PCE below half a unit would round to zero, silently turning a vehicle that
        // was on the link into one that carried no weight at all. Report it as
        // unresolved instead, so the interval is flagged rather than under-counted.
        (units >= 1.0).then_some(units as i128)
    }
}

/// Everything needed to turn one side of a link's observed volumes into a ratio.
#[derive(Debug, Clone, Copy)]
pub struct RatioInput {
    /// Network capacity of the whole link in PCE per hour; never scaled by lanes.
    pub capacity_pce_per_hour: f64,
    /// Width of the reported interval in hours.
    pub interval_hours: f64,
    /// Simulated share of the population, i.e. `qsim.sample_size`.
    pub sample_size: f64,
    /// PCE-weighted volume observed in the sample, or `None` if any vehicle in
    /// the interval had an unusable PCE.
    pub pce: Option<f64>,
}

/// One side of a link's capacity utilization within a single interval.
///
/// Unavailable quantities are `None` and `status` names the reason. A capacity
/// problem only invalidates the ratio: the observed and expanded volumes stay
/// reportable because they do not depend on the capacity.
#[derive(Debug, Clone, Copy)]
pub struct RatioOutcome {
    pub status: UsageStatus,
    /// Interval capacity of the unsampled network in PCE.
    pub effective_capacity_pce: Option<f64>,
    /// Observed PCE volume expanded to the unsampled network.
    pub expanded_pce: Option<f64>,
    /// Expanded PCE volume per hour.
    pub flow_pce_per_hour: Option<f64>,
    /// Expanded PCE volume divided by the interval capacity.
    pub ratio: Option<f64>,
}

impl RatioOutcome {
    fn unavailable(status: UsageStatus, effective_capacity_pce: Option<f64>) -> Self {
        Self {
            status,
            effective_capacity_pce,
            expanded_pce: None,
            flow_pce_per_hour: None,
            ratio: None,
        }
    }
}

/// Width of the reported interval that the simulation actually covered, in hours.
///
/// The last interval is truncated when `simulation_end_time` is not a whole
/// multiple of `interval_seconds`, so it must be given only the capacity of the
/// window that exists. Without this the final interval is credited with a full
/// interval of capacity, which understates its flow and its V/C. Intervals at or
/// beyond the end time keep the full width, because events are still recorded
/// there.
pub fn covered_interval_hours(
    interval_start_seconds: u64,
    interval_seconds: u32,
    simulation_end_time: u32,
) -> f64 {
    let end = u64::from(simulation_end_time);
    let covered = if interval_start_seconds >= end {
        u64::from(interval_seconds)
    } else {
        u64::from(interval_seconds).min(end - interval_start_seconds)
    };
    covered as f64 / 3600.0
}

/// Capacity available in one interval for the unsampled network, in PCE.
///
/// Returns `None` for a capacity or interval width that cannot describe a
/// positive, finite amount of traffic.
pub fn effective_capacity_pce(capacity_pce_per_hour: f64, interval_hours: f64) -> Option<f64> {
    if !capacity_pce_per_hour.is_finite()
        || capacity_pce_per_hour <= 0.0
        || !interval_hours.is_finite()
        || interval_hours <= 0.0
    {
        return None;
    }
    let capacity = capacity_pce_per_hour * interval_hours;
    capacity.is_finite().then_some(capacity)
}

/// Compute the utilization of one side of a link in one interval.
///
/// The sample only observes `sample_size` of the population, so its PCE volume
/// is divided by that fraction before being compared with the interval capacity;
/// this reproduces exactly the ratio the flow cap enforced, because the cap
/// itself only releases `capacity * sample_size` per hour.
pub fn volume_capacity_ratio(input: &RatioInput) -> RatioOutcome {
    // An unusable interval width breaks the flow denominator and the capacity
    // alike, so nothing derived from it is reportable.
    if !input.interval_hours.is_finite() || input.interval_hours <= 0.0 {
        return RatioOutcome::unavailable(UsageStatus::InvalidInterval, None);
    }
    let effective_capacity =
        effective_capacity_pce(input.capacity_pce_per_hour, input.interval_hours);
    let Some(pce) = input.pce else {
        return RatioOutcome::unavailable(UsageStatus::MissingPce, effective_capacity);
    };
    if !input.sample_size.is_finite() || input.sample_size <= 0.0 {
        return RatioOutcome::unavailable(UsageStatus::InvalidRatio, effective_capacity);
    }
    // Only the ratio divides by the capacity, so the volumes above it stay valid
    // for a link whose capacity cannot be used.
    let expanded_pce = pce / input.sample_size;
    let flow_pce_per_hour = expanded_pce / input.interval_hours;
    let ratio = effective_capacity.map(|capacity| expanded_pce / capacity);
    let finite = [expanded_pce, flow_pce_per_hour]
        .into_iter()
        .all(f64::is_finite)
        && ratio.is_none_or(f64::is_finite);
    if !finite {
        return RatioOutcome {
            status: UsageStatus::InvalidRatio,
            effective_capacity_pce: effective_capacity,
            expanded_pce: None,
            flow_pce_per_hour: None,
            ratio: None,
        };
    }
    RatioOutcome {
        status: match effective_capacity {
            Some(_) => UsageStatus::Available,
            None => UsageStatus::InvalidCapacity,
        },
        effective_capacity_pce: effective_capacity,
        expanded_pce: Some(expanded_pce),
        flow_pce_per_hour: Some(flow_pce_per_hour),
        ratio,
    }
}

/// Index of the histogram bin holding `ratio`.
///
/// Bins are left-closed and right-open, so a value exactly on an edge belongs to
/// the higher bin; anything at or beyond the last edge is overflow. Values that
/// are not finite or not a non-negative ratio have no bin.
pub fn vc_bin_index(ratio: f64) -> Option<usize> {
    if !ratio.is_finite() || ratio < 0.0 {
        return None;
    }
    for (index, edge) in VC_BIN_EDGES.iter().enumerate().skip(1) {
        if ratio < *edge {
            return Some(index - 1);
        }
    }
    Some(VC_BIN_COUNT - 1)
}

/// Lower bound and upper bound of a bin; the overflow bin has no upper bound.
pub fn vc_bin_bounds(bin: usize) -> (f64, Option<f64>) {
    (VC_BIN_EDGES[bin], VC_BIN_EDGES.get(bin + 1).copied())
}

/// Capacity utilization of one link in one reported interval.
///
/// It carries only what the exported row and the histogram need: the interval
/// width and the sample size are already baked into the outcomes.
#[derive(Debug, Clone, Copy)]
pub struct LinkUtilization<'a> {
    pub link_id: &'a str,
    /// Whole-link network capacity in PCE per hour, exactly as read from the
    /// network and not scaled by the lane count.
    pub capacity_pce_per_hour: f64,
    /// Lane count of the link, reported for comparison only.
    pub permlanes: f64,
    pub entry_vehicles: u64,
    pub exit_vehicles: u64,
    pub entry: RatioOutcome,
    pub exit: RatioOutcome,
}

impl<'a> LinkUtilization<'a> {
    pub fn new(
        link: &'a Link,
        interval_hours: f64,
        sample_size: f64,
        volumes: &IntervalVolumes,
    ) -> Self {
        let input = |pce: Option<f64>| RatioInput {
            capacity_pce_per_hour: link.capacity,
            interval_hours,
            sample_size,
            pce,
        };
        Self {
            link_id: link.id.external(),
            capacity_pce_per_hour: link.capacity,
            permlanes: link.permlanes,
            entry_vehicles: volumes.entries,
            exit_vehicles: volumes.exits,
            entry: volume_capacity_ratio(&input(volumes.pce(FlowSide::Entry))),
            exit: volume_capacity_ratio(&input(volumes.pce(FlowSide::Exit))),
        }
    }

    fn side(&self, side: FlowSide) -> (&RatioOutcome, u64) {
        match side {
            FlowSide::Entry => (&self.entry, self.entry_vehicles),
            FlowSide::Exit => (&self.exit, self.exit_vehicles),
        }
    }
}

/// Fixed-bin distribution of one side's utilization over all link intervals of
/// a single reported interval.
#[derive(Debug, Clone)]
pub struct VcHistogram {
    pub bins: [u64; VC_BIN_COUNT],
    pub observations: u64,
    pub unused_links: u64,
    pub unavailable_links: u64,
}

impl Default for VcHistogram {
    fn default() -> Self {
        Self {
            bins: [0; VC_BIN_COUNT],
            observations: 0,
            unused_links: 0,
            unavailable_links: 0,
        }
    }
}

impl VcHistogram {
    pub fn observe(&mut self, link: &LinkUtilization<'_>, side: FlowSide) {
        let (outcome, vehicles) = link.side(side);
        // "Unused" means the link carried nothing on this side, whatever its capacity
        // says. A link that was idle but has an unusable capacity is still idle, and
        // counting it as unavailable would hide how much of the network is simply
        // not in use. Its capacity problem stays visible in the per-link status.
        if vehicles == 0 {
            self.unused_links += 1;
            return;
        }
        if !outcome.status.is_available() {
            self.unavailable_links += 1;
            return;
        }
        let Some(bin) = outcome.ratio.and_then(vc_bin_index) else {
            // An available ratio that cannot be binned would otherwise disappear.
            self.unavailable_links += 1;
            return;
        };
        self.bins[bin] += 1;
        self.observations += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available(ratio: f64) -> RatioOutcome {
        RatioOutcome {
            status: UsageStatus::Available,
            effective_capacity_pce: Some(1.0),
            expanded_pce: Some(ratio),
            flow_pce_per_hour: Some(ratio),
            ratio: Some(ratio),
        }
    }

    fn link_with(
        entry: RatioOutcome,
        exits: RatioOutcome,
        entry_vehicles: u64,
    ) -> LinkUtilization<'static> {
        LinkUtilization {
            link_id: "link",
            capacity_pce_per_hour: 1000.0,
            permlanes: 1.0,
            entry_vehicles,
            exit_vehicles: 0,
            entry,
            exit: exits,
        }
    }

    #[test]
    fn ratio_expands_the_sample_and_uses_the_interval_capacity() {
        let outcome = volume_capacity_ratio(&RatioInput {
            capacity_pce_per_hour: 3600.0,
            interval_hours: 0.5,
            sample_size: 0.5,
            pce: Some(4.5),
        });

        assert_eq!(outcome.status, UsageStatus::Available);
        assert_eq!(outcome.effective_capacity_pce, Some(1800.0));
        assert_eq!(outcome.expanded_pce, Some(9.0));
        assert_eq!(outcome.flow_pce_per_hour, Some(18.0));
        assert_eq!(outcome.ratio, Some(9.0 / 1800.0));
    }

    #[test]
    fn ratio_of_a_full_sample_needs_no_scaling() {
        let outcome = volume_capacity_ratio(&RatioInput {
            capacity_pce_per_hour: 1000.0,
            interval_hours: 1.0,
            sample_size: 1.0,
            pce: Some(10.0),
        });

        assert_eq!(outcome.expanded_pce, Some(10.0));
        assert_eq!(outcome.flow_pce_per_hour, Some(10.0));
        assert_eq!(outcome.ratio, Some(0.01));
    }

    #[test]
    fn invalid_capacity_only_invalidates_the_ratio() {
        for capacity in [0.0, -3600.0, f64::NAN, f64::INFINITY] {
            let outcome = volume_capacity_ratio(&RatioInput {
                capacity_pce_per_hour: capacity,
                interval_hours: 0.5,
                sample_size: 0.25,
                pce: Some(4.0),
            });
            assert_eq!(outcome.status, UsageStatus::InvalidCapacity);
            assert_eq!(outcome.effective_capacity_pce, None);
            assert_eq!(outcome.ratio, None);
            // The volumes do not involve the capacity, so they stay reportable.
            assert_eq!(outcome.expanded_pce, Some(16.0));
            assert_eq!(outcome.flow_pce_per_hour, Some(32.0));
        }
    }

    #[test]
    fn ratio_reports_unavailable_pce_and_interval() {
        let missing = volume_capacity_ratio(&RatioInput {
            capacity_pce_per_hour: 1000.0,
            interval_hours: 1.0,
            sample_size: 1.0,
            pce: None,
        });
        assert_eq!(missing.status, UsageStatus::MissingPce);
        assert_eq!(missing.ratio, None);
        // The capacity itself stays reportable, so the reader sees the denominator.
        assert_eq!(missing.effective_capacity_pce, Some(1000.0));
        assert_eq!(missing.expanded_pce, None);

        // A zero-width interval has neither a flow nor a capacity denominator, and
        // that is reported as its own reason rather than as a missing PCE.
        for interval_hours in [0.0, -1.0, f64::NAN] {
            let no_interval = volume_capacity_ratio(&RatioInput {
                capacity_pce_per_hour: 1000.0,
                interval_hours,
                sample_size: 1.0,
                pce: Some(10.0),
            });
            assert_eq!(no_interval.status, UsageStatus::InvalidInterval);
            assert_eq!(no_interval.effective_capacity_pce, None);
            assert_eq!(no_interval.expanded_pce, None);
            assert_eq!(no_interval.flow_pce_per_hour, None);
            assert_eq!(no_interval.ratio, None);
        }

        let no_sample = volume_capacity_ratio(&RatioInput {
            capacity_pce_per_hour: 1000.0,
            interval_hours: 1.0,
            sample_size: 0.0,
            pce: Some(10.0),
        });
        assert_eq!(no_sample.status, UsageStatus::InvalidRatio);

        let overflow = volume_capacity_ratio(&RatioInput {
            capacity_pce_per_hour: 1000.0,
            interval_hours: 1.0,
            sample_size: 0.5,
            pce: Some(f64::MAX),
        });
        assert_eq!(overflow.status, UsageStatus::InvalidRatio);
    }

    #[test]
    fn pce_totals_do_not_depend_on_the_order_of_the_crossings() {
        // Summing these as binary floating point would give a different last bit
        // depending on the order, which could move a ratio across a bin edge.
        let pce_values = [0.1, 0.2, 0.3];
        let orders = [[0, 1, 2], [2, 1, 0], [1, 2, 0]];
        let totals: Vec<_> = orders
            .iter()
            .map(|order| {
                let mut volumes = IntervalVolumes::default();
                for &index in order {
                    volumes.record(FlowSide::Entry, Some(pce_values[index]));
                }
                volumes.pce(FlowSide::Entry)
            })
            .collect();
        assert_eq!(totals[0], Some(0.6));
        assert!(totals.iter().all(|total| *total == totals[0]));
        // The unreproducible binary sum differs from the exact one, which is why the
        // accumulation is done in fixed-point integers.
        assert_ne!(0.1f64 + 0.2 + 0.3, 0.6);
    }

    #[test]
    fn unresolved_or_unrepresentable_pce_makes_the_total_unavailable() {
        for pce in [
            None,
            Some(0.0),
            Some(-1.0),
            Some(f64::NAN),
            Some(f64::INFINITY),
            Some(f64::MAX),
            // Below half a unit these would round to zero, so they are reported as
            // unresolved rather than counted as a vehicle carrying no weight.
            Some(1e-9),
            Some(4.0e-7),
        ] {
            let mut volumes = IntervalVolumes::default();
            volumes.record(FlowSide::Entry, pce);
            assert_eq!(volumes.pce(FlowSide::Entry), None, "pce {pce:?}");
            // The crossing itself is still counted as a vehicle.
            assert_eq!(volumes.entries, 1);
            assert_eq!(volumes.entry_unresolved_pce, 1);
        }
    }

    #[test]
    fn the_smallest_representable_pce_is_kept() {
        // Half a unit rounds up to one unit and stays reportable.
        let mut volumes = IntervalVolumes::default();
        volumes.record(FlowSide::Entry, Some(5e-7));
        assert_eq!(volumes.pce(FlowSide::Entry), Some(1e-6));
        assert_eq!(volumes.entry_unresolved_pce, 0);
    }

    #[test]
    fn realistic_pce_values_survive_the_fixed_point_scale_exactly() {
        for pce in [
            0.1, 0.05, 0.2, 0.25, 0.35, 1.0, 1.5, 2.0, 2.5, 3.0, 0.333, 1.234_567,
        ] {
            let mut volumes = IntervalVolumes::default();
            volumes.record(FlowSide::Entry, Some(pce));
            assert_eq!(volumes.pce(FlowSide::Entry), Some(pce), "pce {pce}");
        }
    }

    #[test]
    fn pce_totals_are_kept_separate_per_side() {
        let mut volumes = IntervalVolumes::default();
        volumes.record(FlowSide::Entry, Some(2.0));
        volumes.record(FlowSide::Exit, Some(1.0));
        volumes.record(FlowSide::Exit, None);

        assert_eq!(volumes.pce(FlowSide::Entry), Some(2.0));
        assert_eq!(volumes.pce(FlowSide::Exit), None);
        assert_eq!(volumes.entries, 1);
        assert_eq!(volumes.exits, 2);
        assert_eq!(volumes.exit_unresolved_pce, 1);
        assert_eq!(volumes.entry_unresolved_pce, 0);
    }

    #[test]
    fn the_final_interval_is_only_as_wide_as_the_window_that_exists() {
        // A 1 h interval with 1.5 h of simulation: the second interval really covers
        // only half an hour and must not be credited with a full hour of capacity.
        assert_eq!(covered_interval_hours(0, 3600, 5400), 1.0);
        assert_eq!(covered_interval_hours(3600, 3600, 5400), 0.5);
        // An exact multiple leaves every interval full width.
        assert_eq!(covered_interval_hours(0, 3600, 7200), 1.0);
        assert_eq!(covered_interval_hours(3600, 3600, 7200), 1.0);
        // Intervals at or past the end keep the full width, because events are still
        // recorded there and the simulation did not truncate them.
        assert_eq!(covered_interval_hours(7200, 3600, 5400), 1.0);
        assert_eq!(covered_interval_hours(5400, 1800, 5400), 0.5);
        // A sub-minute remainder still contributes its own width.
        assert_eq!(covered_interval_hours(0, 3600, 60), 60.0 / 3600.0);
    }

    #[test]
    fn a_truncated_final_interval_reports_the_higher_ratio() {
        // One PCE-1 vehicle in the half hour that the final interval really covers,
        // against 3600 PCE/h: 1 / (3600 * 0.5) rather than 1 / (3600 * 1).
        let truncated = covered_interval_hours(3600, 3600, 5400);
        let outcome = volume_capacity_ratio(&RatioInput {
            capacity_pce_per_hour: 3600.0,
            interval_hours: truncated,
            sample_size: 1.0,
            pce: Some(1.0),
        });
        assert_eq!(outcome.ratio, Some(1.0 / 1800.0));
        assert_eq!(outcome.flow_pce_per_hour, Some(2.0));
    }

    #[test]
    fn bin_index_uses_left_closed_bins_and_overflow() {
        assert_eq!(vc_bin_index(0.0), Some(0));
        assert_eq!(vc_bin_index(0.099_999), Some(0));
        assert_eq!(vc_bin_index(0.1), Some(1));
        assert_eq!(vc_bin_index(0.2), Some(2));
        assert_eq!(vc_bin_index(0.999_999), Some(9));
        assert_eq!(vc_bin_index(1.0), Some(10));
        assert_eq!(vc_bin_index(1.199_999), Some(10));
        assert_eq!(vc_bin_index(1.2), Some(VC_BIN_COUNT - 1));
        assert_eq!(vc_bin_index(42.0), Some(VC_BIN_COUNT - 1));
        assert_eq!(vc_bin_index(f64::NAN), None);
        assert_eq!(vc_bin_index(-0.1), None);
    }

    #[test]
    fn bin_bounds_cover_every_bin_once() {
        assert_eq!(vc_bin_bounds(0), (0.0, Some(0.1)));
        assert_eq!(vc_bin_bounds(VC_BIN_COUNT - 2), (1.0, Some(1.2)));
        assert_eq!(vc_bin_bounds(VC_BIN_COUNT - 1), (1.2, None));
    }

    #[test]
    fn histogram_separates_unused_and_unavailable_links() {
        let unused = link_with(available(0.0), available(0.0), 0);
        let lightly_used = link_with(available(0.0), available(0.0), 1);
        let mut busy = link_with(available(1.4), available(0.05), 12);
        busy.exit_vehicles = 12;
        let unavailable = link_with(
            RatioOutcome::unavailable(UsageStatus::InvalidCapacity, None),
            available(0.0),
            3,
        );

        let mut entry = VcHistogram::default();
        for link in [&unused, &lightly_used, &busy, &unavailable] {
            entry.observe(link, FlowSide::Entry);
        }
        assert_eq!(entry.observations, 2);
        // A lightly used link with a zero ratio is an observation, not an unused link.
        assert_eq!(entry.bins[0], 1);
        assert_eq!(entry.bins[VC_BIN_COUNT - 1], 1);
        assert_eq!(entry.unused_links, 1);
        assert_eq!(entry.unavailable_links, 1);

        let mut exit = VcHistogram::default();
        exit.observe(&busy, FlowSide::Exit);
        assert_eq!(exit.observations, 1);
        assert_eq!(exit.bins[0], 1);
        assert_eq!(exit.unused_links, 0);
        assert_eq!(exit.unavailable_links, 0);
    }

    #[test]
    fn an_idle_link_stays_unused_even_when_its_capacity_is_unusable() {
        // No vehicle ever crossed, so the link is idle. Reporting it as unavailable
        // instead would hide how much of the network is simply not in use.
        let idle_without_capacity = link_with(
            RatioOutcome::unavailable(UsageStatus::InvalidCapacity, None),
            available(0.0),
            0,
        );
        // A link that did carry traffic but cannot be weighted stays unavailable.
        let busy_without_capacity = link_with(
            RatioOutcome::unavailable(UsageStatus::MissingPce, Some(1000.0)),
            available(0.0),
            1,
        );

        let mut histogram = VcHistogram::default();
        histogram.observe(&idle_without_capacity, FlowSide::Entry);
        assert_eq!(histogram.unused_links, 1);
        assert_eq!(histogram.unavailable_links, 0);
        assert_eq!(histogram.observations, 0);

        let mut histogram = VcHistogram::default();
        histogram.observe(&busy_without_capacity, FlowSide::Entry);
        assert_eq!(histogram.unused_links, 0);
        assert_eq!(histogram.unavailable_links, 1);
        assert_eq!(histogram.observations, 0);
    }
}
