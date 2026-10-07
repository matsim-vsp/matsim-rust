use crate::simulation::events::{
    EventsManager, LinkEnterEvent, LinkLeaveEvent, VehicleEntersTrafficEvent,
    VehicleLeavesTrafficEvent,
};
use crate::simulation::framework_events::{
    MobsimEvent, MobsimEventsManager, PartitionEvent, PartitionEventsManager,
    VehicleEntersPartitionEvent, VehicleLeavesPartitionEvent,
};
use crate::simulation::id::Id;
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::SimTime;
use arc_swap::ArcSwap;
use nohash_hasher::IntMap;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug)]
struct ActiveLinkEnter {
    link: Id<Link>,
    time: SimTime,
}

#[derive(Clone, Debug, Default)]
struct TravelTimeBin {
    mean_nanos: f64,
    count: u64,
}

#[derive(Debug)]
struct TravelTimeData {
    travel_time_bins: Vec<TravelTimeBin>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TravelTimeGetter {
    Average,
    LinearInterpolation,
}

impl TravelTimeData {
    fn new(num_bins: usize) -> Self {
        TravelTimeData {
            travel_time_bins: vec![TravelTimeBin::default(); num_bins],
        }
    }

    fn observe(&mut self, slot: usize, travel_time: Duration) {
        let bin = &mut self.travel_time_bins[slot];
        let next_count = bin.count + 1;
        bin.mean_nanos = ((bin.mean_nanos * bin.count as f64) + duration_to_nanos_f64(travel_time))
            / next_count as f64;
        bin.count = next_count;
    }

    fn build_consolidated_travel_times(&self, link: &Link, bin_size: Duration) -> Vec<Duration> {
        let freespeed_travel_time = travel_time_from_speed(link.length, link.freespeed);
        let mut result = vec![freespeed_travel_time; self.travel_time_bins.len()];

        for (i, bin) in self.travel_time_bins.iter().enumerate() {
            if bin.count > 0 {
                result[i] = Duration::from_secs_f64(bin.mean_nanos / 1_000_000_000.0);
            }
        }

        // MATSim does not let an empty bin immediately fall back to freespeed after a very slow
        // observed bin. Instead, each following bin may drop by at most one bin size. For example,
        // with 900s bins and a 3000s observation in bin 0, empty later bins become 2100s, 1200s,
        // 300s, and only then freespeed again.
        for i in 1..result.len() {
            let lower_bound = result[i - 1].saturating_sub(bin_size);
            if result[i] < lower_bound {
                result[i] = lower_bound;
            }
        }

        result
    }
}

pub(crate) type PartitionTravelTimes = IntMap<Id<String>, IntMap<Id<Link>, Vec<Duration>>>;

#[derive(Debug)]
pub(crate) struct PartitionTravelTimeCollector {
    bin_size: Duration,
    num_bins: usize,
    vehicle_modes: IntMap<Id<InternalVehicle>, Id<String>>,
    active_link_enters_by_vehicle: IntMap<Id<InternalVehicle>, ActiveLinkEnter>,
    travel_time_data_by_mode: IntMap<Id<String>, IntMap<Id<Link>, TravelTimeData>>,
}

impl PartitionTravelTimeCollector {
    pub fn new(bin_size: Duration, max_time: Duration) -> Self {
        assert!(
            bin_size > Duration::ZERO,
            "travel time bin size must be greater than zero"
        );
        Self {
            bin_size,
            num_bins: number_of_bins(bin_size, max_time),
            vehicle_modes: IntMap::default(),
            active_link_enters_by_vehicle: IntMap::default(),
            travel_time_data_by_mode: IntMap::default(),
        }
    }

    pub fn process_vehicle_enters_traffic_event(&mut self, event: &VehicleEntersTrafficEvent) {
        self.assign_vehicle_mode(&event.vehicle, &event.network_mode);
    }

    pub fn process_vehicle_enters_partition_event(&mut self, event: &VehicleEntersPartitionEvent) {
        self.assign_vehicle_mode(&event.vehicle_id, &event.network_mode);
    }

