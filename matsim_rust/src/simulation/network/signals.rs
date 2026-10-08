//! Plan-based traffic signals, keyed by approach link.
//!
//! MATSim's signals contrib writes three companion files next to a run's output:
//! `output_signal_systems_v2.0.xml.gz` names which approach links carry a signal,
//! `output_signal_groups_v2.0.xml.gz` says which signal belongs to which group, and
//! `output_signal_control_v2.0.xml.gz` carries the timing plan -- a cycle length, an
//! offset, and an onset/dropping second for every group. Together they reduce to a
//! per-link list of green windows inside a repeating cycle.
//!
//! The rule is deliberately link-keyed: a vehicle standing on approach link `L` may
//! enter its next link when the current second falls inside one of `L`'s green
//! windows, and may always do so when `L` carries no signal at all. A vehicle is
//! released as soon as *any* group on its approach is green, matching MATSim's
//! "any green movement releases the vehicle" behaviour.
//!
//! # Deliberate deviation from MATSim Java
//!
//! MATSim resolves a turn against the signals of the *node* and compares the
//! vehicle's candidate next links against them, which is what lets opposing and
//! conflicting turns at one junction take turns releasing. This port keeps the
//! link keying of the input instead, because the Bangkok `build_signal_xml_*.py`
//! generators emit a `linkIdRef` per signal and nothing that names a movement. The
//! consequences are real and worth stating: turns off the same approach share a
//! single state, and turns from different approaches at the same junction never
//! contend. A vehicle therefore faces no turn-acceptance conflict, only a stop.
//!
//! There is a second deviation, in `SimNetworkPartition`, where a red signal
//! suspends the link's stuck timer. See that call site for the reasoning.

use crate::simulation::id::Id;
use crate::simulation::io::xml;
use crate::simulation::scenario::network::Link;
use nohash_hasher::{IntMap, IntSet};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use tracing::{info, warn};

/// Paths to the three MATSim signal files that together define the signal plan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignalFiles {
    pub systems: Option<std::path::PathBuf>,
    pub groups: Option<std::path::PathBuf>,
    pub control: Option<std::path::PathBuf>,
}

impl SignalFiles {
    /// Whether all three inputs needed to resolve a plan are present. Anything less
    /// than a complete triple is a configuration error, not a silent no-signal run.
    pub fn is_complete(&self) -> bool {
        self.systems.is_some() && self.groups.is_some() && self.control.is_some()
    }

    pub fn any_present(&self) -> bool {
        self.systems.is_some() || self.groups.is_some() || self.control.is_some()
    }
}

/// The repeating green windows of one signalised approach link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalWindows {
    cycle_secs: u32,
    /// Sorted, non-overlapping, non-empty windows within `[0, cycle_secs)`.
    green: Vec<(u32, u32)>,
}

impl SignalWindows {
    /// Builds the green windows of one group within `cycle_secs`.
    ///
    /// `onset` is taken modulo the cycle. `dropping` is expected in `0..=cycle`; a
    /// value past the cycle end is clamped, and a `dropping` below `onset` is read as
    /// a window that wraps the cycle boundary, which real plans do contain. A window
    /// whose normalised onset equals its dropping is zero-length and yields no green
    /// at all, so the group is permanently red; that is a deliberate reading of a
    /// degenerate input rather than a guess at intent.
    pub fn from_group(cycle_secs: u32, onset: u32, dropping: u32) -> Self {
        assert!(cycle_secs > 0, "signal cycle must be > 0, got {cycle_secs}");
        let start = onset % cycle_secs;
        let green = if dropping > start {
            vec![(start, dropping.min(cycle_secs))]
        } else if dropping < start {
            let mut windows = vec![(start, cycle_secs)];
            if dropping > 0 {
                windows.push((0, dropping));
            }
            windows
        } else {
            Vec::new()
        };
        Self { cycle_secs, green }
    }

    pub fn cycle_secs(&self) -> u32 {
        self.cycle_secs
    }

    /// Whether the approach is green at whole-second `secs`.
    ///
    /// Onset is inclusive and dropping exclusive, matching the second at which a
    /// MATSim group stops being green. Whole seconds are deliberate: the plans are
    /// expressed in seconds, so a vehicle sampled part-way through the second that
    /// carries an onset already sees that onset.
    pub fn is_green_at(&self, secs: u32) -> bool {
        if self.green.is_empty() {
            return false;
        }
        let t = secs % self.cycle_secs;
        self.green.iter().any(|&(s, e)| t >= s && t < e)
    }

