use crate::simulation::Identifiable;
use crate::simulation::agents::SimulationAgentLogic;
use crate::simulation::config;
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::events::{
    VehicleEntersTrafficEventBuilder, VehicleLeavesTrafficEventBuilder,
};
use crate::simulation::id::Id;
use crate::simulation::network::flow_cap::Flowcap;
use crate::simulation::network::storage_cap::{StorageCap, StorageCapacityDefinition};
use crate::simulation::network::stuck_timer::StuckTimer;
use crate::simulation::pt::driver::{StopOutcome, serve_stop};
use crate::simulation::pt::stops::TransitStops;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::network::Node;
use crate::simulation::time::{SimClock, Tick};
use crate::simulation::vehicles::SimulationVehicle;
use std::cmp::Reverse;
use std::collections::VecDeque;
use std::fmt::Debug;
use std::time::Duration;

pub enum LinkPosition {
    QStart,
    Waiting,
}

#[derive(Debug)]
pub(crate) struct CarSpotSpeedSample {
    pub vehicle_id: u64,
    pub link_id: String,
    pub speed_mps: f64,
}

#[derive(Debug)]
pub enum SimLink {
    Local(LocalLink),
    In(SplitInLink),
    Out(SplitOutLink),
}

impl SimLink {
    pub(super) fn append_car_spot_speed_samples(
        &self,
        now: Tick,
        samples: &mut Vec<CarSpotSpeedSample>,
    ) {
        match self {
            SimLink::Local(link) => link.append_car_spot_speed_samples(now, samples),
            SimLink::In(link) => link.local_link.append_car_spot_speed_samples(now, samples),
            SimLink::Out(_) => {}
        }
    }

    pub fn id(&self) -> &Id<Link> {
        match self {
            SimLink::Local(ll) => &ll.id,
            SimLink::In(il) => &il.local_link.id,
            SimLink::Out(ol) => &ol.id,
        }
    }

    pub fn from(&self) -> &Id<Node> {
        match self {
            SimLink::Local(l) => l.from(),
            SimLink::In(l) => l.local_link.from(),
            SimLink::Out(l) => &l.from,
        }
    }

    pub fn to(&self) -> &Id<Node> {
        match self {
            SimLink::Local(l) => l.to(),
            SimLink::In(l) => l.local_link.to(),
            SimLink::Out(_) => {
                panic!("There is no from_id of a split out link.")
            }
        }
    }

    pub fn neighbor_part(&self) -> u32 {
        match self {
            SimLink::Local(_) => {
                panic!("local links don't have information about neighbor partitions")
            }
            SimLink::In(il) => il.from_part,
            SimLink::Out(ol) => ol.to_part,
        }
    }

    pub fn flow_cap(&self) -> f64 {
        match self {
            SimLink::Local(l) => l.flow_cap.capacity_per_tick(),
            SimLink::In(il) => il.local_link.flow_cap.capacity_per_tick(),
            SimLink::Out(_) => {
                panic!("no flow cap for out links")
            }
        }
    }

    pub fn offers_veh(&self) -> Option<&SimulationVehicle> {
        match self {
            SimLink::Local(ll) => ll.offers_veh(),
            SimLink::In(il) => il.local_link.offers_veh(),
            SimLink::Out(_) => {
                panic!("can't query out links to offer vehicles.")
            }
        }
    }

    #[allow(dead_code)] // Diagnostic hooks are consumed by the alternate network stepper.
    pub(super) fn traces_discharge(&self) -> bool {
        match self {
            SimLink::Local(link) => link.storage_cap.traces_discharge(),
            SimLink::In(link) => link.local_link.storage_cap.traces_discharge(),
            SimLink::Out(link) => link.storage_cap.traces_discharge(),
        }
    }

    #[allow(dead_code)]
    pub(super) fn flow_remaining(&self) -> f64 {
        match self {
            SimLink::Local(link) => link.flow_cap.accumulated(),
            SimLink::In(link) => link.local_link.flow_cap.accumulated(),
            SimLink::Out(_) => panic!("out links have no flow cap"),
        }
    }

    #[allow(dead_code)]
    pub(super) fn flow_limited_front(&self, now: Tick) -> Option<&SimulationVehicle> {
        match self {
            SimLink::Local(link) => link.flow_limited_front(now),
            SimLink::In(link) => link.local_link.flow_limited_front(now),
            SimLink::Out(_) => None,
        }
    }

    pub(super) fn is_veh_stuck(&self, now: impl Into<Tick>) -> bool {
        let now = now.into();
        match self {
            SimLink::Local(ll) => ll.is_veh_stuck(now),
            SimLink::In(il) => il.local_link.is_veh_stuck(now),
            SimLink::Out(_) => {
                panic!("Out links don't offer vehicles")
            }
        }
    }

    /// Suspends stuck accrual for a link whose front vehicle is held legitimately, such
    /// as at a red signal. See `LocalLink::hold_stuck_timer`.
    pub(super) fn hold_stuck_timer(&mut self, now: impl Into<Tick>) {
        match self {
            SimLink::Local(ll) => ll.hold_stuck_timer(now),
            SimLink::In(il) => il.local_link.hold_stuck_timer(now),
            SimLink::Out(_) => {
                panic!("Out links don't offer vehicles")
            }
        }
    }

    /// Whether the out-link currently has room and flow capacity for another vehicle.
    pub fn is_available(&self) -> bool {
        match self {
            SimLink::Local(ll) => ll.is_available(),
            SimLink::In(_) => {
                panic!("In Links can't accept vehicles")
            }
            SimLink::Out(ol) => ol.storage_cap.is_available(),
        }
    }

    pub(super) fn is_sleeping(&self) -> bool {
        match self {
            SimLink::Local(ll) => ll.is_sleeping(),
            SimLink::In(il) => il.local_link.is_sleeping(),
            SimLink::Out(_) => false,
        }
    }

    pub(super) fn wake_up(&mut self, at: Tick) -> bool {
        match self {
            SimLink::Local(ll) => ll.wake_up(at),
            SimLink::In(il) => il.local_link.wake_up(at),
            SimLink::Out(o) => panic!("Out link {} never sleeps.", o.id),
        }
    }

    pub(super) fn is_active(&self) -> bool {
        match self {
            SimLink::Local(ll) => ll.is_active(),
            SimLink::In(il) => il.local_link.is_active(),
            SimLink::Out(o) => {
                panic!(
                    "Trying to check whether out link {} is active. This is not possible.",
                    o.id
                )
            }
        }
    }