    pub fn process_link_enter_event(&mut self, event: &LinkEnterEvent) {
        assert!(
            self.vehicle_modes.contains_key(&event.vehicle),
            "LinkEnter for vehicle {} has no network-mode assignment in the partition travel-time collector",
            event.vehicle.external()
        );
        self.active_link_enters_by_vehicle.insert(
            event.vehicle.clone(),
            ActiveLinkEnter {
                link: event.link.clone(),
                time: event.time,
            },
        );
    }

    pub fn process_link_leave_event(&mut self, event: &LinkLeaveEvent) {
        let Some(mode) = self.vehicle_modes.get(&event.vehicle).cloned() else {
            // Without a known vehicle there cannot be a locally recorded link entry.
            return;
        };
        let Some(active_enter) = self.active_link_enters_by_vehicle.remove(&event.vehicle) else {
            // Return if vehicle didn't enter link before, i.e., this is the first link leave after activity.
            return;
        };
        let travel_time = event.time.duration_since(active_enter.time);
        let slot = time_slot(active_enter.time, self.bin_size, self.num_bins);
        self.travel_time_data_by_mode
            .entry(mode)
            .or_default()
            .entry(active_enter.link)
            .or_insert_with(|| TravelTimeData::new(self.num_bins))
            .observe(slot, travel_time);
    }

    pub fn process_vehicle_leaves_traffic_event(&mut self, event: &VehicleLeavesTrafficEvent) {
        self.clear_vehicle_state(&event.vehicle);
    }

    pub fn process_vehicle_leaves_partition_event(&mut self, event: &VehicleLeavesPartitionEvent) {
        self.clear_vehicle_state(&event.vehicle_id);
    }

    /// Clears every piece of iteration-local state.
    pub fn reset(&mut self) {
        self.vehicle_modes.clear();
        self.active_link_enters_by_vehicle.clear();
        self.travel_time_data_by_mode.clear();
    }

    pub(crate) fn finish(&mut self, network: &Network) -> PartitionTravelTimes {
        let mut modes = IntMap::default();
        for (mode, data_by_link) in std::mem::take(&mut self.travel_time_data_by_mode) {
            let links = data_by_link
                .into_iter()
                .map(|(link_id, data)| {
                    let link = network.get_link(&link_id);
                    (
                        link_id,
                        data.build_consolidated_travel_times(link, self.bin_size),
                    )
                })
                .collect();
            modes.insert(mode, links);
        }
        modes
    }

    pub(crate) fn register_events(calculator: &Rc<RefCell<Self>>, events: &mut EventsManager) {
        let enters_traffic = calculator.clone();
        events.on::<VehicleEntersTrafficEvent, _>(move |event| {
            enters_traffic
                .borrow_mut()
                .process_vehicle_enters_traffic_event(event);
        });

        let link_enters = calculator.clone();
        events.on::<LinkEnterEvent, _>(move |event| {
            link_enters.borrow_mut().process_link_enter_event(event);
        });

        let link_leaves = calculator.clone();
        events.on::<LinkLeaveEvent, _>(move |event| {
            link_leaves.borrow_mut().process_link_leave_event(event);
        });

        let leaves_traffic = calculator.clone();
        events.on::<VehicleLeavesTrafficEvent, _>(move |event| {
            leaves_traffic
                .borrow_mut()
                .process_vehicle_leaves_traffic_event(event);
        });

        let reset = calculator.clone();
        events.on_reset_iteration(move |_| reset.borrow_mut().reset());
    }

    pub(crate) fn register_partition_events(
        calculator: &Rc<RefCell<Self>>,
        events: &mut PartitionEventsManager,
    ) {
        let calculator = calculator.clone();
        events.on_event(move |event| {
            let mut calculator = calculator.borrow_mut();
            match &event.payload {
                PartitionEvent::VehicleEntersPartition(event) => {
                    calculator.process_vehicle_enters_partition_event(event);
                }
                PartitionEvent::VehicleLeavesPartition(event) => {
                    calculator.process_vehicle_leaves_partition_event(event);
                }
                PartitionEvent::AgentEntersPartition(_)
                | PartitionEvent::AgentLeavesPartition(_) => {}
            }
        });
    }

