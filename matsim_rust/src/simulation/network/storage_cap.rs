use crate::simulation::InternalAttributes;
use crate::simulation::config;
use crate::simulation::config::TrafficDynamics;
use crate::simulation::id::Id;
use crate::simulation::scenario::network::{Link, Network};
use nohash_hasher::IntMap;
use crate::simulation::time::{SimClock, Tick};
use std::collections::VecDeque;

use super::STORAGE_CAPACITY_USED_IN_QSIM;

/// Backward wave speed, independent of sampling (15 km/h).
pub(super) const GAP_SPEED: f64 = 15.0 / 3.6;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StorageCapacityDefinition {
    qsim_override: Option<f64>,
    original: f64,
    gap_travel_ticks: Option<Tick>,
    inflow_per_tick: Option<f64>,
}

impl StorageCapacityDefinition {
    #[cfg(test)]
    pub(crate) fn build(
        length: f64,
        perm_lanes: f64,
        capacity_h: f64,
        sample_size: f64,
        effective_cell_size: f64,
        free_speed: f64,
    ) -> Self {
        Self::build_with_dynamics(length, perm_lanes, capacity_h, sample_size,
            effective_cell_size, free_speed, &config::QSim::default())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_with_dynamics(
        length: f64,
        perm_lanes: f64,
        capacity_h: f64,
        sample_size: f64,
        effective_cell_size: f64,
        free_speed: f64,
        config: &config::QSim,
    ) -> Self {
        let flow_cap_s = capacity_h * sample_size / 3600.;
        let cap = length * perm_lanes * sample_size / effective_cell_size;

        // set storage capacity to at least flow capacity. Otherwise, the flow capacity would never be fully used during `move_node`.
        let max_storage_cap = flow_cap_s.max(cap);

        let freespeed_travel_time = length / free_speed;
        let kinematic = config.traffic_dynamics == TrafficDynamics::KinematicWaves;
        let gap_travel_time = if kinematic { length / GAP_SPEED } else { 0.0 };
        let temp_storage_cap = (freespeed_travel_time + gap_travel_time) * flow_cap_s;

        // set storage capacity to at least the freespeed travel time * flow capacity. Otherwise, the flow capacity would never be fully used,
        // because vehicles would need at least need freespeed travel time to reach the end of the link.
        // With kinematic waves, include the time until the gap reaches the upstream end as well.
        let adjusted = (max_storage_cap < temp_storage_cap).then_some(temp_storage_cap);

        Self {
            qsim_override: adjusted,
            original: max_storage_cap,
            // A release in the link phase is usable in a later node phase. Rounding upwards
            // prevents early release; a minimum of one tick also covers zero-length links.
            gap_travel_ticks: kinematic.then(|| SimClock::new(config.ticks_per_second)
                .secs_to_ticks_ceil(gap_travel_time).max(Tick::new(1))),
            inflow_per_tick: kinematic.then(||
                (perm_lanes / effective_cell_size) / (1.0 / GAP_SPEED + 1.0 / free_speed)
                    * sample_size / f64::from(config.ticks_per_second)),
        }
    }

    pub(crate) fn qsim_override(&self) -> Option<f64> {
        self.qsim_override
    }

    #[allow(dead_code)]
    pub(crate) fn original(&self) -> f64 {
        self.original
    }

    pub(crate) fn max(&self) -> f64 {
        self.qsim_override.unwrap_or(self.original)
    }

    pub(super) fn inflow_per_tick(&self) -> Option<f64> {
        self.inflow_per_tick
    }
}

#[derive(Debug, Clone)]
pub struct LinkStorageCapacities {
    capacities: IntMap<Id<Link>, StorageCapacityDefinition>,
}

impl LinkStorageCapacities {
    pub fn from_network(network: &Network, config: &config::QSim) -> Self {
        let effective_cell_size = network.effective_cell_size();
        let capacities = network
            .links()
            .into_iter()
            .map(|link| {
                let definition = StorageCapacityDefinition::build_with_dynamics(
                    link.length,
                    link.permlanes,
                    link.capacity,
                    config.sample_size,
                    effective_cell_size,
                    link.freespeed,
                    config,
                );
                (link.id.clone(), definition)
            })
            .collect();

        Self { capacities }
    }

    pub(crate) fn get(&self, link_id: &Id<Link>) -> &StorageCapacityDefinition {
        self.capacities
            .get(link_id)
            .unwrap_or_else(|| panic!("No storage-capacity definition found for link {link_id}"))
    }