    #[allow(dead_code)]
    pub(super) fn used_storage(&self) -> f64 {
        match self {
            SimLink::Local(ll) => ll.storage_cap.used(),
            SimLink::In(il) => il.local_link.storage_cap.used(),
            SimLink::Out(ol) => ol.storage_cap.used(),
        }
    }

    #[allow(dead_code)]
    pub(super) fn max_storage(&self) -> f64 {
        match self {
            SimLink::Local(ll) => ll.storage_cap.max(),
            SimLink::In(il) => il.local_link.storage_cap.max(),
            SimLink::Out(ol) => ol.storage_cap.max(),
        }
    }

    #[allow(dead_code)]
    pub(super) fn traces_storage_acceptance(&self) -> bool {
        match self {
            SimLink::Local(ll) => ll.storage_cap.traces_acceptance(),
            SimLink::In(il) => il.local_link.storage_cap.traces_acceptance(),
            SimLink::Out(ol) => ol.storage_cap.traces_acceptance(),
        }
    }

    pub(super) fn push_veh(
        &mut self,
        vehicle: SimulationVehicle,
        position: LinkPosition,
        now: impl Into<Tick>,
    ) {
        let now = now.into();
        match self {
            SimLink::Local(l) => l.push_veh(vehicle, now, position),
            SimLink::In(il) => il.local_link.push_veh(vehicle, now, position),
            SimLink::Out(ol) => ol.push_veh(vehicle, position),
        }
    }

    pub(super) fn pop_veh_and_restart_stuck_timer(
        &mut self,
        now: impl Into<Tick>,
    ) -> Option<SimulationVehicle> {
        let now = now.into();
        match self {
            SimLink::Local(ll) => ll.pop_veh_and_restart_stuck_timer(now),
            SimLink::In(il) => il.local_link.pop_veh_and_restart_stuck_timer(now),
            SimLink::Out(_) => {
                panic!("Can't pop vehicle from out link")
            }
        }
    }

    pub(super) fn drain(&mut self) -> Vec<SimulationVehicle> {
        match self {
            SimLink::Local(ll) => ll.drain(),
            SimLink::In(il) => il.local_link.drain(),
            SimLink::Out(ol) => ol.take_veh().into(),
        }
    }
}

fn dwell_until(clock: SimClock, now: Tick, seconds: f64) -> Tick {
    clock.time_to_tick(
        clock
            .tick_to_time(now)
            .saturating_add(Duration::from_secs_f64(seconds)),
    )
}

#[derive(Debug)]
pub struct LocalLink {
    pub id: Id<Link>,
    q: VecDeque<VehicleQEntry>,
    buffer: VecDeque<SimulationVehicle>,
    // Sum of the PCE of the vehicles in the buffer. They have already consumed flow capacity.
    buffer_pce: f64,
    waiting_list: VecDeque<SimulationVehicle>,
    /// Transit vehicles dwelling at a stop beside the lane, ordered by when they want to leave.
    /// MATSim's `TransitQLink.transitVehicleStopQueue`.
    stop_bay: Vec<VehicleQEntry>,
    length: f64,
    free_speed: f64,
    storage_cap: StorageCap,
    flow_cap: Flowcap,
    stuck_timer: StuckTimer,
    clock: SimClock,
    // The link does not need to be moved before this tick, see `fall_asleep`.
    sleeping_until: Option<Tick>,
    pub from: Id<Node>,
    pub to: Id<Node>,
}

#[derive(Debug)]
struct VehicleQEntry {
    vehicle: SimulationVehicle,
    earliest_exit_time: Tick,
    /// Whether the vehicle consumed storage when it entered the queue. A transit vehicle that
    /// departs into a stop bay from the waiting list never did, so it must not release any.
    occupies_storage: bool,
}

impl LocalLink {
    fn append_car_spot_speed_samples(&self, now: Tick, samples: &mut Vec<CarSpotSpeedSample>) {
        let mut append = |vehicle: &SimulationVehicle, speed_mps: f64| {
            let driver = vehicle.driver();
            if driver.transit_driver().is_none() && driver.curr_leg().mode.external() == "car" {
                samples.push(CarSpotSpeedSample {
                    vehicle_id: vehicle.id().internal(),
                    link_id: self.id.external().to_owned(),
                    speed_mps,
                });
            }
        };

        for entry in &self.q {
            let speed = if now < entry.earliest_exit_time {
                self.free_speed.min(entry.vehicle.max_v())
            } else {
                0.0
            };
            append(&entry.vehicle, speed);
        }
        for vehicle in &self.buffer {
            append(vehicle, 0.0);
        }
    }