    /// Total green seconds per cycle, useful for reporting a plan's restrictiveness.
    pub fn green_secs(&self) -> u32 {
        self.green.iter().map(|(s, e)| e - s).sum()
    }

    /// Green fraction of a cycle in `0.0..=1.0`.
    pub fn green_fraction(&self) -> f64 {
        f64::from(self.green_secs()) / f64::from(self.cycle_secs)
    }

    fn merge(mut self, other: Self) -> Self {
        debug_assert_eq!(self.cycle_secs, other.cycle_secs);
        self.green.extend(other.green);
        self.green.sort_unstable();
        // Coalesce touching or overlapping windows so the stored form is canonical
        // regardless of the order groups were visited in.
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(self.green.len());
        for (s, e) in self.green.drain(..) {
            match merged.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        self.green = merged;
        self
    }
}

/// Signal green windows for every signalised approach link in a scenario.
///
/// A link that is absent from `by_link` is unsignalised and always discharges.
#[derive(Debug, Clone, Default)]
pub struct Signals {
    by_link: IntMap<Id<Link>, SignalWindows>,
    cycle_secs: u32,
}

impl Signals {
    /// Whether a vehicle on `link` may enter its next link at whole-second `secs`.
    pub fn allows_departure(&self, link: &Id<Link>, secs: u32) -> bool {
        match self.by_link.get(link) {
            Some(windows) => windows.is_green_at(secs),
            None => true,
        }
    }

    /// Whether `link` carries a signal at all, independent of the current state.
    pub fn is_signalised(&self, link: &Id<Link>) -> bool {
        self.by_link.contains_key(link)
    }

    pub fn signalised_links(&self) -> usize {
        self.by_link.len()
    }

    pub fn cycle_secs(&self) -> u32 {
        self.cycle_secs
    }

    /// Installs a plan for one link, bypassing file parsing.
    ///
    /// Exists for tests that need a specific timing without writing three files; the
    /// parser is covered separately by the tests in this module.
    #[cfg(test)]
    pub(crate) fn insert_for_test(&mut self, link: Id<Link>, windows: SignalWindows) {
        self.by_link.insert(link, windows);
    }

    /// Drops every entry whose link is not in `keep`.
    ///
    /// Signal state depends only on the link and the time, so it partitions with the
    /// link. Filtering once at construction keeps each partition holding only the
    /// plan for the links it owns instead of a copy of the whole city's signals.
    pub fn retain_links(&mut self, keep: &IntSet<Id<Link>>) {
        self.by_link.retain(|link, _| keep.contains(link));
    }