    pub(crate) fn register_travel_time_publication(
        collector: &Rc<RefCell<PartitionTravelTimeCollector>>,
        global: Arc<GlobalTravelTimeCalculator>,
        network: Arc<Network>,
        rank: u32,
        events: &mut MobsimEventsManager,
    ) {
        let collector = collector.clone();
        events.on_event(move |event| {
            if matches!(&event.payload, MobsimEvent::BeforeCleanup) {
                let times = collector.borrow_mut().finish(&network);
                global.submit(event.meta.iteration, rank, times);
            }
        });
    }

    fn assign_vehicle_mode(&mut self, vehicle: &Id<InternalVehicle>, mode: &Id<String>) {
        if let Some(previous_mode) = self.vehicle_modes.insert(vehicle.clone(), mode.clone()) {
            if previous_mode != *mode {
                panic!(
                    "Vehicle {} changed mode from {} to {}",
                    vehicle, previous_mode, mode
                )
            }
        }
    }

    fn clear_vehicle_state(&mut self, vehicle: &Id<InternalVehicle>) {
        self.vehicle_modes.remove(vehicle);
        self.active_link_enters_by_vehicle.remove(vehicle);
    }
}

#[derive(Debug, Default)]
struct TravelTimeSnapshot {
    bin_size: Duration,
    num_bins: usize,
    // Only links with observations need consolidated bins. Others use freespeed at lookup.
    times_by_partition: Vec<PartitionTravelTimes>,
}

#[derive(Debug, Default)]
struct PendingTravelTimes {
    iteration: Option<u32>,
    last_published_iteration: Option<u32>,
    parts: Vec<Option<PartitionTravelTimes>>,
    received: usize,
}

#[derive(Debug)]
pub struct GlobalTravelTimeCalculator {
    // Serializes concurrent submissions, once per partition and iteration.
    pending: Mutex<PendingTravelTimes>,
    // Routing reads the published snapshot without taking the staging lock.
    snapshot: ArcSwap<TravelTimeSnapshot>,
}

impl GlobalTravelTimeCalculator {
    pub fn new(num_parts: usize, bin_size: Duration, max_time: Duration) -> Self {
        assert!(num_parts > 0, "travel times require at least one partition");
        assert!(
            bin_size > Duration::ZERO,
            "travel time bin size must be greater than zero"
        );
        let num_bins = number_of_bins(bin_size, max_time);
        Self {
            pending: Mutex::new(PendingTravelTimes {
                iteration: None,
                last_published_iteration: None,
                parts: (0..num_parts).map(|_| None).collect(),
                received: 0,
            }),
            snapshot: ArcSwap::from_pointee(TravelTimeSnapshot {
                bin_size,
                num_bins,
                times_by_partition: vec![IntMap::default(); num_parts],
            }),
        }
    }

    /// Lets worker publish their full snapshot. Checks if all parts are received. If this is the case, swap the snapshots.
    pub(crate) fn submit(&self, iteration: u32, rank: u32, times: PartitionTravelTimes) {
        let mut pending = self
            .pending
            .lock()
            .expect("travel-time submission lock poisoned");
        let rank = rank as usize;
        assert!(
            rank < pending.parts.len(),
            "travel-time rank {rank} is out of range"
        );
        assert!(
            pending
                .last_published_iteration
                .is_none_or(|last| iteration > last),
            "travel times for iteration {iteration} were already published"
        );
        if let Some(active) = pending.iteration {
            assert_eq!(
                active, iteration,
                "travel-time submissions from different iterations overlap"
            );
        } else {
            pending.iteration = Some(iteration);
        }
        assert!(
            pending.parts[rank].is_none(),
            "duplicate travel-time submission for rank {rank} in iteration {iteration}"
        );
        pending.parts[rank] = Some(times);
        pending.received += 1;
        if pending.received == pending.parts.len() {
            let times_by_partition = pending
                .parts
                .iter_mut()
                .map(|part| part.take().unwrap())
                .collect();
            let current = self.snapshot.load();
            self.snapshot.store(Arc::new(TravelTimeSnapshot {
                bin_size: current.bin_size,
                num_bins: current.num_bins,
                times_by_partition,
            }));
            pending.iteration = None;
            pending.last_published_iteration = Some(iteration);
            pending.received = 0;
        }
    }