    pub(crate) fn attribute_overrides(&self) -> IntMap<Id<Link>, InternalAttributes> {
        self.capacities
            .iter()
            .filter_map(|(link_id, definition)| {
                definition.qsim_override().map(|adjusted| {
                    let mut attributes = InternalAttributes::default();
                    attributes.insert(STORAGE_CAPACITY_USED_IN_QSIM, adjusted);
                    (link_id.clone(), attributes)
                })
            })
            .collect()
    }
}

/// Storage capacity consumed at entry. Queue releases immediately in the link phase, after the
/// node phase. Kinematic waves reserve that space until the backward-moving gap arrives.
/// SplitInLinks report newly created gaps, rather than changes caused by later availability
/// queries; see `SimNetworkPartition::move_in_link`.
#[derive(Debug, Clone)]
pub enum StorageCap {
    Queue(QueueStorageCap),
    Kinematic(KinematicStorageCap),
}

#[derive(Debug, Clone)]
pub struct QueueStorageCap {
    max: f64,
    used: f64,
}

#[derive(Debug, Clone)]
pub struct KinematicStorageCap {
    reserved: QueueStorageCap,
    gap_travel_ticks: Tick,
    pending: VecDeque<StorageRelease>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct StorageRelease {
    pub pce: f64,
    pub available_at: Tick,
}

impl StorageCap {
    pub(crate) fn from_definition(definition: &StorageCapacityDefinition) -> Self {
        let reserved = QueueStorageCap {
            max: definition.max(),
            used: 0.0,
        };
        match definition.gap_travel_ticks {
            None => Self::Queue(reserved),
            Some(gap_travel_ticks) => Self::Kinematic(KinematicStorageCap {
                reserved,
                gap_travel_ticks,
                pending: VecDeque::new(),
            }),
        }
    }

    #[cfg(test)]
    pub fn build(
        length: f64,
        perm_lanes: f64,
        capacity_h: f64,
        sample_size: f64,
        effective_cell_size: f64,
        free_speed: f64,
    ) -> Self {
        let definition = StorageCapacityDefinition::build(
            length,
            perm_lanes,
            capacity_h,
            sample_size,
            effective_cell_size,
            free_speed,
        );
        Self::from_definition(&definition)
    }

    pub fn used(&self) -> f64 {
        match self {
            Self::Queue(cap) => cap.used,
            Self::Kinematic(cap) => cap.reserved.used,
        }
    }

    #[cfg(test)]
    pub(crate) fn max(&self) -> f64 {
        match self {
            Self::Queue(cap) => cap.max,
            Self::Kinematic(cap) => cap.reserved.max,
        }
    }

    /// Consumes storage capacity on a link
    ///
    /// This method should be called when a vehicle enters a link.
    ///
    /// # Parameters
    /// * 'value' storage capacity to be consumed
    pub fn consume(&mut self, value: f64) {
        match self {
            Self::Queue(cap) => cap.used += value,
            Self::Kinematic(cap) => cap.reserved.used += value,
        }
    }

    /// Releases storage capacity on a link
    ///
    /// This method should be called when a vehicle leaves a link
    pub fn release(&mut self, value: f64, now: Tick) {
        self.release_at(value, self.release_tick(now));
    }

    pub(super) fn release_tick(&self, now: Tick) -> Tick {
        match self {
            Self::Queue(_) => now,
            Self::Kinematic(cap) => now.saturating_add(cap.gap_travel_ticks),
        }
    }

    /// Apply the sender's absolute arrival tick. Never add another gap travel time on receipt.
    pub(super) fn release_at(&mut self, pce: f64, available_at: Tick) {
        match self {
            Self::Queue(cap) => cap.used -= pce,
            Self::Kinematic(cap) => {
                let release = StorageRelease { pce, available_at };
                // Releases on one link normally arrive in order. Also accept batches received
                // out of order without hiding an earlier gap behind a later one.
                if cap.pending.back().is_none_or(|gap| gap.available_at <= available_at) {
                    cap.pending.push_back(release);
                } else {
                    let index = cap.pending.partition_point(|gap| gap.available_at <= available_at);
                    cap.pending.insert(index, release);
                }
            }
        }
    }

    /// Tests whether there is storage capacity available on the link.
    pub fn is_available(&mut self, now: Tick) -> bool {
        let cap = match self {
            Self::Queue(cap) => cap,
            Self::Kinematic(cap) => {
                while cap.pending.front().is_some_and(|gap| gap.available_at <= now) {
                    cap.reserved.used -= cap.pending.pop_front().unwrap().pce;
                }
                &mut cap.reserved
            }
        };
        cap.max - cap.used > 0.0
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::simulation::config;
    use crate::simulation::id::Id;
    use crate::simulation::network::STORAGE_CAPACITY_USED_IN_QSIM;
    use crate::simulation::network::storage_cap::{
        LinkStorageCapacities, StorageCap, StorageCapacityDefinition,
    };
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Network, Node};
    use macros::deterministic_id_test;

    fn kinematic_definition(length: f64, sample: f64, ticks: u32) -> StorageCapacityDefinition {
        let config = config::QSim {
            traffic_dynamics: TrafficDynamics::KinematicWaves,
            ticks_per_second: ticks,
            ..Default::default()
        };
        StorageCapacityDefinition::build_with_dynamics(length, 1., 3600., sample, 7.5, 10., &config)
    }