    pub(crate) fn from_link(
        link: &Link,
        storage_capacity: &StorageCapacityDefinition,
        config: &config::QSim,
    ) -> Self {
        LocalLink::build_with_storage_capacity(
            link.id.clone(),
            link.capacity,
            link.freespeed,
            link.length,
            storage_capacity,
            config,
            link.from.clone(),
            link.to.clone(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    pub fn build(
        id: Id<Link>,
        capacity_h: f64,
        free_speed: f64,
        perm_lanes: f64,
        length: f64,
        effective_cell_size: f64,
        config: &config::QSim,
        from: Id<Node>,
        to: Id<Node>,
    ) -> Self {
        let storage_factor = config.storage_capacity_factor.unwrap_or(config.sample_size);
        let storage_capacity = StorageCapacityDefinition::build(
            length,
            perm_lanes,
            capacity_h,
            config.sample_size,
            storage_factor,
            effective_cell_size,
            free_speed,
        );
        Self::build_with_storage_capacity(
            id,
            capacity_h,
            free_speed,
            length,
            &storage_capacity,
            config,
            from,
            to,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_with_storage_capacity(
        id: Id<Link>,
        capacity_h: f64,
        free_speed: f64,
        length: f64,
        storage_capacity: &StorageCapacityDefinition,
        config: &config::QSim,
        from: Id<Node>,
        to: Id<Node>,
    ) -> Self {
        let clock = SimClock::new(config.ticks_per_second);
        let capacity_per_tick =
            (capacity_h * config.sample_size / 3600.) * clock.tick_length().as_secs_f64();
        let storage_cap = StorageCap::from_definition(storage_capacity);

        LocalLink {
            id,
            q: VecDeque::new(),
            buffer: VecDeque::new(),
            buffer_pce: 0.0,
            waiting_list: VecDeque::new(),
            stop_bay: Vec::new(),
            length,
            free_speed,
            storage_cap,
            flow_cap: Flowcap::new(capacity_per_tick),
            stuck_timer: StuckTimer::new(clock.secs_to_tick(config.stuck_threshold as u64)),
            clock,
            sleeping_until: None,
            from,
            to,
        }
    }

    pub fn push_veh(
        &mut self,
        vehicle: SimulationVehicle,
        now: impl Into<Tick>,
        position: LinkPosition,
    ) {
        let now = now.into();
        match position {
            LinkPosition::QStart => self.push_veh_to_queue(vehicle, now),
            LinkPosition::Waiting => self.push_veh_to_waiting_list(vehicle),
        }
    }

    fn push_veh_to_queue(&mut self, vehicle: SimulationVehicle, now: Tick) {
        let speed = self.free_speed.min(vehicle.max_v());
        // Transit vehicles are checked at a stop in the queue phase, then move across the node
        // in the next tick. Account for that phase in their link time so each link does not add a
        // tick of schedule delay; keep one queue tick to preserve partition-independent movement.
        let duration = self.clock.secs_to_ticks_floor(self.length / speed);
        let duration = if vehicle.driver().transit_driver().is_some() {
            duration.saturating_sub(Tick::new(1)).max(Tick::new(1))
        } else {
            duration.max(Tick::new(1))
        };
        let earliest_exit_time = now.saturating_add(duration);

        // update state
        self.storage_cap.consume(vehicle.pce());
        self.q.push_back(VehicleQEntry {
            vehicle,
            earliest_exit_time,
            occupies_storage: true,
        });
    }

    /// Push a vehicle into the waiting list. The departure wakes up a sleeping link.
    pub fn push_veh_to_waiting_list(&mut self, vehicle: SimulationVehicle) {
        self.sleeping_until = None;
        self.waiting_list.push_back(vehicle);
    }

    /// This method fills the buffer from two sources with priority:
    /// 1. Check if there are vehicles in the waiting list and move them to the buffer.
    /// 2. Check if there are vehicles in the queue that have reached their earliest exit time and move them to the buffer.
    ///
    /// Both is done only if the flow capacity allows this. A vehicle consumes flow capacity when it enters the buffer.
    ///
    /// Returns the vehicles that end their leg on the link
    pub(crate) fn do_sim_step(
        &mut self,
        now: impl Into<Tick>,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        transit: &mut TransitStops,
    ) -> Vec<SimulationVehicle> {
        let now = now.into();
        let buffer_was_empty = self.buffer.is_empty();
        self.update_flow_cap(now);
        if self.waiting_list.is_empty()
            && self
                .q
                .front()
                .is_none_or(|entry| entry.earliest_exit_time > now)
            && self
                .stop_bay
                .first()
                .is_none_or(|entry| entry.earliest_exit_time > now)
        {
            return Vec::new();
        }

        // MATSim's default order: waiting vehicles first, then transit vehicles leaving their
        // stop bay, then the queue.
        let mut ending_vehicles = self.add_waiting_to_buffer(comp_env, transit, now);
        self.release_stop_bay(now);
        ending_vehicles.append(&mut self.add_queue_to_buffer(comp_env, transit, now));
        let now_time = self.clock.tick_to_time(now);
        if buffer_was_empty && !self.buffer.is_empty() {
            self.restart_stuck_timer(now);
        }

        // emit events for new vehicles that end their leg on the link
        for v in &ending_vehicles {
            comp_env.events_manager_borrow_mut().process_event(
                &VehicleLeavesTrafficEventBuilder::default()
                    .vehicle(v.id().clone())
                    .link(self.id.clone())
                    .person(v.driver().id().clone())
                    .time(now_time)
                    .network_mode(v.driver().curr_leg().mode.clone())
                    .build()
                    .unwrap(),
            );
        }

        ending_vehicles
    }

    fn add_queue_to_buffer(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        transit: &mut TransitStops,
        now: Tick,
    ) -> Vec<SimulationVehicle> {
        let mut released_vehicles = vec![];

        loop {
            let option = self.q.front_mut();

            // If queue is empty, break the loop.
            if option.is_none() {
                break;
            }

            let veh = option.unwrap();

            // If the earliest exit time has not passed, nothing to do
            if veh.earliest_exit_time > now {
                break;
            }

            // A transit vehicle serves a stop at the end of the link before anything else.
            let outcome = if veh.vehicle.driver().transit_driver().is_some() {
                serve_stop(
                    &mut veh.vehicle,
                    &self.id,
                    self.clock.tick_to_time(now),
                    transit,
                    &mut comp_env.events_manager_borrow_mut(),
                    false,
                )
            } else {
                StopOutcome::NoStop
            };
            match outcome {
                StopOutcome::NoStop => {}
                StopOutcome::Departed => continue,
                StopOutcome::Dwell {
                    seconds,
                    blocks_lane,
                } => {
                    veh.earliest_exit_time = dwell_until(self.clock, now, seconds);
                    if blocks_lane {
                        // The vehicle stays at the head of the queue and holds everyone behind.
                        break;
                    }
                    let entry = self.q.pop_front().unwrap();
                    self.park_in_stop_bay(entry);
                    continue;
                }
            }

            let veh = self.q.front().unwrap();
            let arrive = veh.vehicle.driver().is_wanting_to_arrive_on_current_link();
            let capacity_left = self.has_flow_capacity_left();

            // If the vehicle wants to arrive, remove it from the queue
            if arrive {
                let entry = self.q.pop_front().unwrap();
                self.release_storage(&entry);
                released_vehicles.push(entry.vehicle);
                continue;
            }

            // If the vehicle wants to move to another link, put it into buffer
            if capacity_left {
                let entry = self.q.pop_front().unwrap();
                self.release_storage(&entry);
                self.push_to_buffer(entry.vehicle);
            } else {
                break;
            }
        }

        released_vehicles
    }

    fn release_storage(&mut self, entry: &VehicleQEntry) {
        if entry.occupies_storage {
            self.storage_cap.release(entry.vehicle.pce());
        }
    }

    /// Keeps the bay ordered as MATSim's stop queue: earliest exit first, and on a tie the
    /// vehicle with the lexicographically larger id first.
    ///
    /// A vehicle in the bay keeps the storage it occupied. MATSim frees it on entering the bay
    /// and frees it again when the vehicle finally leaves the link, which leaks capacity; here a
    /// vehicle frees storage once, and a split link never has to report storage growing back.
    fn park_in_stop_bay(&mut self, entry: VehicleQEntry) {
        let key = |e: &VehicleQEntry| {
            (
                e.earliest_exit_time,
                Reverse(e.vehicle.id().external().to_owned()),
            )
        };
        let position = self.stop_bay.partition_point(|e| key(e) <= key(&entry));
        self.stop_bay.insert(position, entry);
    }

    /// Moves transit vehicles whose dwell has ended back to the head of the queue, in bay order.
    /// MATSim's `handleTransitVehiclesInStopQueue`.
    fn release_stop_bay(&mut self, now: Tick) {
        let due = self
            .stop_bay
            .partition_point(|e| e.earliest_exit_time <= now);
        for entry in self.stop_bay.drain(..due).rev() {
            self.q.push_front(entry);
        }
    }

    fn add_waiting_to_buffer(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        transit: &mut TransitStops,
        now: Tick,
    ) -> Vec<SimulationVehicle> {
        let mut released_vehicles = vec![];

        loop {
            let option = self.waiting_list.front();

            // If waiting list is empty, break the loop.
            if option.is_none() {
                break;
            }

            // A transit vehicle enters traffic like MATSim's `moveWaitToRoad`: once the link
            // accepts it, then it serves the stops at the start of its route.
            if option.unwrap().driver().transit_driver().is_some() {
                if !self.is_accepting_from_wait() {
                    break;
                }
                let vehicle = self.pop_from_waiting(comp_env, now);
                let Some(vehicle) = self.serve_stops_on_entry(vehicle, comp_env, transit, now)
                else {
                    continue;
                };
                if vehicle.driver().is_wanting_to_arrive_on_current_link() {
                    released_vehicles.push(vehicle);
                } else {
                    self.push_to_buffer(vehicle);
                }
                continue;
            }

            // If arrival on link, remove from waiting list and put into buffer
            if option
                .unwrap()
                .driver()
                .is_wanting_to_arrive_on_current_link()
            {
                released_vehicles.push(self.pop_from_waiting(comp_env, now));
                continue;
            }

            // If not arriving on link, check if flow capacity allows to move vehicle to buffer
            if self.is_accepting_from_wait() {
                let vehicle = self.pop_from_waiting(comp_env, now);
                self.push_to_buffer(vehicle);
            } else {
                break;
            }
        }

        released_vehicles
    }

    fn pop_from_waiting(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        now: Tick,
    ) -> SimulationVehicle {
        let vehicle = self.waiting_list.pop_front().unwrap();
        let now_time = self.clock.tick_to_time(now);
        comp_env.events_manager_borrow_mut().process_event(
            &VehicleEntersTrafficEventBuilder::default()
                .vehicle(vehicle.id().clone())
                .link(self.id.clone())
                .person(vehicle.driver().id().clone())
                .time(now_time)
                .network_mode(vehicle.driver().curr_leg().mode.clone())
                .build()
                .unwrap(),
        );
        vehicle
    }

    /// Serves every stop on this link a departing transit vehicle reaches before it moves. A
    /// vehicle that has to dwell goes to the stop bay even at a blocking stop, because it is not
    /// on the lane yet (MATSim's `addTransitToStopQueue`), and `None` is returned.
    fn serve_stops_on_entry(
        &mut self,
        mut vehicle: SimulationVehicle,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        transit: &mut TransitStops,
        now: Tick,
    ) -> Option<SimulationVehicle> {
        loop {
            match serve_stop(
                &mut vehicle,
                &self.id,
                self.clock.tick_to_time(now),
                transit,
                &mut comp_env.events_manager_borrow_mut(),
                false,
            ) {
                StopOutcome::NoStop => return Some(vehicle),
                StopOutcome::Departed => {}
                StopOutcome::Dwell { seconds, .. } => {
                    let earliest_exit_time = dwell_until(self.clock, now, seconds);
                    self.park_in_stop_bay(VehicleQEntry {
                        vehicle,
                        earliest_exit_time,
                        occupies_storage: false,
                    });
                    return None;
                }
            }
        }
    }

    fn is_accepting_from_wait(&self) -> bool {
        self.has_flow_capacity_left()
    }

    fn has_flow_capacity_left(&self) -> bool {
        self.flow_cap.has_capacity_left()
    }

    /// Moves a vehicle into the buffer. As in MATSim's QSim, the vehicle consumes flow capacity
    /// when it enters the buffer.
    fn push_to_buffer(&mut self, vehicle: SimulationVehicle) {
        self.flow_cap.consume(vehicle.pce());
        self.buffer_pce += vehicle.pce();
        self.buffer.push_back(vehicle);
    }

    #[allow(dead_code)]
    fn flow_limited_front(&self, now: Tick) -> Option<&SimulationVehicle> {
        if !self.flow_cap.has_capacity_left() {
            if let Some(vehicle) = self.buffer.front() {
                return Some(vehicle);
            }
        }
        let queued = self.q.front()?;
        (queued.earliest_exit_time <= now && !self.flow_cap.has_capacity_left())
            .then_some(&queued.vehicle)
    }

    fn pop_veh_and_restart_stuck_timer(
        &mut self,
        now: impl Into<Tick>,
    ) -> Option<SimulationVehicle> {
        if let Some(veh) = self.buffer.pop_front() {
            // The vehicle has consumed flow capacity when it entered the buffer. An empty buffer resets the sum exactly.
            self.buffer_pce = if self.buffer.is_empty() {
                0.0
            } else {
                self.buffer_pce - veh.pce()
            };
            self.restart_stuck_timer(now);
            Some(veh)
        } else {
            None
        }
    }

    fn update_flow_cap(&mut self, now: Tick) {
        // increase flow cap if new time step
        self.flow_cap.update_capacity(now, self.buffer_pce);
    }

    /// This method returns the next vehicle allowed to leave the connection.
    fn offers_veh(&self) -> Option<&SimulationVehicle> {
        self.buffer.front()
    }

    #[cfg(test)]
    pub(super) fn veh_count(&self) -> usize {
        self.q.len() + self.waiting_list.len() + self.buffer.len() + self.stop_bay.len()
    }

    pub fn is_available(&self) -> bool {
        self.storage_cap.is_available()
    }

    fn drain(&mut self) -> Vec<SimulationVehicle> {
        let mut vehicles = Vec::with_capacity(
            self.q.len() + self.buffer.len() + self.waiting_list.len() + self.stop_bay.len(),
        );
        vehicles.extend(self.q.drain(..).map(|entry| entry.vehicle));
        vehicles.extend(self.buffer.drain(..));
        self.buffer_pce = 0.0;
        vehicles.extend(self.waiting_list.drain(..));
        vehicles.extend(self.stop_bay.drain(..).map(|entry| entry.vehicle));
        vehicles
    }

    /// A link is active, if either the queue, waiting_list, buffer or stop bay is not empty.
    pub(super) fn is_active(&self) -> bool {
        !self.q.is_empty()
            || !self.waiting_list.is_empty()
            || !self.buffer.is_empty()
            || !self.stop_bay.is_empty()
    }

    pub(super) fn is_veh_stuck(&self, now: impl Into<Tick>) -> bool {
        self.stuck_timer.is_stuck(now)
    }

    pub(super) fn restart_stuck_timer(&mut self, now: impl Into<Tick>) {
        self.stuck_timer.restart(now);
    }

    /// Starts the stuck clock now, for a link holding vehicles whose hold is legitimate.
    ///
    /// A red signal is such a hold: the vehicle is stopped by the plan, not by a
    /// blockage, and must not be forced through the red once `stuck_threshold` elapses.
    pub(super) fn hold_stuck_timer(&mut self, now: impl Into<Tick>) {
        self.restart_stuck_timer(now);
    }

    fn from(&self) -> &Id<Node> {
        &self.from
    }

    fn to(&self) -> &Id<Node> {
        &self.to
    }

    pub fn to_nodes_active(&self) -> bool {
        // the node will only look at the vehicle at the at the top of the queue in the next timestep
        // therefore, peek whether vehicles are available for the next timestep.
        self.offers_veh().is_some()
    }

    /// Lets the link sleep until its first vehicle is due, if moving it until then would change
    /// nothing: no vehicle waits for departure, the buffer is empty and the flow capacity is full.
    /// Returns the tick from which on the link has to be moved again.
    pub(super) fn fall_asleep(&mut self, now: Tick) -> Option<Tick> {
        if !self.waiting_list.is_empty() || !self.buffer.is_empty() || !self.flow_cap.is_full() {
            return None;
        }
        let wake_up = self.q.front()?.earliest_exit_time;
        if wake_up <= now {
            return None;
        }
        self.sleeping_until = Some(wake_up);
        Some(wake_up)
    }

    /// Wakes the link up, if it still sleeps until `at`. Returns false, if the link was woken up
    /// earlier.
    pub(super) fn wake_up(&mut self, at: Tick) -> bool {
        if self.sleeping_until == Some(at) {
            self.sleeping_until = None;
            true
        } else {
            false
        }
    }

    pub(super) fn is_sleeping(&self) -> bool {
        self.sleeping_until.is_some()
    }
}

#[derive(Debug)]
pub struct SplitOutLink {
    pub id: Id<Link>,
    from: Id<Node>,
    pub to_part: u32,
    q: VecDeque<SimulationVehicle>,
    storage_cap: StorageCap,
}

impl SplitOutLink {
    pub(crate) fn new(
        link: &Link,
        storage_capacity: &StorageCapacityDefinition,
        to_part: u32,
    ) -> SplitOutLink {
        let storage_cap = StorageCap::from_definition(storage_capacity);

        SplitOutLink {
            id: link.id.clone(),
            from: link.from.clone(),
            to_part,
            q: VecDeque::default(),
            storage_cap,
        }
    }

    pub fn apply_storage_cap_update(&mut self, released: f64) {
        self.storage_cap.consume(-released);
    }

    pub fn take_veh(&mut self) -> VecDeque<SimulationVehicle> {
        std::mem::take(&mut self.q)
    }

    pub fn push_veh(&mut self, veh: SimulationVehicle, position: LinkPosition) {
        match position {
            LinkPosition::QStart => {}
            LinkPosition::Waiting => {
                panic!(
                    "SplitOutLink {} cannot push vehicle {:?} into the buffer.",
                    self.id, veh
                )
            }
        }
        self.storage_cap.consume(veh.pce());
        self.q.push_back(veh);
    }
}

#[derive(Debug)]
pub struct SplitInLink {
    pub from_part: u32,
    pub local_link: LocalLink,
}

impl SplitInLink {
    pub(super) fn new(from_part: u32, local_link: LocalLink) -> Self {
        SplitInLink {
            from_part,
            local_link,
        }
    }

    pub(super) fn occupied_storage(&self) -> f64 {
        self.local_link.storage_cap.used()
    }
}

#[cfg(test)]
mod sim_link_tests {
    use crate::simulation::id::Id;
    use crate::simulation::network::link::LinkPosition::QStart;
    use crate::simulation::network::link::{LinkPosition, LocalLink, SimLink};
    use crate::simulation::vehicles::SimulationVehicle;
    use crate::test_utils;
    use crate::test_utils::create_agent_without_route;
    use assert_approx_eq::assert_approx_eq;
    use macros::deterministic_id_test;

    #[deterministic_id_test]
    fn storage_cap_consumed() {
        let mut link = SimLink::Local(LocalLink::build(
            Id::create("0"),
            3600.,
            10.,
            3.,
            100.,
            7.5,
            &test_utils::qsim_config(),
            Id::create("0"),
            Id::create("0"),
        ));
        let agent = create_agent_without_route(1);
        let vehicle = SimulationVehicle::from_parts(1, 0, 10., 1.5, agent);

        link.push_veh(vehicle, QStart, 0);

        // storage capacity should be consumed immediately. The expected value is max_storage_cap - pce of the vehicle
        assert_eq!(1.5, link.used_storage())
    }

    #[deterministic_id_test]
    fn storage_cap_released() {
        let mut link = SimLink::Local(LocalLink::build(
            Id::create("0"),
            3600.,
            10.,
            3.,
            10.,
            7.5,
            &test_utils::qsim_config(),
            Id::create("0"),
            Id::create("0"),
        ));
        let agent = create_agent_without_route(1);
        let vehicle = SimulationVehicle::from_parts(1, 0, 10., 1.5, agent);

        link.push_veh(vehicle, QStart, 0);

        // After pushing, storage is 1.5
        assert_eq!(1.5, link.used_storage());

        let SimLink::Local(l) = &mut link else {
            unreachable!()
        };

        l.do_sim_step(1, &mut Default::default(), &mut Default::default());
        let _vehicle = link.pop_veh_and_restart_stuck_timer(1).unwrap();

        // After popping, storage is 0.
        assert_eq!(0., link.used_storage());
    }

    #[deterministic_id_test]
    fn flow_cap_accumulates() {
        let mut link = SimLink::Local(LocalLink::build(
            Id::create("0"),
            360.,
            10.,
            3.,
            100.,
            7.5,
            &test_utils::qsim_config(),
            Id::create("0"),
            Id::create("0"),
        ));

        let agent1 = create_agent_without_route(1);
        let vehicle1 = SimulationVehicle::from_parts(1, 0, 10., 1.5, agent1);
        let agent2 = create_agent_without_route(2);
        let vehicle2 = SimulationVehicle::from_parts(2, 0, 10., 1.5, agent2);

        link.push_veh(vehicle1, QStart, 0);
        link.push_veh(vehicle2, QStart, 0);

        let SimLink::Local(l) = &mut link else {
            unreachable!()
        };

        l.do_sim_step(10, &mut Default::default(), &mut Default::default());

        // this should reduce the flow capacity, so that no other vehicle can leave during this time step
        let popped1 = l.pop_veh_and_restart_stuck_timer(10).unwrap();
        assert_eq!("1", popped1.id().external());

        // as the flow cap is 0.1/s the next vehicle can leave the link 15s after the first
        for now in 11..24 {
            l.do_sim_step(now, &mut Default::default(), &mut Default::default());
            assert!(l.offers_veh().is_none());
        }
        l.do_sim_step(25, &mut Default::default(), &mut Default::default());

        if let Some(popped2) = link.offers_veh() {
            assert_eq!("2", popped2.id().external());
        } else {
            panic!("Expected vehicle2 to be available at t=30")
        }
    }

    #[deterministic_id_test]
    fn calculates_exit_time() {
        let mut link = SimLink::Local(LocalLink::build(
            Id::create("0"),
            3600.,
            10.,
            3.,
            100.,
            7.5,
            &test_utils::qsim_config(),
            Id::create("0"),
            Id::create("0"),
        ));

        let agent1 = create_agent_without_route(1);
        let vehicle1 = SimulationVehicle::from_parts(1, 0, 10., 1.5, agent1);

        link.push_veh(vehicle1, QStart, 0);

        // this is also implicitly tested above, but we'll do it here again, so that we have descriptive
        // test naming
        for now in 0..9 {
            let SimLink::Local(l) = &mut link else {
                unreachable!()
            };
            l.do_sim_step(now, &mut Default::default(), &mut Default::default());
            assert!(link.offers_veh().is_none());
        }

        let SimLink::Local(l) = &mut link else {
            unreachable!()
        };
        l.do_sim_step(10, &mut Default::default(), &mut Default::default());
        assert!(link.offers_veh().is_some())
    }

    #[deterministic_id_test]
    fn stuck_timer_starts_when_vehicle_enters_buffer() {
        let mut config = test_utils::qsim_config();
        config.stuck_threshold = 10;
        let mut link = SimLink::Local(LocalLink::build(
            Id::create("stuck-link"),
            3600.,
            1.,
            1.,
            10.,
            7.5,
            &config,
            Id::create("from-node"),
            Id::create("to-node"),
        ));
        let vehicle = SimulationVehicle::from_parts(1, 0, 10., 1., create_agent_without_route(1));
        link.push_veh(vehicle, QStart, 0);

        let SimLink::Local(local_link) = &mut link else {
            unreachable!()
        };
        local_link.do_sim_step(9, &mut Default::default(), &mut Default::default());
        assert!(local_link.offers_veh().is_none());
        assert!(!local_link.is_veh_stuck(100));

        local_link.do_sim_step(10, &mut Default::default(), &mut Default::default());
        assert!(local_link.offers_veh().is_some());
        assert!(!local_link.is_veh_stuck(19));
        assert!(local_link.is_veh_stuck(20));
    }

    #[deterministic_id_test]
    fn popping_front_vehicle_restarts_stuck_timer_for_next_vehicle() {
        let mut config = test_utils::qsim_config();
        config.stuck_threshold = 10;
        let mut link = SimLink::Local(LocalLink::build(
            Id::create("stuck-link"),
            7200.,
            1.,
            1.,
            1.,
            7.5,
            &config,
            Id::create("from-node"),
            Id::create("to-node"),
        ));
        for id in 1..=2 {
            let vehicle =
                SimulationVehicle::from_parts(id, 0, 10., 1., create_agent_without_route(id));
            link.push_veh(vehicle, QStart, 0);
        }

        let SimLink::Local(local_link) = &mut link else {
            unreachable!()
        };
        local_link.do_sim_step(1, &mut Default::default(), &mut Default::default());
        assert!(local_link.offers_veh().is_some());

        let popped = local_link.pop_veh_and_restart_stuck_timer(5).unwrap();
        assert_eq!("1", popped.id().external());
        assert!(local_link.offers_veh().is_some());
        assert!(!local_link.is_veh_stuck(14));
        assert!(local_link.is_veh_stuck(15));
    }

    #[deterministic_id_test]
    fn fifo_ordering() {
        let id1 = 42;
        let id2 = 43;
        let mut link = SimLink::Local(LocalLink::build(
            Id::create("1"),
            1.,
            1.,
            1.,
            15.0,
            10.0,
            &test_utils::qsim_config(),
            Id::create("0"),
            Id::create("0"),
        ));

        let agent1 = create_agent_without_route(1);
        let vehicle1 = SimulationVehicle::from_parts(id1, 0, 10., 1., agent1);
        let agent2 = create_agent_without_route(1);
        let vehicle2 = SimulationVehicle::from_parts(id2, 0, 10., 1., agent2);

        link.push_veh(vehicle1, QStart, 0);
        assert_approx_eq!(1., link.used_storage());
        assert!(link.is_available());

        link.push_veh(vehicle2, QStart, 0);
        assert_approx_eq!(2.0, link.used_storage());
        assert!(!link.is_available());

        let SimLink::Local(l) = &mut link else {
            unreachable!()
        };
        l.do_sim_step(15, &mut Default::default(), &mut Default::default());

        // First vehicle pops after 15 s
        let popped_vehicle1 = l.pop_veh_and_restart_stuck_timer(15).unwrap();
        assert_eq!(id1.to_string(), popped_vehicle1.id().external());

        l.do_sim_step(3614, &mut Default::default(), &mut Default::default());
        assert!(l.pop_veh_and_restart_stuck_timer(3614).is_none());

        // Second vehicle pops after 3615 s
        l.do_sim_step(3615, &mut Default::default(), &mut Default::default());
        let popped_vehicle2 = link.pop_veh_and_restart_stuck_timer(3615).unwrap();
        assert_eq!(id2.to_string(), popped_vehicle2.id().external());
    }

    /// A 10 m link with a free speed of 10 m/s, so that vehicles need one second on the link. The
    /// capacities used below are exactly representable per tick, e.g. 900 veh/h = 0.25 veh/s.
    fn flow_test_link(capacity_h: f64, ticks_per_second: u32) -> LocalLink {
        let mut config = test_utils::qsim_config();
        config.ticks_per_second = ticks_per_second;
        LocalLink::build(
            Id::create("flow-link"),
            capacity_h,
            10.,
            1.,
            10.,
            7.5,
            &config,
            Id::create("from-node"),
            Id::create("to-node"),
        )
    }

    fn flow_test_vehicle(id: u64, pce: f64) -> SimulationVehicle {
        SimulationVehicle::from_parts(id, 0, 10., pce, create_agent_without_route(id))
    }

    fn buffered_ids(link: &LocalLink) -> Vec<String> {
        link.buffer
            .iter()
            .map(|v| v.id().external().to_owned())
            .collect()
    }

    /// Setting: 0.25 veh per tick, two vehicles with pce 1 reach the end of the link at tick 1.
    /// Execution: One link step.
    /// Expectation: The first vehicle consumes the capacity when it enters the buffer and is offered,
    /// although the accumulated capacity is negative now. The second vehicle stays in the queue.
    #[deterministic_id_test]
    fn flow_capacity_is_consumed_when_vehicle_enters_buffer() {
        let mut link = flow_test_link(900., 1);
        link.push_veh(flow_test_vehicle(1, 1.), 0, QStart);
        link.push_veh(flow_test_vehicle(2, 1.), 0, QStart);

        link.do_sim_step(1, &mut Default::default(), &mut Default::default());

        assert_eq!(vec!["1"], buffered_ids(&link));
        assert_eq!(1, link.q.len());
        assert_eq!(-0.75, link.flow_cap.accumulated());
        assert_eq!("1", link.offers_veh().unwrap().id().external());
    }

    /// Setting: 0.25 veh per tick. The first vehicle enters the buffer at tick 1 and is blocked
    /// there until tick 21; the second vehicle is due since tick 1.
    /// Execution: Link steps every tick, the node takes the first vehicle at tick 21.
    /// Expectation: While the first vehicle is blocked, no capacity accumulates. The second vehicle
    /// enters the buffer once 1 pce has accumulated after the first one left, i.e. at tick 24.
    #[deterministic_id_test]
    fn blocked_vehicle_holds_flow_capacity_until_it_leaves_buffer() {
        let mut link = flow_test_link(900., 1);
        link.push_veh(flow_test_vehicle(1, 1.), 0, QStart);
        link.push_veh(flow_test_vehicle(2, 1.), 0, QStart);

        for now in 1..=20 {
            link.do_sim_step(now, &mut Default::default(), &mut Default::default());
            assert_eq!(vec!["1"], buffered_ids(&link), "tick {now}");
            assert_eq!(-0.75, link.flow_cap.accumulated(), "tick {now}");
        }

        let popped = link.pop_veh_and_restart_stuck_timer(21).unwrap();
        assert_eq!("1", popped.id().external());

        for now in 21..=23 {
            link.do_sim_step(now, &mut Default::default(), &mut Default::default());
            assert!(link.buffer.is_empty(), "tick {now}");
        }
        link.do_sim_step(24, &mut Default::default(), &mut Default::default());
        assert_eq!(vec!["2"], buffered_ids(&link));
    }

    /// Setting: 2 veh per tick and three vehicles with pce 1, which are due at tick 1.
    /// Execution: Link steps at ticks 1 and 2, the node takes all offered vehicles in between.
    /// Expectation: Two vehicles enter the buffer at tick 1, the third one at tick 2.
    #[deterministic_id_test]
    fn several_vehicles_enter_buffer_within_one_tick() {
        let mut link = flow_test_link(7200., 1);
        for id in 1..=3 {
            link.push_veh(flow_test_vehicle(id, 1.), 0, QStart);
        }

        link.do_sim_step(1, &mut Default::default(), &mut Default::default());
        assert_eq!(vec!["1", "2"], buffered_ids(&link));

        assert!(link.pop_veh_and_restart_stuck_timer(2).is_some());
        assert!(link.pop_veh_and_restart_stuck_timer(2).is_some());
        link.do_sim_step(2, &mut Default::default(), &mut Default::default());
        assert_eq!(vec!["3"], buffered_ids(&link));
    }

    /// Setting: 2 veh per tick, a truck with pce 2.5 followed by a car with pce 1.
    /// Execution: Link steps at ticks 1 to 3, the node takes the truck at tick 2.
    /// Expectation: The truck enters with 2 pce available and leaves a deficit of 0.5, which blocks
    /// the car in tick 1. After the truck has left, the car enters at tick 2.
    #[deterministic_id_test]
    fn large_vehicle_consumes_its_pce_on_entering_buffer() {
        let mut link = flow_test_link(7200., 1);
        link.push_veh(flow_test_vehicle(1, 2.5), 0, QStart);
        link.push_veh(flow_test_vehicle(2, 1.), 0, QStart);

        link.do_sim_step(1, &mut Default::default(), &mut Default::default());
        assert_eq!(vec!["1"], buffered_ids(&link));
        assert_eq!(-0.5, link.flow_cap.accumulated());

        assert!(link.pop_veh_and_restart_stuck_timer(2).is_some());
        link.do_sim_step(2, &mut Default::default(), &mut Default::default());
        assert_eq!(vec!["2"], buffered_ids(&link));
        assert_eq!(0.5, link.flow_cap.accumulated());
    }

    /// Setting: 4 ticks per second and 0.25 veh/s, i.e. 0.0625 veh per tick. Two vehicles are due at
    /// tick 4.
    /// Execution: The node takes the first vehicle at tick 5, the link steps every tick.
    /// Expectation: The second vehicle needs 16 ticks of capacity after the first vehicle entered
    /// the buffer. It enters at tick 20.
    #[deterministic_id_test]
    fn flow_capacity_below_one_vehicle_per_tick_with_sub_second_ticks() {
        let mut link = flow_test_link(900., 4);
        link.push_veh(flow_test_vehicle(1, 1.), 0, QStart);
        link.push_veh(flow_test_vehicle(2, 1.), 0, QStart);

        link.do_sim_step(4, &mut Default::default(), &mut Default::default());
        assert_eq!(vec!["1"], buffered_ids(&link));
        assert!(link.pop_veh_and_restart_stuck_timer(5).is_some());

        for now in 5..=19 {
            link.do_sim_step(now, &mut Default::default(), &mut Default::default());
            assert!(link.buffer.is_empty(), "tick {now}");
        }
        link.do_sim_step(20, &mut Default::default(), &mut Default::default());
        assert_eq!(vec!["2"], buffered_ids(&link));
    }

    /// Setting: Three departures in the waiting list: the second vehicle ends its leg on the link.
    /// Execution: One link step with 0.25 veh per tick.
    /// Expectation: The first departure consumes the capacity when it enters the buffer, the
    /// arriving vehicle leaves without consuming capacity, and the third departure has to wait.
    #[deterministic_id_test]
    fn departures_consume_flow_capacity_but_arrivals_do_not() {
        let mut link = flow_test_link(900., 1);
        link.push_veh(flow_test_vehicle(1, 1.), 0, LinkPosition::Waiting);
        let arriving_agent = test_utils::create_agent(2, vec!["flow-link"]);
        let arriving = SimulationVehicle::from_parts(2, 0, 10., 1., arriving_agent);
        link.push_veh(arriving, 0, LinkPosition::Waiting);
        link.push_veh(flow_test_vehicle(3, 1.), 0, LinkPosition::Waiting);

        let ending = link.do_sim_step(0, &mut Default::default(), &mut Default::default());

        assert_eq!(
            vec!["2"],
            ending
                .iter()
                .map(|v| v.id().external().to_owned())
                .collect::<Vec<_>>()
        );
        assert_eq!(vec!["1"], buffered_ids(&link));
        assert_eq!(1, link.waiting_list.len());
        assert_eq!(-0.75, link.flow_cap.accumulated());
    }

    /// Lets two vehicles pass a link with 800 veh/h at a 0.1 % sample, i.e. 1/4500 veh per tick.
    /// The first vehicle enters the buffer at tick 1 and leaves at tick 2. The second vehicle is
    /// due at `second_due`. Before that, the link is updated in every tick if `update_every_tick`,
    /// and not at all otherwise, as if it was inactive. Returns the tick in which the second
    /// vehicle enters the buffer.
    fn tick_when_second_vehicle_enters_buffer(update_every_tick: bool, second_due: u64) -> u64 {
        let mut config = test_utils::qsim_config();
        config.sample_size = 0.001;
        let mut link = LocalLink::build(
            Id::create("tie-link"),
            800.,
            10.,
            1.,
            10.,
            7.5,
            &config,
            Id::create("from-node"),
            Id::create("to-node"),
        );
        link.push_veh(flow_test_vehicle(1, 1.), 0, QStart);
        link.do_sim_step(1, &mut Default::default(), &mut Default::default());
        assert!(link.pop_veh_and_restart_stuck_timer(2).is_some());
        link.do_sim_step(2, &mut Default::default(), &mut Default::default());

        link.push_veh(flow_test_vehicle(2, 1.), second_due - 1, QStart);
        let first_step = if update_every_tick { 3 } else { second_due };
        (first_step..10_000)
            .find(|&now| {
                link.do_sim_step(now, &mut Default::default(), &mut Default::default());
                !link.buffer.is_empty()
            })
            .unwrap()
    }

    /// Setting: The headway of 4500 ticks makes the exact accumulated capacity 0 at tick 4500.
    /// Execution: The capacity is accumulated tick by tick, or at once after the link was inactive.
    /// Expectation: Both enter the buffer at tick 4501, as with exact arithmetic, although
    /// rounding leaves a tiny positive residue at tick 4500 when updating tick by tick.
    #[deterministic_id_test]
    fn flow_capacity_tie_does_not_depend_on_update_pattern() {
        assert_eq!(4501, tick_when_second_vehicle_enters_buffer(true, 4));
        assert_eq!(4501, tick_when_second_vehicle_enters_buffer(false, 4000));
    }

    /// Setting: Vehicles whose pce values do not add up exactly in floating point.
    /// Execution: All of them enter the buffer and leave it again.
    /// Expectation: The pce in the buffer is exactly 0 afterwards, so that the flow capacity is
    /// refilled to exactly one tick of capacity.
    #[deterministic_id_test]
    fn buffer_pce_is_exactly_zero_after_mixed_vehicles_left() {
        let mut link = flow_test_link(36000., 1);
        for (id, pce) in [(1, 0.1), (2, 0.2), (3, 0.7)] {
            link.push_veh(flow_test_vehicle(id, pce), 0, QStart);
        }

        link.do_sim_step(1, &mut Default::default(), &mut Default::default());
        assert_eq!(vec!["1", "2", "3"], buffered_ids(&link));
        while link.pop_veh_and_restart_stuck_timer(2).is_some() {}

        assert_eq!(0.0, link.buffer_pce);
        link.do_sim_step(2, &mut Default::default(), &mut Default::default());
        assert_eq!(10.0, link.flow_cap.accumulated());
    }
}

#[cfg(test)]
mod out_link_tests {
    use crate::simulation::id::Id;
    use crate::simulation::network::link::LinkPosition::QStart;
    use crate::simulation::network::link::{SimLink, SplitOutLink};
    use crate::simulation::network::storage_cap::StorageCap;
    use crate::simulation::vehicles::SimulationVehicle;
    use crate::test_utils::create_agent_without_route;
    use macros::deterministic_id_test;

    #[deterministic_id_test]
    fn push_and_take() {
        let mut link = SimLink::Out(SplitOutLink {
            id: Id::new_internal(0),
            from: Id::new_internal(0),
            to_part: 1,
            q: Default::default(),
            storage_cap: StorageCap::build(100., 1., 1., 1., 1., 1.),
        });
        let id1 = 42;
        let id2 = 43;
        let agent1 = create_agent_without_route(1);
        let vehicle1 = SimulationVehicle::from_parts(id1, 0, 10., 1., agent1);
        let agent2 = create_agent_without_route(1);
        let vehicle2 = SimulationVehicle::from_parts(id2, 0, 10., 1., agent2);

        link.push_veh(vehicle1, QStart, 0);
        link.push_veh(vehicle2, QStart, 0);

        // storage should be consumed
        assert_eq!(2., link.used_storage());

        if let SimLink::Out(ref mut ol) = link {
            let mut result = ol.take_veh();

            // make sure, that vehicles have correct order
            assert_eq!(2, result.len());
            let taken_1 = result.pop_front().unwrap();
            assert_eq!(id1.to_string(), taken_1.id().external());
            let taken_2 = result.pop_front().unwrap();
            assert_eq!(id2.to_string(), taken_2.id().external());

            // make sure storage capacity is not released
            assert_eq!(2., link.used_storage());
        } else {
            panic!("expected out link")
        }
    }

    #[deterministic_id_test]
    fn update_storage_caps() {
        // set up the link, so that we consume two units of storage.
        let mut cap = StorageCap::build(100., 1., 1., 1., 1., 1.);
        cap.consume(2.);
        let mut out_link = SplitOutLink {
            id: Id::new_internal(0),
            from: Id::new_internal(0),
            to_part: 1,
            q: Default::default(),
            storage_cap: cap,
        };

        assert_eq!(2., out_link.storage_cap.used());
        out_link.apply_storage_cap_update(2.);

        assert_eq!(0., out_link.storage_cap.used());
    }
}