    pub fn get_link_travel_time(
        &self,
        mode: &Id<String>,
        link: &Link,
        now: SimTime,
        vehicle: Option<&InternalVehicle>,
        getter: TravelTimeGetter,
    ) -> Duration {
        let snapshot = self.snapshot.load();
        let observed = match snapshot.times_by_partition[link.partition as usize]
            .get(mode)
            .and_then(|links| links.get(&link.id))
        {
            Some(times) => {
                let slot = time_slot(now, snapshot.bin_size, snapshot.num_bins);
                match getter {
                    TravelTimeGetter::Average => times[slot],
                    TravelTimeGetter::LinearInterpolation => {
                        interpolated_travel_time(times, now, snapshot.bin_size)
                    }
                }
            }
            None => travel_time_from_speed(link.length, link.freespeed),
        };

        if let Some(vehicle) = vehicle {
            if vehicle.max_v.is_finite() && vehicle.max_v > 0.0 {
                return observed.max(travel_time_from_speed(link.length, vehicle.max_v));
            }
        }
        observed
    }
}

fn number_of_bins(bin_size: Duration, max_time: Duration) -> usize {
    ((max_time.as_nanos() / bin_size.as_nanos()) + 1)
        .try_into()
        .expect("number of travel time bins does not fit into usize")
}

fn time_slot(time: SimTime, bin_size: Duration, num_bins: usize) -> usize {
    let slot = time.as_duration().as_nanos() / bin_size.as_nanos();
    let slot = usize::try_from(slot).unwrap_or(usize::MAX);
    slot.min(num_bins - 1)
}

fn duration_to_nanos_f64(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000_000_000.0
}

fn travel_time_from_speed(length: f64, speed: f64) -> Duration {
    assert!(
        speed.is_finite() && speed > 0.0,
        "speed must be finite and greater than zero"
    );
    Duration::from_secs_f64(length / speed)
}

fn interpolated_travel_time(
    travel_times: &[Duration],
    now: SimTime,
    bin_size: Duration,
) -> Duration {
    if travel_times.len() == 1 {
        return travel_times[0];
    }

    let bin_size_nanos = duration_to_nanos_f64(bin_size);
    let now_nanos = duration_to_nanos_f64(now.as_duration());
    let first_center = bin_size_nanos * 0.5;
    let last_center = (travel_times.len() as f64 - 0.5) * bin_size_nanos;

    if now_nanos <= first_center {
        return travel_times[0];
    }
    if now_nanos >= last_center {
        return *travel_times.last().unwrap();
    }

    let lower = ((now_nanos / bin_size_nanos) - 0.5).floor() as usize;
    let upper = lower + 1;
    let lower_center = (lower as f64 + 0.5) * bin_size_nanos;
    let fraction = (now_nanos - lower_center) / bin_size_nanos;

    let lower_nanos = duration_to_nanos_f64(travel_times[lower]);
    let upper_nanos = duration_to_nanos_f64(travel_times[upper]);
    Duration::from_secs_f64(
        ((lower_nanos + ((upper_nanos - lower_nanos) * fraction)) / 1_000_000_000.0).max(0.0),
    )
}

#[cfg(test)]
pub(crate) mod test {
    use super::{
        GlobalTravelTimeCalculator, PartitionTravelTimeCollector, PartitionTravelTimes,
        TravelTimeData, TravelTimeGetter,
    };
    use crate::simulation::InternalAttributes;
    use crate::simulation::events::{
        LinkEnterEvent, LinkLeaveEvent, VehicleEntersTrafficEvent, VehicleLeavesTrafficEvent,
    };
    use crate::simulation::framework_events::{MobsimEvent, MobsimEventsManager};
    use crate::simulation::id::Id;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Network, Node};
    use crate::simulation::scenario::vehicles::InternalVehicle;
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use nohash_hasher::IntSet;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::Duration;

    pub(crate) fn link(id: &str, length: f64, freespeed: f64) -> Link {
        Link {
            id: Id::create(id),
            from: Id::<Node>::create(&format!("{id}_from")),
            to: Id::<Node>::create(&format!("{id}_to")),
            length,
            capacity: 3600.0,
            freespeed,
            permlanes: 1.0,
            modes: IntSet::default(),
            partition: 0,
            attributes: InternalAttributes::default(),
        }
    }