    /// Resolves the three MATSim signal files into per-link green windows.
    pub fn from_files(files: &SignalFiles) -> Result<Self, String> {
        let (systems, groups, control) = match (&files.systems, &files.groups, &files.control) {
            (Some(s), Some(g), Some(c)) => (s, g, c),
            _ => {
                return Err(
                    "signals need all three of systems, groups and control files".to_string(),
                );
            }
        };

        let io_systems: IOSignalSystems = read(systems);
        let io_groups: IOSignalGroups = read(groups);
        let io_control: IOSignalControl = read(control);

        // system id -> (signal id -> approach link)
        let mut approach: BTreeMap<u32, BTreeMap<u32, Id<Link>>> = BTreeMap::new();
        for system in &io_systems.systems {
            let entry = approach.entry(system.id).or_default();
            for signal in &system.signals.signals {
                let link = Id::get_from_ext(&signal.link_id_ref);
                if let Some(previous) = entry.insert(signal.id, link.clone())
                    && previous != link
                {
                    warn!(
                        "signal {} of system {} is defined on two approach links; keeping {}",
                        signal.id,
                        system.id,
                        link.external()
                    );
                }
            }
        }

        // system id -> group id -> signal ids
        let mut membership: BTreeMap<u32, BTreeMap<u32, Vec<u32>>> = BTreeMap::new();
        for system in &io_groups.systems {
            let entry = membership.entry(system.ref_id).or_default();
            for group in &system.groups {
                entry.insert(group.id, group.signals.iter().map(|s| s.ref_id).collect());
            }
        }

        // system id -> the cycle and the timed groups of that system
        let mut timing: BTreeMap<u32, SystemTiming> = BTreeMap::new();
        for system in &io_control.systems {
            let controller = match &system.controller {
                Some(c) => c,
                None => {
                    warn!(
                        "signal system {} has no controller in the signal control file",
                        system.ref_id
                    );
                    continue;
                }
            };
            for plan in &controller.plans {
                let cycle = plan.cycle_time.as_ref().map(|c| c.sec).unwrap_or(0);
                if cycle == 0 {
                    warn!(
                        "signal system {} has a plan with no cycle time; skipping the plan",
                        system.ref_id
                    );
                    continue;
                }
                let offset = plan.offset.as_ref().map(|o| o.sec).unwrap_or(0);
                let entry = timing
                    .entry(system.ref_id)
                    .or_insert_with(|| SystemTiming::new(cycle));
                for setting in &plan.group_settings {
                    match (
                        setting.onset.as_ref().map(|o| o.sec),
                        setting.dropping.as_ref().map(|d| d.sec),
                    ) {
                        (Some(onset), Some(dropping)) => entry.groups.push(GroupTiming {
                            group_id: setting.ref_id,
                            // The offset shifts the whole plan in time. It is added
                            // before normalisation so an offset that pushes an onset past
                            // the cycle end still wraps correctly.
                            onset: onset.saturating_add(offset),
                            dropping: dropping.saturating_add(offset),
                        }),
                        _ => warn!(
                            "signal group {} of system {} lacks an onset or dropping second; \
                             treating the group as always red",
                            setting.ref_id, system.ref_id
                        ),
                    }
                }
            }
        }

        let mut by_link: IntMap<Id<Link>, SignalWindows> = IntMap::default();

        for (system_id, system_timing) in &timing {
            let signals_of_system = match approach.get(system_id) {
                Some(s) => s,
                None => {
                    warn!(
                        "signal control names system {system_id}, which the systems file does \
                         not define; skipping it"
                    );
                    continue;
                }
            };
            let groups_of_system = membership.get(system_id);
            for group in &system_timing.groups {
                // A group can only be timed if we know which approach links it governs.
                let governed = match groups_of_system.and_then(|g| g.get(&group.group_id)) {
                    Some(signal_ids) => signal_ids,
                    None => {
                        warn!(
                            "signal control times group {} of system {system_id}, which the \
                             groups file does not define; skipping the group",
                            group.group_id
                        );
                        continue;
                    }
                };
                for signal_id in governed {
                    let link = match signals_of_system.get(signal_id) {
                        Some(link) => link.clone(),
                        None => {
                            warn!(
                                "group {} of system {system_id} references signal {signal_id}, \
                                 which the systems file does not define; skipping the signal",
                                group.group_id
                            );
                            continue;
                        }
                    };
                    let windows = SignalWindows::from_group(
                        system_timing.cycle_secs,
                        group.onset,
                        group.dropping,
                    );
                    match by_link.get_mut(&link) {
                        // A link governed by several groups is green as soon as any of
                        // them is, so the link's windows are the union.
                        Some(existing) => *existing = existing.clone().merge(windows),
                        None => {
                            by_link.insert(link, windows);
                        }
                    }
                }
            }
        }

        let cycle_secs = by_link
            .values()
            .next()
            .map(SignalWindows::cycle_secs)
            .unwrap_or(0);

        let restrictiveness = summarise(&by_link);
        info!(
            "Signals: {} approach links signalised, cycle {} s, green fraction \
             median {:.2} min {:.2} max {:.2}",
            by_link.len(),
            cycle_secs,
            restrictiveness.median,
            restrictiveness.min,
            restrictiveness.max
        );

        Ok(Self {
            by_link,
            cycle_secs,
        })
    }
}

/// The cycle and timed groups of one signal system, as written in the control file.
struct SystemTiming {
    cycle_secs: u32,
    groups: Vec<GroupTiming>,
}

impl SystemTiming {
    fn new(cycle_secs: u32) -> Self {
        Self {
            cycle_secs,
            groups: Vec::new(),
        }
    }
}

struct GroupTiming {
    group_id: u32,
    onset: u32,
    dropping: u32,
}

struct Restrictiveness {
    median: f64,
    min: f64,
    max: f64,
}

fn summarise(by_link: &IntMap<Id<Link>, SignalWindows>) -> Restrictiveness {
    let mut fractions: Vec<f64> = by_link
        .values()
        .map(SignalWindows::green_fraction)
        .collect();
    if fractions.is_empty() {
        return Restrictiveness {
            median: 0.0,
            min: 0.0,
            max: 0.0,
        };
    }
    fractions.sort_by(|a, b| a.partial_cmp(b).expect("green fractions are finite"));
    Restrictiveness {
        median: fractions[fractions.len() / 2],
        min: fractions[0],
        max: fractions[fractions.len() - 1],
    }
}

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> T {
    xml::read_from_file(path)
}

// ---------------------------------------------------------------------------
// Input types. These mirror the three MATSim v2.0 signal DTDs and are private to
// this module; the runtime model above is what the rest of the engine sees.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename = "signalSystems")]
struct IOSignalSystems {
    #[serde(rename = "signalSystem", default)]
    systems: Vec<IOSignalSystem>,
}

#[derive(Debug, Deserialize)]
struct IOSignalSystem {
    #[serde(rename = "@id")]
    id: u32,
    signals: IOSignals,
}

#[derive(Debug, Deserialize)]
struct IOSignals {
    #[serde(rename = "signal", default)]
    signals: Vec<IOSignal>,
}

#[derive(Debug, Deserialize)]
struct IOSignal {
    #[serde(rename = "@id")]
    id: u32,
    #[serde(rename = "@linkIdRef")]
    link_id_ref: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "signalGroups")]