    #[test]
    fn kinematic_capacity_includes_gap_time_and_scales_only_capacities() {
        let full = kinematic_definition(100., 1., 1);
        let sampled = kinematic_definition(100., 0.1, 4);
        assert_eq!(Some(34.), full.qsim_override());
        assert!((sampled.qsim_override().unwrap() - 3.4).abs() < 1e-12);
        assert_eq!(Some(Tick::new(24)), full.gap_travel_ticks);
        assert_eq!(Some(Tick::new(96)), sampled.gap_travel_ticks);
        let rate = (1. / 7.5) / (1. / GAP_SPEED + 1. / 10.);
        assert!((full.inflow_per_tick().unwrap() - rate).abs() < 1e-12);
        assert!((sampled.inflow_per_tick().unwrap() - rate * 0.1 / 4.).abs() < 1e-12);
    }

    #[test]
    fn gap_is_released_before_at_and_after_its_arrival() {
        let mut cap = StorageCap::from_definition(&kinematic_definition(100., 1., 1));
        cap.consume(cap.max());
        cap.release(1.5, Tick::new(10));
        assert!(!cap.is_available(Tick::new(33)));
        assert!(cap.is_available(Tick::new(34)));
        assert_eq!(32.5, cap.used());
        assert!(cap.is_available(Tick::new(35)));
        assert_eq!(32.5, cap.used());
    }

    #[test]
    fn several_future_gaps_keep_their_pce_and_absolute_arrival() {
        let mut cap = StorageCap::from_definition(&kinematic_definition(100., 1., 1));
        cap.consume(34.);
        // Model a received batch, including an earlier timestamp arriving after a later one.
        cap.release_at(2.5, Tick::new(50));
        cap.release_at(0.5, Tick::new(30));
        cap.release_at(1.0, Tick::new(50));
        assert!(!cap.is_available(Tick::new(29)));
        assert!(cap.is_available(Tick::new(30)));
        assert_eq!(33.5, cap.used());
        assert!(cap.is_available(Tick::new(49)));
        assert_eq!(33.5, cap.used());
        assert!(cap.is_available(Tick::new(50)));
        assert_eq!(30.0, cap.used());
        assert!(cap.is_available(Tick::new(1_000_000)));
        assert!(cap.is_available(Tick::new(1_000_000)));
        assert_eq!(30.0, cap.used());
    }

    #[test]
    fn gap_duration_rounds_up_including_exact_and_short_links() {
        for ticks in [1, 4, 10] {
            for (length, expected) in [(0., 1), (0.01, 1), (25., 6 * ticks as u64),
                (25.01, 6 * ticks as u64 + 1)] {
                let definition = kinematic_definition(length, 1., ticks);
                assert_eq!(Some(Tick::new(expected)), definition.gap_travel_ticks);
                let mut cap = StorageCap::from_definition(&definition);
                cap.consume(cap.max() + 1.);
                cap.release(2., Tick::new(100));
                assert!(!cap.is_available(Tick::new(100 + expected - 1)));
                assert!(cap.is_available(Tick::new(100 + expected)));
            }
        }
    }

    #[test]
    fn queue_release_is_immediate() {
        let mut cap = StorageCap::build(100., 1., 3600., 1., 7.5, 10.);
        cap.consume(cap.max());
        cap.release(1., Tick::new(5));
        assert!(cap.is_available(Tick::new(5)));
    }

    #[test]
    fn storage_capacity_definition_without_adjustment() {
        let cap = StorageCap::build(100., 3., 1., 0.2, 7.5, 10.);
        assert_eq!(8., cap.max());
    }

    #[test]
    fn storage_capacity_definition_with_freespeed_adjustment() {
        let definition = StorageCapacityDefinition::build(100., 3., 360000., 0.2, 7.5, 10.);
        assert_eq!(Some(200.), definition.qsim_override());
        assert_eq!(20., definition.original());
    }

    #[test]
    fn storage_capacity_definition_does_not_adjust_equal_capacity() {
        let definition = StorageCapacityDefinition::build(100., 3., 360000., 0.2, 7.5, 100.);
        assert_eq!(None, definition.qsim_override());
        assert_eq!(20., definition.original());
    }

    #[deterministic_id_test]
    fn storage_capacity_catalog_maps_adjusted_capacity_to_link_attribute() {
        let mut network = Network::new();
        let from = Node::new(Id::create("from"), Coordinate::default(), 0, 1);
        let to = Node::new(Id::create("to"), Coordinate::default(), 0, 1);
        let link_id = Id::create("link");
        let mut link = Link::new_with_default(link_id.clone(), &from, &to);
        link.length = 100.;
        link.capacity = 360000.;
        link.freespeed = 10.;
        link.permlanes = 3.;
        network.add_node(from);
        network.add_node(to);
        network.add_link(link);

        let mut qsim = config::QSim::default();
        qsim.sample_size = 0.2;
        let capacities = LinkStorageCapacities::from_network(&network, &qsim);

        assert_eq!(Some(200.), capacities.get(&link_id).qsim_override());
        assert_eq!(
            Some(200.),
            capacities.attribute_overrides()[&link_id].get::<f64>(STORAGE_CAPACITY_USED_IN_QSIM)
        );
        assert_eq!(
            None,
            network
                .get_link(&link_id)
                .attributes
                .get::<f64>(STORAGE_CAPACITY_USED_IN_QSIM)
        );
    }
}