    pub(crate) fn network(links: &[Link]) -> Arc<Network> {
        let mut network = Network::new();
        for link in links {
            network.add_node(Node::new(
                link.from.clone(),
                Coordinate::new_2d(0.0, 0.0),
                link.partition,
                1,
            ));
            network.add_node(Node::new(
                link.to.clone(),
                Coordinate::new_2d(1.0, 0.0),
                link.partition,
                1,
            ));
            network.add_link(link.clone());
        }
        Arc::new(network)
    }

    fn collector(bin_size: u64, max_time: u64) -> PartitionTravelTimeCollector {
        PartitionTravelTimeCollector::new(
            Duration::from_secs(bin_size),
            Duration::from_secs(max_time),
        )
    }

    fn global(num_parts: usize, bin_size: u64, max_time: u64) -> GlobalTravelTimeCalculator {
        GlobalTravelTimeCalculator::new(
            num_parts,
            Duration::from_secs(bin_size),
            Duration::from_secs(max_time),
        )
    }

    fn vehicle(id: &str, max_v: f64) -> InternalVehicle {
        InternalVehicle {
            id: Id::create(id),
            max_v,
            pce: 1.0,
            vehicle_type: Id::create("default"),
            attributes: InternalAttributes::default(),
        }
    }

    fn enter_traffic(
        collector: &mut PartitionTravelTimeCollector,
        vehicle: &Id<InternalVehicle>,
        link: &Id<Link>,
        mode: &Id<String>,
    ) {
        collector.process_vehicle_enters_traffic_event(&VehicleEntersTrafficEvent {
            time: SimTime::from_secs(0),
            vehicle: vehicle.clone(),
            link: link.clone(),
            person: Id::create(format!("person-{}", vehicle.external()).as_str()),
            network_mode: mode.clone(),
            relative_position: 1.0,
            attributes: InternalAttributes::default(),
        });
    }

    fn link_enter(
        collector: &mut PartitionTravelTimeCollector,
        vehicle: &Id<InternalVehicle>,
        link: &Id<Link>,
        time: u64,
    ) {
        collector.process_link_enter_event(&LinkEnterEvent {
            time: SimTime::from_secs(time),
            vehicle: vehicle.clone(),
            link: link.clone(),
            attributes: InternalAttributes::default(),
        });
    }

    fn link_leave(
        collector: &mut PartitionTravelTimeCollector,
        vehicle: &Id<InternalVehicle>,
        link: &Id<Link>,
        time: u64,
    ) {
        collector.process_link_leave_event(&LinkLeaveEvent {
            time: SimTime::from_secs(time),
            vehicle: vehicle.clone(),
            link: link.clone(),
            attributes: InternalAttributes::default(),
        });
    }

    fn observe(
        collector: &mut PartitionTravelTimeCollector,
        mode: &Id<String>,
        link: &Id<Link>,
        vehicle: &Id<InternalVehicle>,
        enter: u64,
        leave: u64,
    ) {
        enter_traffic(collector, vehicle, link, mode);
        link_enter(collector, vehicle, link, enter);
        link_leave(collector, vehicle, link, leave);
    }

    fn lookup(
        global: &GlobalTravelTimeCalculator,
        mode: &Id<String>,
        link: &Link,
        now: u64,
    ) -> Duration {
        global.get_link_travel_time(
            mode,
            link,
            SimTime::from_secs(now),
            None,
            TravelTimeGetter::Average,
        )
    }

    fn submit_collector(
        global: &GlobalTravelTimeCalculator,
        iteration: u32,
        rank: u32,
        collector: &mut PartitionTravelTimeCollector,
        network: &Network,
    ) {
        global.submit(iteration, rank, collector.finish(network));
    }

    #[deterministic_id_test]
    fn empty_snapshot_uses_freespeed_and_vehicle_limit() {
        let link = link("empty", 100.0, 20.0);
        let car = Id::create("car");
        let global = global(1, 10, 100);
        assert_eq!(Duration::from_secs(5), lookup(&global, &car, &link, 0));
        assert_eq!(
            Duration::from_secs(10),
            global.get_link_travel_time(
                &car,
                &link,
                SimTime::from_secs(0),
                Some(&vehicle("slow", 10.0)),
                TravelTimeGetter::Average,
            )
        );
    }