struct IOSignalGroups {
    #[serde(rename = "signalSystem", default)]
    systems: Vec<IOSignalGroupsSystem>,
}

#[derive(Debug, Deserialize)]
struct IOSignalGroupsSystem {
    #[serde(rename = "@refId")]
    ref_id: u32,
    #[serde(rename = "signalGroup", default)]
    groups: Vec<IOSignalGroup>,
}

#[derive(Debug, Deserialize)]
struct IOSignalGroup {
    #[serde(rename = "@id")]
    id: u32,
    #[serde(rename = "signal", default)]
    signals: Vec<IOSignalRef>,
}

#[derive(Debug, Deserialize)]
struct IOSignalRef {
    #[serde(rename = "@refId")]
    ref_id: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "signalControl")]
struct IOSignalControl {
    #[serde(rename = "signalSystem", default)]
    systems: Vec<IOSignalControlSystem>,
}

#[derive(Debug, Deserialize)]
struct IOSignalControlSystem {
    #[serde(rename = "@refId")]
    ref_id: u32,
    #[serde(rename = "signalSystemController", default)]
    controller: Option<IOSignalSystemController>,
}

#[derive(Debug, Deserialize)]
struct IOSignalSystemController {
    #[serde(rename = "signalPlan", default)]
    plans: Vec<IOSignalPlan>,
}

#[derive(Debug, Deserialize)]
struct IOSignalPlan {
    #[serde(rename = "cycleTime", default)]
    cycle_time: Option<IOSeconds>,
    #[serde(rename = "offset", default)]
    offset: Option<IOSeconds>,
    #[serde(rename = "signalGroupSettings", default)]
    group_settings: Vec<IOSignalGroupSettings>,
}

#[derive(Debug, Deserialize)]
struct IOSignalGroupSettings {
    #[serde(rename = "@refId")]
    ref_id: u32,
    #[serde(rename = "onset", default)]
    onset: Option<IOSeconds>,
    #[serde(rename = "dropping", default)]
    dropping: Option<IOSeconds>,
}

#[derive(Debug, Deserialize)]
struct IOSeconds {
    #[serde(rename = "@sec")]
    sec: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use macros::deterministic_id_test;

    #[test]
    fn window_is_green_between_onset_and_dropping() {
        let w = SignalWindows::from_group(180, 113, 174);
        assert!(!w.is_green_at(112));
        assert!(w.is_green_at(113), "onset is inclusive");
        assert!(w.is_green_at(173));
        assert!(!w.is_green_at(174), "dropping is exclusive");
    }

    #[test]
    fn window_repeats_every_cycle() {
        let w = SignalWindows::from_group(180, 113, 174);
        assert!(!w.is_green_at(0));
        assert!(w.is_green_at(113 + 180));
        assert!(!w.is_green_at(113 + 360 - 1));
        assert!(w.is_green_at(113 + 2 * 180));
    }