    #[deterministic_id_test]
    fn running_mean_uses_enter_slot_and_clamps_late_times() {
        let link = link("observed", 100.0, 100.0);
        let net = network(&[link.clone()]);
        let car = Id::create("car");
        let mut collector = collector(10, 25);
        let global = global(1, 10, 25);

        // No local link enter: the leave must be ignored.
        link_leave(&mut collector, &Id::create("unknown"), &link.id, 5);
        observe(&mut collector, &car, &link.id, &Id::create("v1"), 2, 4); // bin 0, 2s
        observe(&mut collector, &car, &link.id, &Id::create("v2"), 3, 7); // bin 0, 4s
        observe(&mut collector, &car, &link.id, &Id::create("v3"), 99, 104); // las bin, 5s
        submit_collector(&global, 0, 0, &mut collector, &net);

        // Bin 0 averages 2s and 4s; bin 1 uses freespeed; late times use the last bin's 5s.
        assert_eq!(Duration::from_secs(3), lookup(&global, &car, &link, 0));
        assert_eq!(Duration::from_secs(1), lookup(&global, &car, &link, 10));
        assert_eq!(Duration::from_secs(5), lookup(&global, &car, &link, 20));
        assert_eq!(Duration::from_secs(5), lookup(&global, &car, &link, 999));
    }

    // Partitions and modes stay separate; missing observations use freespeed.
    #[deterministic_id_test]
    fn modes_partitions_and_unobserved_links_stay_separate() {
        let first = link("first", 100.0, 100.0);
        let mut second = link("second", 100.0, 100.0);
        second.partition = 1;
        let missing = link("missing", 100.0, 10.0);
        let net = network(&[first.clone(), second.clone(), missing.clone()]);
        let car = Id::create("car");
        let walk = Id::create("walk");
        let mut part_0 = collector(10, 100);
        let mut part_1 = collector(10, 100);
        observe(&mut part_0, &car, &first.id, &Id::create("car-0"), 0, 4);
        observe(&mut part_0, &walk, &first.id, &Id::create("walk-0"), 0, 9);
        observe(&mut part_1, &car, &second.id, &Id::create("car-1"), 0, 7);
        let global = global(2, 10, 100);

        // Arrival order is irrelevant. Nothing becomes visible before the last partition.
        submit_collector(&global, 0, 1, &mut part_1, &net);
        assert_eq!(Duration::from_secs(1), lookup(&global, &car, &second, 0));
        submit_collector(&global, 0, 0, &mut part_0, &net);
        assert_eq!(Duration::from_secs(4), lookup(&global, &car, &first, 0));
        assert_eq!(Duration::from_secs(9), lookup(&global, &walk, &first, 0));
        assert_eq!(Duration::from_secs(7), lookup(&global, &car, &second, 0));
        assert_eq!(Duration::from_secs(1), lookup(&global, &walk, &second, 0));
        assert_eq!(Duration::from_secs(10), lookup(&global, &car, &missing, 0));
    }

    #[deterministic_id_test]
    fn consolidation_cascades_previous_bin_minus_bin_size() {
        let link = link("slow", 100.0, 100.0);
        let net = network(&[link.clone()]);
        let car = Id::create("car");
        let mut collector = collector(900, 3600);
        observe(&mut collector, &car, &link.id, &Id::create("v1"), 0, 3000);
        let global = global(1, 900, 3600);
        submit_collector(&global, 0, 0, &mut collector, &net);

        for (time, expected) in [(0, 3000), (900, 2100), (1800, 1200), (2700, 300), (3600, 1)] {
            assert_eq!(
                Duration::from_secs(expected),
                lookup(&global, &car, &link, time)
            );
        }
    }

    #[deterministic_id_test]
    fn interpolation_and_vehicle_max_speed_are_preserved() {
        let link = link("interpolated", 100.0, 100.0);
        let net = network(&[link.clone()]);
        let car = Id::create("car");
        let mut collector = collector(10, 100);
        observe(&mut collector, &car, &link.id, &Id::create("first"), 0, 10);
        observe(
            &mut collector,
            &car,
            &link.id,
            &Id::create("second"),
            10,
            30,
        );
        let global = global(1, 10, 100);
        submit_collector(&global, 0, 0, &mut collector, &net);
        assert_eq!(
            Duration::from_secs(15),
            global.get_link_travel_time(
                &car,
                &link,
                SimTime::from_secs(10),
                None,
                TravelTimeGetter::LinearInterpolation,
            )
        );
        assert_eq!(
            Duration::from_secs(25),
            global.get_link_travel_time(
                &car,
                &link,
                SimTime::from_secs(10),
                Some(&vehicle("slow", 4.0)),
                TravelTimeGetter::LinearInterpolation,
            )
        );
    }

    #[deterministic_id_test]
    fn snapshot_changes_only_after_complete_iteration() {
        let link = link("iterated", 100.0, 10.0);
        let mut second = self::link("second", 100.0, 10.0);
        second.partition = 1;
        let net = network(&[link.clone(), second]);
        let car = Id::create("car");
        let global = Arc::new(global(2, 10, 100));
        let reader = global.clone();
        assert!(Arc::ptr_eq(&global, &reader));
        let mut first = collector(10, 100);

        // Observe 20s travel time
        observe(&mut first, &car, &link.id, &Id::create("v1"), 0, 20);
        submit_collector(&global, 0, 0, &mut first, &net);
        // Until the second partition submits, the initial freespeed snapshot remains visible.
        assert_eq!(Duration::from_secs(10), lookup(&reader, &car, &link, 0));
        global.submit(0, 1, PartitionTravelTimes::default());
        // Both partitions have submitted, thus the new travel time snapshot is available.
        assert_eq!(Duration::from_secs(20), lookup(&reader, &car, &link, 0));
        first.reset();

        // Observe 30s travel time
        observe(&mut first, &car, &link.id, &Id::create("v2"), 0, 30);
        global.submit(1, 1, PartitionTravelTimes::default());
        assert_eq!(Duration::from_secs(20), lookup(&reader, &car, &link, 0));
        submit_collector(&global, 1, 0, &mut first, &net);
        assert_eq!(Duration::from_secs(30), lookup(&reader, &car, &link, 0));

        // Don't observe any travel time
        global.submit(2, 0, PartitionTravelTimes::default());
        assert_eq!(Duration::from_secs(30), lookup(&reader, &car, &link, 0));
        global.submit(2, 1, PartitionTravelTimes::default());
        assert_eq!(Duration::from_secs(10), lookup(&reader, &car, &link, 0));
    }

    #[deterministic_id_test]
    fn before_cleanup_publishes_each_iteration_after_all_partitions_finish() {
        let first = link("first-finished", 100.0, 100.0);
        let mut second = link("second-finished", 100.0, 100.0);
        second.partition = 1;
        let net = network(&[first.clone(), second.clone()]);
        let car = Id::create("car");
        let global = Arc::new(global(2, 10, 100));
        let first_collector = Rc::new(RefCell::new(collector(10, 100)));
        let second_collector = Rc::new(RefCell::new(collector(10, 100)));
        let mut first_events = MobsimEventsManager::for_partition(0, 7);
        let mut second_events = MobsimEventsManager::for_partition(1, 7);
        PartitionTravelTimeCollector::register_travel_time_publication(
            &first_collector,
            global.clone(),
            net.clone(),
            0,
            &mut first_events,
        );
        PartitionTravelTimeCollector::register_travel_time_publication(
            &second_collector,
            global.clone(),
            net.clone(),
            1,
            &mut second_events,
        );

        observe(
            &mut first_collector.borrow_mut(),
            &car,
            &first.id,
            &Id::create("first-vehicle"),
            0,
            5,
        );
        observe(
            &mut second_collector.borrow_mut(),
            &car,
            &second.id,
            &Id::create("second-vehicle"),
            0,
            7,
        );
        first_events.process_event(MobsimEvent::BeforeCleanup);
        assert_eq!(Duration::from_secs(1), lookup(&global, &car, &first, 0));
        second_events.process_event(MobsimEvent::BeforeCleanup);
        assert_eq!(Duration::from_secs(5), lookup(&global, &car, &first, 0));
        assert_eq!(Duration::from_secs(7), lookup(&global, &car, &second, 0));

        first_collector.borrow_mut().reset();
        second_collector.borrow_mut().reset();
        first_events.reset_iteration(8);
        second_events.reset_iteration(8);
        observe(
            &mut first_collector.borrow_mut(),
            &car,
            &first.id,
            &Id::create("next-vehicle"),
            0,
            9,
        );
        second_events.process_event(MobsimEvent::BeforeCleanup);
        assert_eq!(Duration::from_secs(5), lookup(&global, &car, &first, 0));
        first_events.process_event(MobsimEvent::BeforeCleanup);
        assert_eq!(Duration::from_secs(9), lookup(&global, &car, &first, 0));
        assert_eq!(Duration::from_secs(1), lookup(&global, &car, &second, 0));
    }