    #[test]
    fn full_cycle_green_stays_green_everywhere() {
        let w = SignalWindows::from_group(180, 0, 180);
        for secs in [0, 1, 89, 90, 179, 180, 181, 359] {
            assert!(w.is_green_at(secs), "expected green at {secs}");
        }
        assert!((w.green_fraction() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn window_wrapping_the_cycle_boundary_becomes_two_windows() {
        let w = SignalWindows::from_group(180, 170, 20);
        assert!(w.is_green_at(170));
        assert!(w.is_green_at(179));
        assert!(w.is_green_at(0));
        assert!(w.is_green_at(19));
        assert!(!w.is_green_at(20));
        assert!(!w.is_green_at(100));
        assert!((w.green_fraction() - 30.0 / 180.0).abs() < 1e-12);
    }

    #[test]
    fn zero_length_window_is_always_red() {
        let w = SignalWindows::from_group(180, 5, 5);
        assert!(!w.is_green_at(5));
        assert!(!w.is_green_at(100));
        assert_eq!(w.green_secs(), 0);
    }

    #[test]
    fn dropping_past_the_cycle_end_is_clamped() {
        let w = SignalWindows::from_group(180, 170, 400);
        assert!(w.is_green_at(179));
        assert_eq!(w.green_secs(), 10);
    }

    #[test]
    fn merging_groups_unions_their_windows() {
        let a = SignalWindows::from_group(180, 0, 60);
        let b = SignalWindows::from_group(180, 40, 100);
        let merged = a.merge(b);
        assert!(merged.is_green_at(10));
        assert!(merged.is_green_at(50), "overlapping windows are coalesced");
        assert!(merged.is_green_at(90));
        assert!(!merged.is_green_at(110));
        assert_eq!(merged.green_secs(), 100);
    }

    #[test]
    fn unsignalised_link_always_discharges() {
        let signals = Signals::default();
        let link = Id::create("no-signal-here");
        assert!(signals.allows_departure(&link, 0));
        assert!(signals.allows_departure(&link, 7_999));
        assert!(!signals.is_signalised(&link));
    }

    #[test]
    #[should_panic(expected = "signal cycle must be > 0")]
    fn zero_cycle_is_rejected() {
        SignalWindows::from_group(0, 0, 10);
    }

    // --- parsing the three MATSim DTDs ---

    const SYSTEMS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<signalSystems>
    <signalSystem id="1">
        <signals>
            <signal linkIdRef="a1" id="1"/>
            <signal linkIdRef="a2" id="2"/>
        </signals>
    </signalSystem>
</signalSystems>"#;

    const GROUPS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<signalGroups>
    <signalSystem refId="1">
        <signalGroup id="2">
            <signal refId="1"/>
        </signalGroup>
        <signalGroup id="3">
            <signal refId="2"/>
        </signalGroup>
    </signalSystem>
</signalGroups>"#;

    /// Writes `body` to a `.xml.gz` file under a fresh temp dir and returns its path.
    fn gz_in_temp(body: &str, stem: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(format!("{stem}.xml.gz"));
        let file = std::fs::File::create(&path).expect("create signal file");
        let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        encoder.write_all(body.as_bytes()).expect("write");
        encoder.finish().expect("finish");
        (dir, path)
    }

    /// Parses the three files, returning the temp dirs that own them alongside the result.
    ///
    /// The approach-link IDs must already exist in the global store, because a real run
    /// loads the network before the signal files and `get_from_ext` resolves against
    /// links the network already registered. The callers below create them first for
    /// that reason.
    fn parse_bangkok_style(
        control: &str,
    ) -> (
        Signals,
        tempfile::TempDir,
        tempfile::TempDir,
        tempfile::TempDir,
    ) {
        let (sd, systems) = gz_in_temp(SYSTEMS, "output_signal_systems_v2.0");
        let (gd, groups) = gz_in_temp(GROUPS, "output_signal_groups_v2.0");
        let (cd, control_path) = gz_in_temp(control, "output_signal_control_v2.0");
        let files = SignalFiles {
            systems: Some(systems),
            groups: Some(groups),
            control: Some(control_path),
        };
        let signals = Signals::from_files(&files).expect("parse signals");
        (signals, sd, gd, cd)
    }

    /// Registers the approach links the fixture files name, mirroring a real run's
    /// network load.
    fn create_fixture_links() -> (Id<Link>, Id<Link>) {
        (Id::create("a1"), Id::create("a2"))
    }

    #[deterministic_id_test]
    fn parses_three_files_into_per_link_green_windows() {
        let control = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<signalControl>
    <signalSystem refId="1">
        <signalSystemController>
            <controllerIdentifier>DefaultPlanbasedSignalSystemController</controllerIdentifier>
            <signalPlan id="1">
                <cycleTime sec="180"/>
                <offset sec="0"/>
                <signalGroupSettings refId="2">
                    <onset sec="113"/>
                    <dropping sec="174"/>
                </signalGroupSettings>
                <signalGroupSettings refId="3">
                    <onset sec="0"/>
                    <dropping sec="10"/>
                </signalGroupSettings>
            </signalPlan>
        </signalSystemController>
    </signalSystem>
</signalControl>"#;
        let (a1, a2) = create_fixture_links();
        let (signals, _sd, _gd, _cd) = parse_bangkok_style(control);

        assert_eq!(signals.signalised_links(), 2);
        assert_eq!(signals.cycle_secs(), 180);
        // Group 2 governs a1 over [113, 174).
        assert!(!signals.allows_departure(&a1, 112));
        assert!(signals.allows_departure(&a1, 113));
        assert!(!signals.allows_departure(&a1, 174));
        // Group 3 governs a2 over [0, 10).
        assert!(signals.allows_departure(&a2, 0));
        assert!(!signals.allows_departure(&a2, 10));
        // An unlisted link is unsignalised.
        assert!(signals.allows_departure(&Id::create("nowhere"), 100));
    }

    #[deterministic_id_test]
    fn offset_shifts_the_whole_plan() {
        let control = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<signalControl>
    <signalSystem refId="1">
        <signalSystemController>
            <signalPlan id="1">
                <cycleTime sec="180"/>
                <offset sec="30"/>
                <signalGroupSettings refId="2">
                    <onset sec="113"/>
                    <dropping sec="174"/>
                </signalGroupSettings>
            </signalPlan>
        </signalSystemController>
    </signalSystem>
</signalControl>"#;
        let (a1, _a2) = create_fixture_links();
        let (signals, _sd, _gd, _cd) = parse_bangkok_style(control);
        // Unshifted the group would be green at 113; with offset 30 it is green at 143.
        assert!(!signals.allows_departure(&a1, 113));
        assert!(signals.allows_departure(&a1, 143));
    }

    #[deterministic_id_test]
    fn a_group_governing_several_signals_greens_all_of_them() {
        let control = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<signalControl>
    <signalSystem refId="1">
        <signalSystemController>
            <signalPlan id="1">
                <cycleTime sec="180"/>
                <signalGroupSettings refId="2">
                    <onset sec="100"/>
                    <dropping sec="120"/>
                </signalGroupSettings>
                <signalGroupSettings refId="3">
                    <onset sec="130"/>
                    <dropping sec="150"/>
                </signalGroupSettings>
            </signalPlan>
        </signalSystemController>
    </signalSystem>
</signalControl>"#;
        // Collapse the two groups onto one signal so a1 receives both windows.
        let groups = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<signalGroups>
    <signalSystem refId="1">
        <signalGroup id="2"><signal refId="1"/></signalGroup>
        <signalGroup id="3"><signal refId="1"/></signalGroup>
    </signalSystem>
</signalGroups>"#;
        let (a1, _a2) = create_fixture_links();
        let (sd, systems) = gz_in_temp(SYSTEMS, "output_signal_systems_v2.0");
        let (gd, groups_path) = gz_in_temp(groups, "output_signal_groups_v2.0");
        let (cd, control_path) = gz_in_temp(control, "output_signal_control_v2.0");
        let signals = Signals::from_files(&SignalFiles {
            systems: Some(systems),
            groups: Some(groups_path),
            control: Some(control_path),
        })
        .expect("parse");
        drop((sd, gd, cd));

        assert!(signals.allows_departure(&a1, 100));
        assert!(
            !signals.allows_departure(&a1, 125),
            "gap between the two windows"
        );
        assert!(signals.allows_departure(&a1, 140));
        // Both groups name signal 1, so only a1 becomes signalised in this fixture.
        assert_eq!(signals.signalised_links(), 1);
        // A link no timed group governs stays unsignalised and always discharges.
        assert!(signals.allows_departure(&Id::create("a2"), 125));
    }

    #[deterministic_id_test]
    fn incomplete_file_set_is_rejected() {
        let (_sd, systems) = gz_in_temp(SYSTEMS, "output_signal_systems_v2.0");
        let err = Signals::from_files(&SignalFiles {
            systems: Some(systems),
            groups: None,
            control: None,
        })
        .expect_err("an incomplete triple must not parse");
        assert!(err.contains("all three"), "got: {err}");
    }

    #[test]
    fn completeness_is_reported_before_any_parsing() {
        let empty = SignalFiles::default();
        assert!(!empty.is_complete());
        assert!(!empty.any_present());
        let partial = SignalFiles {
            systems: Some("s.xml.gz".into()),
            ..Default::default()
        };
        assert!(!partial.is_complete());
        assert!(
            partial.any_present(),
            "a partial set must be visible as present"
        );
    }
}