    #[deterministic_id_test]
    fn leaving_traffic_discards_active_link_enter() {
        let link = link("left", 100.0, 100.0);
        let net = network(&[link.clone()]);
        let car = Id::create("car");
        let vehicle = vehicle("v1", 20.0);
        let mut collector = collector(10, 100);
        enter_traffic(&mut collector, &vehicle.id, &link.id, &car);
        link_enter(&mut collector, &vehicle.id, &link.id, 0);
        collector.process_vehicle_leaves_traffic_event(&VehicleLeavesTrafficEvent {
            time: SimTime::from_secs(2),
            vehicle: vehicle.id.clone(),
            link: link.id.clone(),
            person: Id::create("p1"),
            network_mode: car.clone(),
            relative_position: 1.0,
            attributes: InternalAttributes::default(),
        });
        link_leave(&mut collector, &vehicle.id, &link.id, 5);
        assert!(collector.finish(&net).is_empty());
    }

    #[deterministic_id_test]
    #[should_panic]
    fn changing_mode_discards_active_link_enter() {
        let link = link("mode-change", 100.0, 100.0);
        let car = Id::create("car");
        let walk = Id::create("walk");
        let vehicle = Id::create("vehicle");
        let mut collector = collector(10, 100);
        enter_traffic(&mut collector, &vehicle, &link.id, &car);
        link_enter(&mut collector, &vehicle, &link.id, 0);

        // vehicle changes mode while on the link, which should panic
        enter_traffic(&mut collector, &vehicle, &link.id, &walk);
    }

    #[deterministic_id_test]
    fn data_running_mean_and_empty_bins_use_freespeed() {
        let link = link("data", 100.0, 10.0);
        let mut data = TravelTimeData::new(3);
        data.observe(0, Duration::from_secs(2));
        data.observe(0, Duration::from_secs(4));
        let bins = data.build_consolidated_travel_times(&link, Duration::from_secs(10));
        assert_eq!(Duration::from_secs(3), bins[0]);
        assert_eq!(Duration::from_secs(10), bins[1]);
    }

    #[deterministic_id_test]
    #[should_panic(expected = "has no network-mode assignment")]
    fn link_enter_without_mode_reports_clear_error() {
        let link = link("unknown", 100.0, 100.0);
        link_enter(
            &mut collector(10, 100),
            &Id::create("unknown-vehicle"),
            &link.id,
            0,
        );
    }

    #[test]
    #[should_panic(expected = "duplicate travel-time submission")]
    fn rejects_duplicate_rank() {
        let global = global(2, 10, 100);
        global.submit(0, 0, PartitionTravelTimes::default());
        global.submit(0, 0, PartitionTravelTimes::default());
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn rejects_invalid_rank() {
        global(1, 10, 100).submit(0, 1, PartitionTravelTimes::default());
    }

    #[test]
    #[should_panic(expected = "different iterations overlap")]
    fn rejects_overlapping_iterations() {
        let global = global(2, 10, 100);
        global.submit(0, 0, PartitionTravelTimes::default());
        global.submit(1, 1, PartitionTravelTimes::default());
    }

    #[test]
    #[should_panic(expected = "already published")]
    fn rejects_stale_iteration() {
        let global = global(1, 10, 100);
        global.submit(1, 0, PartitionTravelTimes::default());
        global.submit(0, 0, PartitionTravelTimes::default());
    }

    #[test]
    #[should_panic(expected = "already published")]
    fn rejects_repeated_published_iteration() {
        let global = global(1, 10, 100);
        global.submit(0, 0, PartitionTravelTimes::default());
        global.submit(0, 0, PartitionTravelTimes::default());
    }
}
