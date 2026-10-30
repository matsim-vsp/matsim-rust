use super::link::{LocalLink, SimLink, SplitInLink, SplitOutLink};
use crate::simulation::agents::LegAgent;
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::events::{EventsManager, LinkEnterEventBuilder, LinkLeaveEventBuilder};
use crate::simulation::id::Id;
use crate::simulation::id::serializable_type::StableTypeId;
use crate::simulation::network::LinkStorageCapacities;
use crate::simulation::network::link::LinkPosition::{QStart, Waiting};
use crate::simulation::scenario::network::{Link, Network, Node};
use crate::simulation::time::{SimClock, Tick};
use crate::simulation::vehicles::SimulationVehicle;
use crate::simulation::{config, random};
use nohash_hasher::{IntMap, IntSet};
use rand::RngExt;
use rand_xoshiro::Xoshiro256PlusPlus;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;
use tracing::warn;

#[derive(Debug, Clone, PartialEq)]
pub struct StorageUpdate {
    pub link_id: Id<Link>,
    pub from_part: u32,
    pub released: f64,
    /// Absolute tick when the sender's gap reaches QStart. Queue updates are immediate.
    pub available_at: Tick,
}

#[derive(Debug)]
struct ActiveCache<C: StableTypeId> {
    active: IntSet<Id<C>>,
}

impl<C: StableTypeId> Default for ActiveCache<C> {
    fn default() -> Self {
        ActiveCache {
            active: IntSet::default(),
        }
    }
}

impl<C: StableTypeId> From<IntSet<Id<C>>> for ActiveCache<C> {
    fn from(value: IntSet<Id<C>>) -> Self {
        ActiveCache { active: value }
    }
}

impl<C: StableTypeId + 'static> ActiveCache<C> {
    fn activate(&mut self, id: Id<C>) -> bool {
        self.active.insert(id)
    }

    fn deactivate(&mut self, id: &Id<C>) -> bool {
        self.active.remove(id)
    }

    fn len(&self) -> usize {
        self.active.len()
    }

    /// Visits the active ids in iteration order and deactivates those for which `f` returns false.
    fn retain(&mut self, f: impl FnMut(&Id<C>) -> bool) {
        self.active.retain(f)
    }

    #[cfg(test)]
    fn contains(&self, id: &Id<C>) -> bool {
        self.active.contains(id)
    }
}

impl<'a, C: StableTypeId + 'static> IntoIterator for &'a ActiveCache<C> {
    type Item = &'a Id<C>;
    type IntoIter = <&'a IntSet<Id<C>> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.active.iter()
    }
}

#[derive(Debug)]
pub struct SimNetworkPartition {
    pub nodes: IntMap<Id<Node>, SimNode>,
    // use int map as hash map variant with stable order
    pub links: IntMap<Id<Link>, SimLink>,
    active_nodes: ActiveCache<Node>,
    active_links: ActiveCache<Link>,
    // Links which hold vehicles but are not moved until the given tick, see `LocalLink::fall_asleep`.
    // Entries of links that were woken up earlier are outdated and skipped.
    sleeping_links: BTreeMap<Tick, Vec<Id<Link>>>,
    // Reused by every node update, so that collecting candidates does not allocate.
    candidates: Vec<Candidate>,
    veh_counter: usize,
    partition: u32,
    // See `find_neighbors`.
    neighbors: IntSet<u32>,
    clock: SimClock,
}

#[derive(Debug)]
#[allow(unused)]
pub struct SimNode {
    id: Id<Node>,
    in_links: Vec<Id<Link>>,
    // The random number sequence used in intersection updates only depends on the node.
    rng: Xoshiro256PlusPlus,
}

#[derive(Debug)]
struct Candidate {
    // index into the in links of the node
    in_link: usize,
    weight: f64,
}

#[derive(Debug, PartialEq)]
enum FrontDecision {
    NoVehicle,
    MoveNormally(Id<Link>),
    MoveAlthoughStuck(Id<Link>),
    Wait,
    Abort,
}

impl SimNetworkPartition {
    pub fn from_network(
        global_network: &Network,
        storage_capacities: &LinkStorageCapacities,
        partition: u32,
        config: &config::Config,
    ) -> Self {
        let qsim_config = config.qsim();
        let clock = SimClock::new(qsim_config.ticks_per_second);
        let nodes: Vec<&Node> = global_network
            .nodes()
            .iter()
            .filter(|n| n.partition == partition)
            .copied()
            .collect();

        let link_ids: Vec<_> = nodes
            .iter()
            .flat_map(|n| n.in_links.iter().chain(n.out_links.iter()))
            .collect(); // collect here to get each link id only once

        let sim_links: IntMap<_, _> = link_ids
            .iter()
            .map(|link_id| global_network.get_link(link_id))
            .map(|link| {
                (
                    link.id.clone(),
                    Self::create_sim_link(
                        link,
                        storage_capacities,
                        partition,
                        qsim_config,
                        global_network,
                    ),
                )
            })
            .collect();

        let base_seed = config.computational_setup().random_seed;
        let sim_nodes: IntMap<_, SimNode> = nodes
            .iter()
            .map(|n| (n.id.clone(), Self::create_sim_node(n, base_seed)))
            .collect();

        let neighbors = Self::find_neighbors(
            global_network,
            &link_ids,
            partition,
            &qsim_config.main_modes,
        );
        SimNetworkPartition::build(sim_nodes, sim_links, partition, neighbors, clock)
    }

    /// The partitions on the other side of the split links of this partition that can carry
    /// vehicles. A partition exchanges a message with each of them in every time step and waits
    /// for theirs.
    ///
    /// A split link that allows modes, but none of `main_modes`, carries no vehicles, e.g. a link
    /// of a transit network whose vehicles are teleported. It does not make the partitions on its
    /// two sides neighbors. Links without any mode, and all links if there are no main modes, do.
    /// If no split link of this partition can carry vehicles, all of them count, so that the
    /// partition still waits for others and reads their messages (e.g. teleported agents) during
    /// the simulation, see [`NetMessageBroker::take_unprocessed_messages`].
    ///
    /// [`NetMessageBroker::take_unprocessed_messages`]: crate::simulation::messaging::sim_communication::message_broker::NetMessageBroker::take_unprocessed_messages
    fn find_neighbors(
        global_network: &Network,
        link_ids: &[&Id<Link>],
        partition: u32,
        main_modes: &[String],
    ) -> IntSet<u32> {
        // Only look up the mode ids. A main mode without an id is allowed on no link.
        let main_modes: IntSet<Id<String>> = main_modes
            .iter()
            .filter_map(|mode| Id::try_get_from_ext(mode))
            .collect();
        let carries_vehicles = |link: &Link| {
            main_modes.is_empty()
                || link.modes.is_empty()
                || link.modes.iter().any(|mode| main_modes.contains(mode))
        };
        let neighbor = |link: &Link| {
            let from_part = global_network.get_node(&link.from).partition;
            let to_part = global_network.get_node(&link.to).partition;
            match (from_part == partition, to_part == partition) {
                (true, false) => Some(to_part),
                (false, true) => Some(from_part),
                _ => None,
            }
        };

        let split_links: Vec<&Link> = link_ids
            .iter()
            .map(|id| global_network.get_link(id))
            .filter(|link| neighbor(link).is_some())
            .collect();
        let neighbors: IntSet<u32> = split_links
            .iter()
            .filter(|link| carries_vehicles(link))
            .filter_map(|link| neighbor(link))
            .collect();
        if neighbors.is_empty() {
            split_links
                .iter()
                .filter_map(|link| neighbor(link))
                .collect()
        } else {
            neighbors
        }
    }

    #[cfg(test)]
    pub(crate) fn from_network_for_test(
        global_network: &Network,
        partition: u32,
        config: &config::Config,
    ) -> Self {
        let storage_capacities = LinkStorageCapacities::from_network(global_network, config.qsim());
        Self::from_network(global_network, &storage_capacities, partition, config)
    }

    pub(crate) fn drain(&mut self) -> Vec<LegAgent> {
        self.links
            .values_mut()
            .flat_map(|link| link.drain())
            .flat_map(SimulationVehicle::into_agents)
            .collect()
    }

    fn create_sim_node(node: &Node, base_seed: u64) -> SimNode {
        let mut in_links: Vec<_> = node.in_links.to_vec();
        in_links.sort_unstable_by(|a, b| a.external().cmp(b.external()));

        SimNode {
            id: node.id.clone(),
            in_links,
            // Initialize RNG with a seed based on the base seed and node id
            rng: random::get_rng(base_seed, "network.node", node.id.external().as_ref()),
        }
    }

    fn create_sim_link(
        link: &Link,
        storage_capacities: &LinkStorageCapacities,
        partition: u32,
        config: &config::QSim,
        global_network: &Network,
    ) -> SimLink {
        let from_part = global_network.get_node(&link.from).partition; //all_nodes.get(link.from.internal()).unwrap().partition;
        let to_part = global_network.get_node(&link.to).partition; //all_nodes.get(link.to.internal()).unwrap().partition;
        let storage_capacity = storage_capacities.get(&link.id);

        if from_part == to_part {
            SimLink::Local(LocalLink::from_link(link, storage_capacity, config))
        } else if to_part == partition {
            let local_link = LocalLink::from_link(link, storage_capacity, config);
            SimLink::In(SplitInLink::new(from_part, local_link))
        } else {
            SimLink::Out(SplitOutLink::new(link, storage_capacity, to_part))
        }
    }

    fn build(
        nodes: IntMap<Id<Node>, SimNode>,
        links: IntMap<Id<Link>, SimLink>,
        partition: u32,
        neighbors: IntSet<u32>,
        clock: SimClock,
    ) -> Self {
        Self {
            nodes,
            links,
            active_links: ActiveCache::<Link>::default(),
            active_nodes: ActiveCache::<Node>::default(),
            sleeping_links: BTreeMap::new(),
            candidates: Vec::new(),
            veh_counter: 0,
            partition,
            neighbors,
            clock,
        }
    }

    pub fn partition(&self) -> u32 {
        self.partition
    }

    /// The partitions this partition exchanges vehicles with, see [`Self::find_neighbors`].
    pub fn neighbors(&self) -> IntSet<u32> {
        self.neighbors.clone()
    }

    pub fn active_nodes(&self) -> usize {
        self.active_nodes.len()
    }

    pub fn active_links(&self) -> usize {
        self.active_links.len()
    }

    pub fn veh_on_net(&self) -> usize {
        self.veh_counter
    }

    pub fn get_link_ids(&self) -> HashSet<Id<Link>> {
        self.links
            .iter()
            .filter(|(_, link)| match link {
                SimLink::Local(_) => true,
                SimLink::In(_) => true,
                SimLink::Out(_) => false,
            })
            .map(|(id, _)| id.clone())
            .collect::<HashSet<_>>()
    }

    pub fn get_node_ids(&self) -> HashSet<Id<Node>> {
        self.nodes.keys().cloned().collect::<HashSet<_>>()
    }

    /// The event manager is only used to publish link enter events. There are two different cases:
    /// 1. The vehicle is received from another partition. The event manager should be Some(_) in order to publish the
    ///    link enter event.
    /// 2. The vehicle starts at this partition. Because its link enter is right after an activity,
    ///    the MATSim default is to not publish this link enter event. Therefore, the event manager should be None.
    pub fn send_veh_en_route(
        &mut self,
        vehicle: SimulationVehicle,
        events_manager: Option<Rc<RefCell<EventsManager>>>,
        now: impl Into<Tick>,
    ) {
        let now = now.into();
        let link_id = vehicle.curr_link_id().unwrap_or_else(|| {
            panic!("Vehicle is expected to have a current link id if it is sent onto the network")
        });
        let link = self.links.get_mut(link_id).unwrap_or_else(|| {
            let agent_id = vehicle.id();
            let coming_from_other_partition = events_manager.is_some();
            let where_is_it_from = if coming_from_other_partition {
                "Vehicle is already en route and comes from another partition."
            } else {
                "Vehicle was just sent en route. This is the first link."
            };
            panic!(
                "#{} Couldn't find link for id {:?}.for Agent {}. {} \n\n The vehicle: {:?}",
                self.partition,
                link_id,
                agent_id.external(),
                where_is_it_from,
                //self.global_network.get_link(&full_id),
                vehicle
            );
        });

        // If events_manager is None, this is the start of the route and the vehicle goes
        // into the waiting list. `fill_buffer` prioritizes draining waiting_list into buffer.
        let is_route_begin = events_manager.is_none();

        if let Some(manager) = events_manager {
            manager.borrow_mut().process_event(
                &LinkEnterEventBuilder::default()
                    .time(self.clock.tick_to_time(now))
                    .link(link.id().clone())
                    .vehicle(vehicle.id().clone())
                    .build()
                    .unwrap(),
            );
        }

        let pos = if is_route_begin { Waiting } else { QStart };
        link.push_veh(vehicle, pos, now);

        self.veh_counter += 1;

        // A vehicle at the end of the queue does not change when a sleeping link has to be moved.
        if !link.is_sleeping() {
            self.active_links.activate(link.id().clone());
        }
    }

    pub fn apply_storage_cap_updates(&mut self, storage_caps: Vec<StorageUpdate>) {
        for cap in storage_caps {
            if let SimLink::Out(link) = self.links.get_mut(&cap.link_id).unwrap() {
                link.apply_storage_cap_update(cap.released, cap.available_at);
            } else {
                panic!("only expecting ids for split out links ")
            }
        }
    }

    pub fn move_links(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        now: impl Into<Tick>,
    ) -> MoveAllLinksResult {
        let now = now.into();
        self.wake_up_links(now);

        let mut storage_cap_updates: Vec<_> = Vec::new();
        let mut vehicles_exit_partition: Vec<_> = Vec::new();
        let mut deactivate: IntSet<_> = IntSet::default();

        let mut vehicles_end_leg = vec![];
        for id in &self.active_links {
            let link = self.links.get_mut(id).unwrap();
            let mut res = match link {
                SimLink::Local(ll) => {
                    Self::move_local_link(ll, &mut self.active_nodes, now, comp_env)
                }
                SimLink::In(il) => Self::move_in_link(
                    il,
                    &mut self.active_nodes,
                    &mut storage_cap_updates,
                    now,
                    comp_env,
                ),
                SimLink::Out(ol) => Self::move_out_link(ol, &mut vehicles_exit_partition),
            };

            if let Some(wake_up) = res.sleeps_until {
                self.sleeping_links
                    .entry(wake_up)
                    .or_default()
                    .push(link.id().clone());
            }
            if !res.is_active || res.sleeps_until.is_some() {
                deactivate.insert(link.id().clone());
            }

            vehicles_end_leg.append(&mut res.vehicles_end_leg);
        }

        // bookkeeping. Empty links are no longer active.
        for id in deactivate {
            self.active_links.deactivate(&id);
        }
        // The agents reach the activity engine in this order, so it must not depend on the order
        // of the active links. The sort is stable and keeps the order of the vehicles of one link.
        vehicles_end_leg.sort_by_key(|vehicle| vehicle.curr_link_id().unwrap().internal());
        // vehicles leaving this partition are no longer part of the veh count
        self.veh_counter -= vehicles_exit_partition.len();
        self.veh_counter -= vehicles_end_leg.len();

        MoveAllLinksResult {
            vehicles_exit_partition,
            vehicles_end_leg,
            storage_cap_updates,
        }
    }

    /// Activates the sleeping links which have to be moved from `now` on.
    fn wake_up_links(&mut self, now: Tick) {
        while let Some(entry) = self.sleeping_links.first_entry()
            && *entry.key() <= now
        {
            let wake_up = *entry.key();
            for id in entry.remove() {
                if self.links.get_mut(&id).unwrap().wake_up(wake_up) {
                    self.active_links.activate(id);
                }
            }
        }
    }

    fn move_local_link(
        link: &mut LocalLink,
        active_nodes: &mut ActiveCache<Node>,
        now: Tick,
        comp_env: &mut ThreadLocalComputationalEnvironment,
    ) -> MoveSingleLinkResult {
        let vehicles_end_leg = link.do_sim_step(now, comp_env);
        Self::finish_link_step(link, active_nodes, now, vehicles_end_leg)
    }

    fn finish_link_step(
        link: &mut LocalLink,
        active_nodes: &mut ActiveCache<Node>,
        now: Tick,
        vehicles_end_leg: Vec<SimulationVehicle>,
    ) -> MoveSingleLinkResult {
        if link.to_nodes_active() {
            active_nodes.activate(link.to.clone());
        }

        // indicate whether link is active. The link is active if it has vehicles on it.
        let is_active = link.is_active();
        let sleeps_until = link.fall_asleep(now);

        MoveSingleLinkResult {
            vehicles_end_leg,
            is_active,
            sleeps_until,
        }
    }

    fn move_in_link(
        link: &mut SplitInLink,
        active_nodes: &mut ActiveCache<Node>,
        storage_cap_updates: &mut Vec<StorageUpdate>,
        now: Tick,
        events: &mut ThreadLocalComputationalEnvironment,
    ) -> MoveSingleLinkResult {
        // if anything has changed on the link, we want to report the updated storage capacity to the
        // upstream partition.
        let link_id = link.local_link.id.clone();
        let from_part = link.from_part;

        // Register every new gap as capacity early, since the link might get empty and thus deactivated now.
        let vehicles_end_leg = link.do_sim_step(now, events, |release| {
            // Transfer every new gap before deactivation, even if the link is now empty.
            // Its arrival is handled lazily by the upstream mirror, not by moving this link.
            storage_cap_updates.push(StorageUpdate {
                link_id: link_id.clone(),
                from_part,
                released: release.pce,
                available_at: release.available_at,
            });
        });
        Self::finish_link_step(&mut link.local_link, active_nodes, now, vehicles_end_leg)
    }

    fn move_out_link(
        link: &mut SplitOutLink,
        vehicles: &mut Vec<SimulationVehicle>,
    ) -> MoveSingleLinkResult {
        let out_q = link.take_veh();
        for veh in out_q {
            vehicles.push(veh);
        }
        MoveSingleLinkResult::default()
    }

    pub fn move_nodes(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        now: impl Into<Tick>,
    ) {
        let now = now.into();
        // Borrow the fields separately, so that the active nodes can be updated in place without
        // copying their ids.
        let Self {
            nodes,
            links,
            active_nodes,
            active_links,
            candidates,
            clock,
            ..
        } = self;

        // `retain` visits the active nodes in iteration order and deactivates them in that order.
        // Moving a node never changes the set of active nodes.
        active_nodes.retain(|node_id| {
            let node = nodes.get_mut(node_id).unwrap();
            Self::move_node_capacity_priority(
                node,
                links,
                active_links,
                candidates,
                comp_env,
                *clock,
                now,
            )
        });
    }

    fn move_node_capacity_priority(
        node: &mut SimNode,
        links: &mut IntMap<Id<Link>, SimLink>,
        active_links: &mut ActiveCache<Link>,
        candidates: &mut Vec<Candidate>,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        clock: SimClock,
        now: Tick,
    ) -> bool {
        let mut total_capacity = Self::get_candidates(&node.in_links, links, candidates);

        while !candidates.is_empty() && total_capacity > 1e-10 {
            let rnd_num = node.rng.random::<f64>() * total_capacity;
            let selected_index = Self::weighted_index(candidates, rnd_num);

            // We are using swap remove here on purpose. It has O(1) instead of O(n). Results are still deterministic, but the ordering is not preserved.
            // This doesn't have any effects on the probabilities, thus it is fine here. paul, jul'26.
            let selected = candidates.swap_remove(selected_index);
            total_capacity -= selected.weight;

            Self::drain_selected_inlink(
                &node.in_links[selected.in_link],
                links,
                active_links,
                comp_env,
                clock,
                now,
            );
        }

        // check whether any link is offering next timestep. Otherwise, the node can be de-activated
        Self::any_link_offers(&node.in_links, links)
    }

    /// Replaces the content of `candidates` with the offering in links in the order of `in_links`
    /// and returns their total capacity.
    fn get_candidates(
        in_links: &[Id<Link>],
        links: &IntMap<Id<Link>, SimLink>,
        candidates: &mut Vec<Candidate>,
    ) -> f64 {
        candidates.clear();
        let mut total_capacity = 0.;

        for (in_link, id) in in_links.iter().enumerate() {
            let link = links.get(id).unwrap();
            if link.offers_veh().is_some() {
                let weight = link.flow_cap();
                candidates.push(Candidate { in_link, weight });
                total_capacity += weight;
            }
        }

        total_capacity
    }

    fn weighted_index(candidates: &[Candidate], weighted_rnd_capacity: f64) -> usize {
        let mut selected_capacity = 0.;
        for (index, candidate) in candidates.iter().enumerate() {
            selected_capacity += candidate.weight;
            if selected_capacity >= weighted_rnd_capacity {
                return index;
            }
        }

        candidates.len() - 1
    }

    fn any_link_offers(link_ids: &[Id<Link>], links: &IntMap<Id<Link>, SimLink>) -> bool {
        link_ids
            .iter()
            .map(|id| links.get(id).unwrap())
            .any(|link| link.offers_veh().is_some())
    }

    fn evaluate_front_vehicle(
        in_id: &Id<Link>,
        links: &mut IntMap<Id<Link>, SimLink>,
        now: Tick,
    ) -> FrontDecision {
        let in_link = links.get(in_id).unwrap();
        let Some(vehicle) = in_link.offers_veh() else {
            return FrontDecision::NoVehicle;
        };
        let Some(next_id) = vehicle.peek_next_route_element() else {
            warn!(
                "Vehicle {} offered by link {} has no next route element.",
                vehicle.id().external(),
                in_link.id().external()
            );
            return FrontDecision::Abort;
        };
        let Some(out_link) = links.get(next_id) else {
            warn!(
                "Next link {} for vehicle {} offered by link {} is not present in this network partition.",
                next_id.external(),
                vehicle.id().external(),
                in_link.id().external()
            );
            return FrontDecision::Abort;
        };
        if in_link.to() != out_link.from() {
            warn!(
                "Next link {} for vehicle {} is not connected to in-link {}.",
                out_link.id().external(),
                vehicle.id().external(),
                in_link.id().external()
            );
            return FrontDecision::Abort;
        }
        let is_stuck = in_link.is_veh_stuck(now);
        let id = next_id.clone();
        if links.get_mut(&id).unwrap().update_and_is_available(now) {
            FrontDecision::MoveNormally(id)
        } else if is_stuck {
            FrontDecision::MoveAlthoughStuck(id)
        } else {
            FrontDecision::Wait
        }
    }

    fn drain_selected_inlink(
        in_link_id: &Id<Link>,
        links: &mut IntMap<Id<Link>, SimLink>,
        active_links: &mut ActiveCache<Link>,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        clock: SimClock,
        now: Tick,
    ) {
        loop {
            match Self::evaluate_front_vehicle(in_link_id, links, now) {
                FrontDecision::MoveNormally(next_link)
                | FrontDecision::MoveAlthoughStuck(next_link) => {
                    let in_link = links.get_mut(in_link_id).unwrap();
                    let vehicle = in_link
                        .pop_veh_and_restart_stuck_timer(now)
                        .expect("No vehicle on selected link");
                    let in_link_active = in_link.is_active();
                    Self::move_vehicle(
                        vehicle,
                        in_link_id,
                        in_link_active,
                        next_link,
                        links,
                        active_links,
                        comp_env,
                        clock,
                        now,
                    );
                }
                FrontDecision::Abort => {
                    panic!("Invalid turn from in-link {}", in_link_id.external());
                }
                // The in link offered a vehicle when it was selected, and only this node removes
                // vehicles from its buffer. Thus, the in link is still active if no vehicle was
                // moved. Otherwise, `move_vehicle` has already deactivated it if necessary.
                FrontDecision::Wait | FrontDecision::NoVehicle => break,
            }
        }
    }

    /// Moves the vehicle from the current link to the next link.
    ///
    /// `old_link_active` tells whether the old link was still active after the vehicle was removed
    /// from it.
    #[allow(
        clippy::too_many_arguments,
        reason = "takes the partition fields separately, so that they can be borrowed independently"
    )]
    fn move_vehicle(
        mut vehicle: SimulationVehicle,
        old_link_id: &Id<Link>,
        old_link_active: bool,
        next_link: Id<Link>,
        links: &mut IntMap<Id<Link>, SimLink>,
        active_links: &mut ActiveCache<Link>,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        clock: SimClock,
        now: Tick,
    ) {
        debug_assert_eq!(Some(old_link_id), vehicle.curr_link_id());
        let now_time = clock.tick_to_time(now);

        comp_env.events_manager_borrow_mut().process_event(
            &LinkLeaveEventBuilder::default()
                .vehicle(vehicle.id().clone())
                .link(old_link_id.clone())
                .time(now_time)
                .build()
                .unwrap(),
        );
        // Reuse the destination whose connectivity/capacity was checked before popping the
        // vehicle. The driver is told which link it moves onto, so the network needs no second
        // phase/role projection to discover the same destination again.
        vehicle.move_over_node(comp_env, &next_link, now_time);
        // A link whose to node equals its from node can be the old and the new link at once. In
        // that case, the vehicle has just been pushed onto the old link, so it stays active.
        let old_link_stays_active = old_link_active || next_link == *old_link_id;
        let new_link = links.get_mut(&next_link).unwrap();

        // for out links, link enter event is published at receiving partition
        if let SimLink::Local(_) = new_link {
            comp_env.events_manager_borrow_mut().process_event(
                &LinkEnterEventBuilder::default()
                    .time(now_time)
                    .link(new_link.id().clone())
                    .vehicle(vehicle.id().clone())
                    .build()
                    .unwrap(),
            );
        }

        new_link.push_veh(vehicle, QStart, now);

        // activate new link, unless it sleeps. A vehicle at the end of the queue does not change
        // when a sleeping link has to be moved.
        if !new_link.is_sleeping() {
            active_links.activate(next_link);
        }

        // deactivate old link if it is not active anymore
        if !old_link_stays_active {
            active_links.deactivate(old_link_id);
        }
    }
}

pub struct MoveAllLinksResult {
    pub vehicles_exit_partition: Vec<SimulationVehicle>,
    pub vehicles_end_leg: Vec<SimulationVehicle>,
    pub storage_cap_updates: Vec<StorageUpdate>,
}

#[derive(Default)]
struct MoveSingleLinkResult {
    vehicles_end_leg: Vec<SimulationVehicle>,
    is_active: bool,
    // the link is not moved until this tick
    sleeps_until: Option<Tick>,
}

#[cfg(test)]
mod tests {
    use super::{Candidate, SimNetworkPartition};
    use crate::simulation::Identifiable;
    use crate::simulation::agents::{
        LegAgent, LegLogic, NetworkDriverAgent, NetworkDriverLogic, NextPhase,
        SimulationAgentLogic, TeleportationAgent,
    };
    use crate::simulation::config::{MetisOptions, PartitionMethod};
    use crate::simulation::controller::ThreadLocalComputationalEnvironment;
    use crate::simulation::events::{LinkEnterEvent, LinkLeaveEvent};
    use crate::simulation::id::Id;
    use crate::simulation::io::xml::events::XmlEventsWriter;
    use crate::simulation::network::link::LinkPosition::QStart;
    use crate::simulation::network::link::SimLink;
    use crate::simulation::network::link::SimLink::Local;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Network, Node};
    use crate::simulation::scenario::population::InternalPerson;
    use crate::simulation::scenario::vehicles::InternalVehicle;
    use crate::simulation::time::SimTime;
    use crate::simulation::vehicles::SimulationVehicle;
    use crate::test_utils;
    use assert_approx_eq::assert_approx_eq;
    use macros::deterministic_id_test;
    use nohash_hasher::IntSet;
    use rand::{RngExt, SeedableRng};
    use rand_xoshiro::Xoshiro256PlusPlus;
    use std::cell::RefCell;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    // simple events handler that just records the events it receives.
    struct TransitionEvents {
        link_enters: Rc<RefCell<Vec<(String, String)>>>,
        link_leaves: Rc<RefCell<Vec<(String, String)>>>,
    }

    impl TransitionEvents {
        fn register(&self, env: &mut ThreadLocalComputationalEnvironment) {
            let enters = self.link_enters.clone();
            env.events_manager_borrow_mut()
                .on::<LinkEnterEvent, _>(move |event| {
                    enters.borrow_mut().push((
                        event.link.external().to_owned(),
                        event.vehicle.external().to_owned(),
                    ));
                });

            let leaves = self.link_leaves.clone();
            env.events_manager_borrow_mut()
                .on::<LinkLeaveEvent, _>(move |event| {
                    leaves.borrow_mut().push((
                        event.link.external().to_owned(),
                        event.vehicle.external().to_owned(),
                    ));
                });
        }

        fn leaving_vehicles(&self) -> Vec<String> {
            self.link_leaves
                .borrow()
                .iter()
                .map(|(_, vehicle)| vehicle.clone())
                .collect()
        }

        fn leaves_on(&self, link: &str) -> Vec<String> {
            self.link_leaves
                .borrow()
                .iter()
                .filter(|(event_link, _)| event_link == link)
                .map(|(_, vehicle)| vehicle.clone())
                .collect()
        }
    }

    fn environment_with_transition_events()
    -> (ThreadLocalComputationalEnvironment, TransitionEvents) {
        let mut env = ThreadLocalComputationalEnvironment::default();
        let events = TransitionEvents::default();
        events.register(&mut env);
        (env, events)
    }

    fn add_test_nodes(network: &mut Network, ids: &[&str]) {
        for id in ids {
            network.add_node(Node::new(Id::create(id), Coordinate::default(), 0, 1));
        }
    }

    fn add_test_link(
        network: &mut Network,
        id: &str,
        from: &str,
        to: &str,
        length: f64,
        capacity: f64,
        freespeed: f64,
    ) {
        network.add_link(Link {
            id: Id::create(id),
            from: Id::create(from),
            to: Id::create(to),
            length,
            capacity,
            freespeed,
            permlanes: 1.0,
            modes: Default::default(),
            partition: 0,
            attributes: Default::default(),
        });
    }

    fn test_vehicle(id: u64, route: Vec<&str>) -> SimulationVehicle {
        SimulationVehicle::from_parts(id, 0, 100.0, 1.0, test_utils::create_agent(id, route))
    }

    /// A driver without plan logic that records the links the network moves it onto.
    struct RecordingDriver {
        delegate: NetworkDriverAgent,
        route: Vec<Id<Link>>,
        position: usize,
        moved_onto: Arc<Mutex<Vec<String>>>,
        // When the driver moves onto the first link, it continues on the second instead of its route.
        diversion: Option<(Id<Link>, Id<Link>)>,
    }

    impl Identifiable<InternalPerson> for RecordingDriver {
        fn id(&self) -> &Id<InternalPerson> {
            self.delegate.id()
        }
    }

    impl SimulationAgentLogic for RecordingDriver {
        fn into_person(self: Box<Self>) -> Option<InternalPerson> {
            self.delegate.into_person()
        }
    }

    impl LegLogic for RecordingDriver {
        fn mode(&self) -> &Id<String> {
            self.delegate.mode()
        }
        fn routing_mode(&self) -> Option<&Id<String>> {
            self.delegate.routing_mode()
        }
        fn departure_link(&self) -> &Id<Link> {
            &self.route[0]
        }
        fn curr_link_id(&self) -> Option<&Id<Link>> {
            self.route.get(self.position)
        }
        fn try_into_network(self: Box<Self>) -> Result<NetworkDriverAgent, LegAgent> {
            Ok(self)
        }
        fn try_into_teleportation(self: Box<Self>) -> Result<TeleportationAgent, LegAgent> {
            Err(self)
        }
        fn finish_leg(self: Box<Self>, now: SimTime) -> NextPhase {
            self.delegate.finish_leg(now)
        }
    }

    impl NetworkDriverLogic for RecordingDriver {
        fn planned_vehicle_id(&self) -> Option<&Id<InternalVehicle>> {
            None
        }
        fn peek_next_link_id(&self) -> Option<&Id<Link>> {
            self.route.get(self.position + 1)
        }
        fn is_wanting_to_arrive_on_current_link(&self) -> bool {
            self.peek_next_link_id().is_none()
        }
        fn notify_move_over_node(
            &mut self,
            _comp_env: &mut ThreadLocalComputationalEnvironment,
            next_link: &Id<Link>,
            _now: SimTime,
        ) {
            self.moved_onto
                .lock()
                .unwrap()
                .push(next_link.external().to_owned());
            self.position += 1;
            if let Some((link, instead)) = &self.diversion
                && link == next_link
            {
                self.route.truncate(self.position + 1);
                self.route.push(instead.clone());
            }
        }
    }

    #[deterministic_id_test]
    fn movement_tells_the_driver_the_checked_destination() {
        let mut global_network = Network::new();
        add_test_nodes(&mut global_network, &["S", "K", "TX", "TY"]);
        add_test_link(&mut global_network, "A", "S", "K", 1.0, 3600.0, 100.0);
        add_test_link(&mut global_network, "X", "K", "TX", 75.0, 3600.0, 100.0);
        add_test_link(&mut global_network, "Y", "K", "TY", 75.0, 3600.0, 100.0);
        let mut network =
            SimNetworkPartition::from_network_for_test(&global_network, 0, &test_utils::config());
        let moved_onto = Arc::new(Mutex::new(Vec::new()));
        let driver = RecordingDriver {
            delegate: test_utils::create_agent(1, vec!["A", "X"]),
            route: vec![Id::get_from_ext("A"), Id::get_from_ext("X")],
            position: 0,
            moved_onto: moved_onto.clone(),
            diversion: None,
        };
        let vehicle = SimulationVehicle::from_parts(1, 0, 100.0, 1.0, Box::new(driver));
        network.send_veh_en_route(vehicle, None, 0);
        let (mut env, events) = environment_with_transition_events();
        network.move_links(&mut env, 0);
        network.move_nodes(&mut env, 1);

        assert_eq!(events.leaves_on("A"), vec!["1"]);
        assert_eq!(
            &*events.link_enters.borrow(),
            &[("X".to_string(), "1".to_string())]
        );
        assert_eq!(local_vehicle_count(&network, "X"), 1);
        assert_eq!(local_vehicle_count(&network, "Y"), 0);
        assert_eq!(*moved_onto.lock().unwrap(), vec!["X"]);
    }

    /// A network S -A-> K -B-> M, where the links X and Y leave M. The destination of Y lies on
    /// `partition_of_y`.
    fn network_with_a_choice_at_m(partition_of_y: u32) -> Network {
        let mut network = Network::new();
        add_test_nodes(&mut network, &["S", "K", "M", "TX"]);
        let mut to_y = Node::new(Id::create("TY"), Coordinate::default(), 0, 1);
        to_y.partition = partition_of_y;
        network.add_node(to_y);
        add_test_link(&mut network, "A", "S", "K", 1.0, 3600.0, 100.0);
        add_test_link(&mut network, "B", "K", "M", 1.0, 3600.0, 100.0);
        add_test_link(&mut network, "X", "M", "TX", 1.0, 3600.0, 100.0);
        network.add_link(Link {
            id: Id::create("Y"),
            from: Id::create("M"),
            to: Id::create("TY"),
            length: 1.0,
            capacity: 3600.0,
            freespeed: 100.0,
            permlanes: 1.0,
            modes: Default::default(),
            partition: partition_of_y,
            attributes: Default::default(),
        });
        network
    }

    /// A vehicle on the route A, B, X whose driver changes to Y when it moves onto B.
    fn vehicle_diverting_at_b(moved_onto: Arc<Mutex<Vec<String>>>) -> SimulationVehicle {
        let driver = RecordingDriver {
            delegate: test_utils::create_agent(1, vec!["A", "B", "X"]),
            route: ["A", "B", "X"].into_iter().map(Id::get_from_ext).collect(),
            position: 0,
            moved_onto,
            diversion: Some((Id::get_from_ext("B"), Id::get_from_ext("Y"))),
        };
        SimulationVehicle::from_parts(1, 0, 100.0, 1.0, Box::new(driver))
    }

    #[deterministic_id_test]
    fn driver_can_change_its_route_at_a_node() {
        let global_network = network_with_a_choice_at_m(0);
        let mut network =
            SimNetworkPartition::from_network_for_test(&global_network, 0, &test_utils::config());
        let moved_onto = Arc::new(Mutex::new(Vec::new()));
        network.send_veh_en_route(vehicle_diverting_at_b(moved_onto.clone()), None, 0);
        let mut env = Default::default();

        let mut arrived = Vec::new();
        for now in 0..10 {
            network.move_nodes(&mut env, now);
            arrived.extend(network.move_links(&mut env, now).vehicles_end_leg);
        }

        assert_eq!(*moved_onto.lock().unwrap(), vec!["B", "Y"]);
        assert_eq!(arrived.len(), 1);
        assert_eq!(arrived[0].curr_link_id().map(Id::external), Some("Y"));
    }

    #[deterministic_id_test]
    fn driver_can_change_its_route_towards_another_partition() {
        let global_network = network_with_a_choice_at_m(1);
        let config = test_utils::config();
        let mut first = SimNetworkPartition::from_network_for_test(&global_network, 0, &config);
        let mut second = SimNetworkPartition::from_network_for_test(&global_network, 1, &config);
        let moved_onto = Arc::new(Mutex::new(Vec::new()));
        first.send_veh_en_route(vehicle_diverting_at_b(moved_onto.clone()), None, 0);
        let mut env: ThreadLocalComputationalEnvironment = Default::default();

        let mut arrived = Vec::new();
        for now in 0..10 {
            first.move_nodes(&mut env, now);
            let exited = first.move_links(&mut env, now).vehicles_exit_partition;
            for vehicle in exited {
                assert_eq!(vehicle.curr_link_id().map(Id::external), Some("Y"));
                second.send_veh_en_route(vehicle, Some(env.events_manager()), now);
            }
            second.move_nodes(&mut env, now);
            arrived.extend(second.move_links(&mut env, now).vehicles_end_leg);
        }

        assert_eq!(*moved_onto.lock().unwrap(), vec!["B", "Y"]);
        assert_eq!(arrived.len(), 1);
    }

    fn push_vehicle_to_queue(
        network: &mut SimNetworkPartition,
        link: &str,
        id: u64,
        route: Vec<&str>,
        now: u64,
    ) {
        network.links.get_mut(&Id::create(link)).unwrap().push_veh(
            test_vehicle(id, route),
            QStart,
            now,
        );
    }

    fn local_vehicle_count(network: &SimNetworkPartition, link: &str) -> usize {
        match network.links.get(&Id::create(link)).unwrap() {
            Local(link) => link.veh_count(),
            _ => panic!("Expected {link} to be a local link"),
        }
    }

    fn sleeping_link_count(network: &SimNetworkPartition) -> usize {
        network
            .links
            .values()
            .filter(|link| link.is_sleeping())
            .count()
    }

    fn is_sleeping(network: &SimNetworkPartition, link: &str) -> bool {
        network.links.get(&Id::create(link)).unwrap().is_sleeping()
    }

    fn is_active(network: &SimNetworkPartition, link: &str) -> bool {
        network.active_links.contains(&Id::create(link))
    }

    /// A takes one tick and X takes ten ticks, both with a flow capacity of 1 veh per tick.
    fn network_with_slow_link() -> SimNetworkPartition {
        let mut global_network = Network::new();
        add_test_nodes(&mut global_network, &["S", "K", "T"]);
        add_test_link(&mut global_network, "A", "S", "K", 10.0, 3600.0, 10.0);
        add_test_link(&mut global_network, "X", "K", "T", 100.0, 3600.0, 10.0);
        SimNetworkPartition::from_network_for_test(&global_network, 0, &test_utils::config())
    }

    /// Moves the network for the given ticks and returns the vehicles that end their leg, with
    /// the tick.
    fn move_network(
        network: &mut SimNetworkPartition,
        env: &mut ThreadLocalComputationalEnvironment,
        ticks: std::ops::RangeInclusive<u64>,
    ) -> Vec<(u64, String)> {
        let mut ended = vec![];
        for now in ticks {
            network.move_nodes(env, now);
            for vehicle in network.move_links(env, now).vehicles_end_leg {
                ended.push((now, vehicle.id().external().to_owned()));
            }
        }
        ended
    }

    fn set_node_rng(network: &mut SimNetworkPartition, node: &str, seed: u64) {
        network.nodes.get_mut(&Id::create(node)).unwrap().rng =
            Xoshiro256PlusPlus::seed_from_u64(seed);
    }

    /// Setting: A offers A1 with capacity 1, while B offers B1/B2 with capacity 2; the fixed seed selects B first.
    /// Execution: A single node transition drains the selected B buffer before selecting and processing A.
    /// Expectation: The exact LinkLeave order is B1, B2, A1 (vehicle IDs 21, 22, 11).
    #[deterministic_id_test]
    fn selected_inlink_buffer_is_drained_before_next_selection() {
        let mut global_network = Network::new();
        add_test_nodes(&mut global_network, &["SA", "SB", "K", "TX", "TY"]);
        add_test_link(&mut global_network, "A", "SA", "K", 1.0, 3600.0, 100.0);
        add_test_link(&mut global_network, "B", "SB", "K", 1.0, 7200.0, 100.0);
        add_test_link(&mut global_network, "X", "K", "TX", 75.0, 3600.0, 100.0);
        add_test_link(&mut global_network, "Y", "K", "TY", 75.0, 3600.0, 100.0);

        let mut network =
            SimNetworkPartition::from_network_for_test(&global_network, 0, &test_utils::config());
        set_node_rng(&mut network, "K", 4712);
        let node_id = Id::create("K");
        let first_draw = network
            .nodes
            .get(&node_id)
            .unwrap()
            .rng
            .clone()
            .random::<f64>();
        assert!(
            first_draw > 1.0 / 3.0,
            "The fixed seed must initially select B, draw was {first_draw}"
        );

        network.send_veh_en_route(test_vehicle(11, vec!["A", "X"]), None, 0);
        network.send_veh_en_route(test_vehicle(21, vec!["B", "Y"]), None, 0);
        network.send_veh_en_route(test_vehicle(22, vec!["B", "Y"]), None, 0);

        let (mut env, events) = environment_with_transition_events();
        network.move_links(&mut env, 0);
        network.move_nodes(&mut env, 1);

        assert_eq!(vec!["21", "22", "11"], events.leaving_vehicles());
    }

    /// Setting: Vehicle 1 enters the slow link X at tick 1 and is due at tick 11. Vehicle 2
    /// enters X at tick 3 and is due at tick 13.
    /// Execution: The network is moved tick by tick.
    /// Expectation: X sleeps until its first vehicle is due and is not woken up by vehicle 2
    /// entering it. Both vehicles end their leg when they are due, as if X was moved every tick.
    #[deterministic_id_test]
    fn idle_link_sleeps_until_first_vehicle_is_due() {
        let mut network = network_with_slow_link();
        let mut env = ThreadLocalComputationalEnvironment::default();
        network.send_veh_en_route(test_vehicle(1, vec!["A", "X"]), None, 0);

        assert!(move_network(&mut network, &mut env, 0..=1).is_empty());
        assert!(is_sleeping(&network, "X"));
        assert!(!is_active(&network, "X"));

        network.send_veh_en_route(test_vehicle(2, vec!["A", "X"]), None, 2);
        assert!(move_network(&mut network, &mut env, 2..=2).is_empty());
        // node K moves vehicle 2 onto X
        network.move_nodes(&mut env, 3);
        assert_eq!(2, local_vehicle_count(&network, "X"));
        assert!(is_sleeping(&network, "X"));
        assert!(!is_active(&network, "X"));
        assert!(network.move_links(&mut env, 3).vehicles_end_leg.is_empty());

        let ended = move_network(&mut network, &mut env, 4..=20);
        assert_eq!(vec![(11, "1".to_owned()), (13, "2".to_owned())], ended);
        assert_eq!(0, sleeping_link_count(&network));
        assert_eq!(0, network.active_links());
        assert_eq!(0, network.veh_on_net());
    }

    /// Setting: X sleeps until tick 11. At tick 5, a vehicle departs on X and ends its leg there.
    /// Execution: The network is moved tick by tick.
    /// Expectation: The departure wakes X up, so that the departing vehicle ends its leg at tick
    /// 5. Afterwards, X sleeps again until its first vehicle is due at tick 11.
    #[deterministic_id_test]
    fn departure_wakes_sleeping_link() {
        let mut network = network_with_slow_link();
        let mut env = ThreadLocalComputationalEnvironment::default();
        network.send_veh_en_route(test_vehicle(1, vec!["A", "X"]), None, 0);
        assert!(move_network(&mut network, &mut env, 0..=4).is_empty());
        assert!(is_sleeping(&network, "X"));

        network.send_veh_en_route(test_vehicle(2, vec!["X"]), None, 5);
        assert!(!is_sleeping(&network, "X"));
        assert!(is_active(&network, "X"));

        let ended = move_network(&mut network, &mut env, 5..=5);
        assert_eq!(vec![(5, "2".to_owned())], ended);
        assert!(is_sleeping(&network, "X"));

        let ended = move_network(&mut network, &mut env, 6..=20);
        assert_eq!(vec![(11, "1".to_owned())], ended);
    }

    /// Setting: L3 and L4 are the fourth and fifth link, so that a set of these two link ids
    /// iterates L4 before L3. A vehicle departs and ends its leg on each of them at tick 0.
    /// Execution: One link update.
    /// Expectation: The vehicles that end their leg are ordered by link id, not by the order of
    /// the active links.
    #[deterministic_id_test]
    fn vehicles_that_end_leg_are_ordered_by_link() {
        let mut global_network = Network::new();
        add_test_nodes(&mut global_network, &["S", "T"]);
        for link in ["L0", "L1", "L2", "L3", "L4"] {
            add_test_link(&mut global_network, link, "S", "T", 100.0, 3600.0, 10.0);
        }
        let mut network =
            SimNetworkPartition::from_network_for_test(&global_network, 0, &test_utils::config());
        network.send_veh_en_route(test_vehicle(4, vec!["L4"]), None, 0);
        network.send_veh_en_route(test_vehicle(3, vec!["L3"]), None, 0);
        let active: Vec<_> = network
            .active_links
            .into_iter()
            .map(|id| id.external().to_owned())
            .collect();
        assert_eq!(vec!["L4", "L3"], active, "precondition of this test");

        let ended: Vec<_> = network
            .move_links(&mut ThreadLocalComputationalEnvironment::default(), 0)
            .vehicles_end_leg
            .iter()
            .map(|vehicle| vehicle.curr_link_id().unwrap().external().to_owned())
            .collect();

        assert_eq!(vec!["L3", "L4"], ended);
    }

    /// Setting: L is a self-loop at node K, and the vehicle's route passes L twice before leaving via X.
    /// Execution: The node moves the vehicle from L onto L again, which empties and refills the same link within one node update.
    /// Expectation: L is active exactly while it holds the vehicle, the vehicle leaves L twice, and its leg ends on X.
    #[deterministic_id_test]
    fn self_loop_link_stays_active_when_vehicle_reenters_it() {
        let mut global_network = Network::new();
        add_test_nodes(&mut global_network, &["K", "T"]);
        add_test_link(&mut global_network, "L", "K", "K", 10.0, 3600.0, 10.0);
        add_test_link(&mut global_network, "X", "K", "T", 10.0, 3600.0, 10.0);

        let mut network =
            SimNetworkPartition::from_network_for_test(&global_network, 0, &test_utils::config());
        network.send_veh_en_route(test_vehicle(1, vec!["L", "L", "X"]), None, 0);

        let (mut env, events) = environment_with_transition_events();
        let link_l = Id::create("L");
        let mut vehicles_end_leg = vec![];
        for now in 0..20 {
            network.move_nodes(&mut env, now);
            let mut result = network.move_links(&mut env, now);
            vehicles_end_leg.append(&mut result.vehicles_end_leg);
            let link_is_moved = network.active_links.contains(&link_l)
                || network.links.get(&link_l).unwrap().is_sleeping();
            assert_eq!(
                local_vehicle_count(&network, "L") > 0,
                link_is_moved,
                "tick {now}"
            );
        }

        assert_eq!(vec!["1", "1"], events.leaves_on("L"));
        assert_eq!(1, vehicles_end_leg.len());
        assert_eq!(0, network.veh_on_net());
    }

    /// Setting: A has weight 1, B has weight 2, both remain supplied, and C has room for exactly two vehicles per tick.
    /// Execution: 10,000 node transitions select each candidate at most once and drain the selected buffer until C is full.
    /// Expectation: Total throughput is exactly two vehicles per tick, with approximately one third from A and five thirds from B per tick.
    #[deterministic_id_test]
    fn merge_throughput_follows_buffer_weighted_selection() {
        let mut global_network = Network::new();
        add_test_nodes(&mut global_network, &["SA", "SB", "K", "T", "TT"]);
        add_test_link(&mut global_network, "A", "SA", "K", 1.0, 3600.0, 100.0);
        add_test_link(&mut global_network, "B", "SB", "K", 1.0, 7200.0, 100.0);
        add_test_link(&mut global_network, "C", "K", "T", 30.0, 7200.0, 100.0);
        add_test_link(&mut global_network, "D", "T", "TT", 30.0, 14_400.0, 100.0);

        let mut network =
            SimNetworkPartition::from_network_for_test(&global_network, 0, &test_utils::config());
        set_node_rng(&mut network, "K", 4712);
        for id in 1..=10_000 {
            network.send_veh_en_route(test_vehicle(id, vec!["A", "C", "D"]), None, 0);
        }
        for id in 10_001..=30_000 {
            network.send_veh_en_route(test_vehicle(id, vec!["B", "C", "D"]), None, 0);
        }

        let (mut env, events) = environment_with_transition_events();
        network.move_links(&mut env, 0);
        for now in 1..=10_000 {
            network.move_nodes(&mut env, now);
            network.move_links(&mut env, now);
        }

        let from_a = events.leaves_on("A").len();
        let from_b = events.leaves_on("B").len();
        assert_eq!(20_000, from_a + from_b);

        // assert 1/3 from A and 5/3 from B. There are the following cases:
        // (1) A is chosen first (p=1/3) => A releases 1 and B releases 1
        // (2) B is chosen first (p=2/3) => A releases 0 and B releases 2
        // => Expected value E(A)=1*1/3+0=1/3; E(B)=1/3*1+2/3*2=5/3
        assert!(from_a.abs_diff(10_000 / 3) <= 250, "A count was {from_a}");
        assert!(from_b.abs_diff(50_000 / 3) <= 250, "B count was {from_b}");
    }

    /// Setting: A1 wants to enter the already full link X, while B1 independently wants to enter the available link Y.
    /// Execution: Both incoming links offer a vehicle at the shared node at the same time.
    /// Expectation: A1 remains on A while B1 leaves B; the blocked turn does not block the entire node.
    #[deterministic_id_test]
    fn full_outlink_does_not_block_independent_turn() {
        let mut global_network = Network::new();
        add_test_nodes(&mut global_network, &["SA", "SB", "K", "TX", "TY", "END"]);
        add_test_link(&mut global_network, "A", "SA", "K", 1.0, 3600.0, 100.0);
        add_test_link(&mut global_network, "B", "SB", "K", 1.0, 3600.0, 100.0);
        add_test_link(&mut global_network, "X", "K", "TX", 7.5, 3600.0, 100.0);
        add_test_link(&mut global_network, "Y", "K", "TY", 7.5, 3600.0, 100.0);
        add_test_link(&mut global_network, "X2", "TX", "END", 7.5, 3600.0, 100.0);

        let mut network =
            SimNetworkPartition::from_network_for_test(&global_network, 0, &test_utils::config());
        push_vehicle_to_queue(&mut network, "X", 90, vec!["X", "X2"], 0);
        network.send_veh_en_route(test_vehicle(11, vec!["A", "X"]), None, 0);
        network.send_veh_en_route(test_vehicle(21, vec!["B", "Y"]), None, 0);

        let (mut env, events) = environment_with_transition_events();
        network.move_links(&mut env, 0);
        network.move_nodes(&mut env, 1);

        assert!(events.leaves_on("A").is_empty());
        assert_eq!(vec!["21"], events.leaves_on("B"));
        assert_eq!(1, local_vehicle_count(&network, "A"));
        assert_eq!(0, local_vehicle_count(&network, "B"));
    }

    /// Setting: A's FIFO buffer contains A1 targeting X at the front and A2 targeting Y behind it; X is full and Y is available.
    /// Execution: The node checks the front vehicle A1 but cannot move it onto X.
    /// Expectation: Neither A1 nor A2 leaves A; the blocked front vehicle blocks the entire incoming buffer for this tick.
    #[deterministic_id_test]
    fn blocked_front_vehicle_blocks_vehicles_behind_it() {
        let mut global_network = Network::new();
        add_test_nodes(&mut global_network, &["S", "K", "TX", "TY", "END"]);
        add_test_link(&mut global_network, "A", "S", "K", 1.0, 7200.0, 100.0);
        add_test_link(&mut global_network, "X", "K", "TX", 7.5, 3600.0, 100.0);
        add_test_link(&mut global_network, "Y", "K", "TY", 7.5, 3600.0, 100.0);
        add_test_link(&mut global_network, "X2", "TX", "END", 7.5, 3600.0, 100.0);

        let mut network =
            SimNetworkPartition::from_network_for_test(&global_network, 0, &test_utils::config());
        push_vehicle_to_queue(&mut network, "X", 90, vec!["X", "X2"], 0);
        network.send_veh_en_route(test_vehicle(11, vec!["A", "X"]), None, 0);
        network.send_veh_en_route(test_vehicle(12, vec!["A", "Y"]), None, 0);

        let (mut env, events) = environment_with_transition_events();
        network.move_links(&mut env, 0);
        network.move_nodes(&mut env, 1);

        assert!(events.leaves_on("A").is_empty());
        assert_eq!(2, local_vehicle_count(&network, "A"));
        assert_eq!(0, local_vehicle_count(&network, "Y"));
    }

    /// Setting: C has exactly one available slot, A offers A1 and A2, and the stuck threshold is ten ticks.
    /// Execution: A1 occupies C's last slot at tick 1, making A2 the blocked front vehicle from tick 1 onward; A2 is checked again at ticks 10 and 11.
    /// Expectation: A2 remains blocked at tick 10 after waiting nine ticks and leaves A exactly at tick 11 because the current implementation uses an inclusive `>=` comparison.
    #[deterministic_id_test]
    fn stuck_vehicle_moves_at_inclusive_threshold() {
        let mut global_network = Network::new();
        add_test_nodes(&mut global_network, &["S", "K", "T", "END"]);
        add_test_link(&mut global_network, "A", "S", "K", 1.0, 7200.0, 100.0);
        add_test_link(&mut global_network, "C", "K", "T", 15.0, 7200.0, 100.0);
        add_test_link(&mut global_network, "C2", "T", "END", 7.5, 3600.0, 100.0);

        let mut config = test_utils::config();
        config.qsim_mut().stuck_threshold = 10;
        let mut network = SimNetworkPartition::from_network_for_test(&global_network, 0, &config);
        push_vehicle_to_queue(&mut network, "C", 90, vec!["C", "C2"], 0);
        network.send_veh_en_route(test_vehicle(11, vec!["A", "C"]), None, 0);
        network.send_veh_en_route(test_vehicle(12, vec!["A", "C"]), None, 0);

        let (mut env, events) = environment_with_transition_events();
        network.move_links(&mut env, 0);
        network.move_nodes(&mut env, 1);
        assert_eq!(vec!["11"], events.leaves_on("A"));

        network.move_nodes(&mut env, 10);
        assert_eq!(vec!["11"], events.leaves_on("A"));

        network.move_nodes(&mut env, 11);
        assert_eq!(vec!["11", "12"], events.leaves_on("A"));
    }

    /// Setting: The slow link C is 100 m long, has a free speed of 1 m/s, a capacity of 3600 vehicles/h, and already contains 14 vehicles; U offers one additional vehicle for C.
    /// Execution: The prepared storage capacity is increased from about 13.33 to 100 vehicles based on the free-speed travel time.
    /// Expectation: C can accept U1, which leaves U and enters C.
    #[deterministic_id_test]
    fn slow_link_uses_freespeed_adjusted_storage_capacity() {
        let mut global_network = Network::new();
        add_test_nodes(&mut global_network, &["S", "K", "T", "END"]);
        add_test_link(&mut global_network, "U", "S", "K", 1.0, 3600.0, 100.0);
        add_test_link(&mut global_network, "C", "K", "T", 100.0, 3600.0, 1.0);
        add_test_link(&mut global_network, "C2", "T", "END", 7.5, 3600.0, 100.0);

        let mut network =
            SimNetworkPartition::from_network_for_test(&global_network, 0, &test_utils::config());
        for id in 100..114 {
            push_vehicle_to_queue(&mut network, "C", id, vec!["C", "C2"], 0);
        }
        network.send_veh_en_route(test_vehicle(1, vec!["U", "C"]), None, 0);

        let (mut env, events) = environment_with_transition_events();
        network.move_links(&mut env, 0);
        assert_eq!(
            14.0,
            network.links.get(&Id::create("C")).unwrap().used_storage()
        );
        assert!(
            network
                .links
                .get_mut(&Id::create("C"))
                .unwrap()
                .update_and_is_available(0)
        );

        network.move_nodes(&mut env, 1);
        assert_eq!(vec!["1"], events.leaves_on("U"));
        assert_eq!(0, local_vehicle_count(&network, "U"));
        assert_eq!(15, local_vehicle_count(&network, "C"));
    }

    /// Setting: A vehicle is waiting on A and names `missing`, a link that is not present in the local network, as its next route element.
    /// Execution: The selected A buffer logs the unknown destination and reaches the Abort decision.
    /// Expectation: Node processing panics before popping the vehicle or emitting a LinkLeave event.
    #[deterministic_id_test]
    fn missing_next_link_panics_before_vehicle_is_popped() {
        let mut global_network = Network::new();
        add_test_nodes(&mut global_network, &["S", "K"]);
        add_test_link(&mut global_network, "A", "S", "K", 1.0, 3600.0, 100.0);

        let mut network =
            SimNetworkPartition::from_network_for_test(&global_network, 0, &test_utils::config());
        network.send_veh_en_route(test_vehicle(1, vec!["A", "missing"]), None, 0);

        let (mut env, events) = environment_with_transition_events();
        network.move_links(&mut env, 0);
        let result = catch_unwind(AssertUnwindSafe(|| network.move_nodes(&mut env, 1)));

        assert!(result.is_err());
        assert!(events.leaves_on("A").is_empty());
        assert!(events.link_enters.borrow().is_empty());
        assert_eq!(1, local_vehicle_count(&network, "A"));
        assert_eq!(1, network.veh_on_net());
    }

    /// Setting: A ends at K1, while the next route link Z begins at the topologically disconnected node K2; both links belong to the same partition.
    /// Execution: The selected A buffer logs the disconnected destination and reaches the Abort decision.
    /// Expectation: Node processing panics before popping the vehicle or emitting transition events.
    #[deterministic_id_test]
    fn disconnected_next_link_panics_before_vehicle_is_popped() {
        let mut global_network = Network::new();
        add_test_nodes(&mut global_network, &["S", "K1", "K2", "T"]);
        add_test_link(&mut global_network, "A", "S", "K1", 1.0, 3600.0, 100.0);
        add_test_link(&mut global_network, "Z", "K2", "T", 75.0, 3600.0, 100.0);

        let mut network =
            SimNetworkPartition::from_network_for_test(&global_network, 0, &test_utils::config());
        network.send_veh_en_route(test_vehicle(1, vec!["A", "Z"]), None, 0);

        let (mut env, events) = environment_with_transition_events();
        network.move_links(&mut env, 0);
        let result = catch_unwind(AssertUnwindSafe(|| network.move_nodes(&mut env, 1)));

        assert!(result.is_err());
        assert!(events.leaves_on("A").is_empty());
        assert!(events.link_enters.borrow().is_empty());
        assert_eq!(1, local_vehicle_count(&network, "A"));
        assert_eq!(0, local_vehicle_count(&network, "Z"));
        assert_eq!(1, network.veh_on_net());
    }

    /// Setting: A is active with capacity 100 but does not yet offer a vehicle at tick 1; B and C each offer a vehicle with capacity 1, and D has only one available slot.
    /// Execution: Candidate collection excludes A, and the fixed seed selects C before B from the externally sorted candidate list.
    /// Expectation: The candidate capacity is 2, C moves first and fills D, and exactly two RNG draws are consumed.
    #[deterministic_id_test]
    fn non_offering_active_link_is_not_a_candidate() {
        let mut global_network = Network::new();
        add_test_nodes(&mut global_network, &["SA", "SB", "SC", "K", "TA", "T"]);
        add_test_link(&mut global_network, "A", "SA", "K", 100.0, 360000.0, 1.0);
        add_test_link(&mut global_network, "B", "SB", "K", 1.0, 3600.0, 100.0);
        add_test_link(&mut global_network, "C", "SC", "K", 1.0, 3600.0, 100.0);
        add_test_link(&mut global_network, "D", "K", "T", 7.5, 3600.0, 100.0);
        add_test_link(&mut global_network, "A2", "K", "TA", 7.5, 3600.0, 100.0);

        let mut network =
            SimNetworkPartition::from_network_for_test(&global_network, 0, &test_utils::config());
        set_node_rng(&mut network, "K", 4711);
        let slow_vehicle = SimulationVehicle::from_parts(
            1,
            0,
            1.0,
            1.0,
            test_utils::create_agent(1, vec!["A", "A2"]),
        );
        network
            .links
            .get_mut(&Id::create("A"))
            .unwrap()
            .push_veh(slow_vehicle, QStart, 0);
        network.active_links.activate(Id::create("A"));
        network.veh_counter += 1;
        network.send_veh_en_route(test_vehicle(2, vec!["B", "D"]), None, 0);
        network.send_veh_en_route(test_vehicle(3, vec!["C", "D"]), None, 0);

        let (mut env, events) = environment_with_transition_events();
        network.move_links(&mut env, 0);
        let node_id = Id::create("K");
        let node = network.nodes.get(&node_id).unwrap();
        let mut candidates = Vec::new();
        let capacity =
            SimNetworkPartition::get_candidates(&node.in_links, &network.links, &mut candidates);
        assert_eq!(
            vec!["B", "C"],
            candidates
                .iter()
                .map(|candidate| node.in_links[candidate.in_link].external().to_owned())
                .collect::<Vec<_>>()
        );
        assert_eq!(2.0, capacity);

        let mut expected_rng = node.rng.clone();
        let first_draw = expected_rng.random::<f64>();
        assert!(
            first_draw > 0.5,
            "The first draw must select C from equally weighted B and C: {first_draw}"
        );
        expected_rng.random::<f64>();
        let third_draw = expected_rng.random::<u64>();

        // move nodes calls 2 times the rng: choose B, move vehicle & mark it as exhausted; choose C, but D is blocked, so mark it as exhausted.
        network.move_nodes(&mut env, 1);
        let actual_third_draw = network.nodes.get_mut(&node_id).unwrap().rng.random::<u64>();

        assert_eq!(third_draw, actual_third_draw);
        assert_eq!(vec!["3"], events.leaves_on("C"));
        assert!(events.leaves_on("A").is_empty());
        assert!(events.leaves_on("B").is_empty());
    }

    /// Setting: The weighted candidate intervals are [0, 1] for A and (1, 3] for B.
    /// Execution: Selection is evaluated exactly at the cumulative boundary and once beyond the accumulated weight to exercise the rounding fallback.
    /// Expectation: The inclusive boundary selects A, while the fallback selects the final candidate B.
    #[deterministic_id_test]
    fn weighted_index_uses_inclusive_boundary_and_last_candidate_fallback() {
        let candidates = vec![
            Candidate {
                in_link: 0,
                weight: 1.0,
            },
            Candidate {
                in_link: 1,
                weight: 2.0,
            },
        ];

        assert_eq!(
            0,
            SimNetworkPartition::weighted_index(&candidates, 0.9999999)
        );
        assert_eq!(0, SimNetworkPartition::weighted_index(&candidates, 1.0));
        assert_eq!(
            1,
            SimNetworkPartition::weighted_index(&candidates, 1.0000001)
        );
        assert_eq!(1, SimNetworkPartition::weighted_index(&candidates, 3.1));
    }

    #[deterministic_id_test]
    fn from_network() {
        let mut network = Network::new();
        let mut sim_nets = create_three_node_sim_network_with_partition(&mut network);
        let net1 = sim_nets.get_mut(0).unwrap();

        // we expect two nodes
        assert_eq!(2, net1.nodes.len());
        // we expect two links one local and one out link
        assert_eq!(2, net1.links.len());
        let local_link = net1.links.get(&Id::create("link1")).unwrap();
        assert!(matches!(local_link, SimLink::Local(_)));
        let out_link = net1.links.get(&Id::create("link2")).unwrap();
        assert!(matches!(out_link, SimLink::Out(_)));

        let net2 = sim_nets.get_mut(1).unwrap();
        // we expect one node
        assert_eq!(1, net2.nodes.len());
        // we expect one in link
        assert_eq!(1, net2.links.len());
        let in_link = net2.links.get(&Id::create("link2")).unwrap();
        assert!(matches!(in_link, SimLink::In(_)));
    }

    #[deterministic_id_test]
    fn vehicle_travels_local() {
        let mut env = ThreadLocalComputationalEnvironment::default();
        let register = XmlEventsWriter::register_fn("test_output/test.xml");
        register(&mut env.events_manager_borrow_mut());

        let global_net = Network::from_file_with_partitioning(
            "./assets/3-links/3-links-network.xml",
            1,
            &PartitionMethod::Metis(MetisOptions::default()),
        );
        let mut network =
            SimNetworkPartition::from_network_for_test(&global_net, 0, &test_utils::config());
        let agent = test_utils::create_agent(1, vec!["link1", "link2", "link3"]);
        let vehicle = SimulationVehicle::from_parts(1, 0, 10., 1., agent);
        network.send_veh_en_route(vehicle, None, 0);

        for i in 0..113 {
            network.move_nodes(&mut env, i);
            let result = network.move_links(&mut Default::default(), i);

            // only in the timestep before the vehicle switches links, we should see one active node. Otherwise not.
            // leaves link1 at timestep 0 and enters link1; leaves link1 and enters link2 at timestep 101
            if i == 0 || i == 101 {
                assert_eq!(1, network.active_nodes(), "There was no active node at {i}");
                network.active_nodes.contains(&Id::create("node1"));
            } else {
                assert_eq!(0, network.active_nodes(), "There was an active node at {i}");
            }

            if i == 112 {
                assert!(!result.vehicles_end_leg.is_empty());
                let veh = result.vehicles_end_leg.first().unwrap();
                assert_eq!(&Id::create("link3"), veh.curr_link_id().unwrap());
            } else {
                // the vehicle should not leave the network until the 112th timestep
                assert_eq!(0, result.vehicles_end_leg.len());
                // we should always have one link which has the vehicle, either active or sleeping
                assert_eq!(1, network.active_links() + sleeping_link_count(&network));
                // we expect one vehicle
                assert_eq!(1, network.veh_on_net());
            }
        }

        // the network should be empty in the end
        assert_eq!(0, network.active_links());
        assert_eq!(0, network.active_nodes());
        assert_eq!(0, network.veh_on_net());
    }

    #[deterministic_id_test]
    fn vehicle_reaches_boundary() {
        let mut env = Default::default();
        let global_net = Network::from_file_with_partitioning(
            "./assets/3-links/3-links-network.xml",
            2,
            &PartitionMethod::None,
        );
        let mut network =
            SimNetworkPartition::from_network_for_test(&global_net, 0, &test_utils::config());
        let agent = test_utils::create_agent(1, vec!["link1", "link2", "link3"]);
        let vehicle = SimulationVehicle::from_parts(1, 0, 10., 100., agent);
        network.send_veh_en_route(vehicle, None, 0);

        for now in 0..20 {
            network.move_nodes(&mut env, now);

            let res = network.move_links(&mut env, now);
            assert_eq!(0, res.storage_cap_updates.len()); // we expect no out links here

            assert_eq!(0, res.vehicles_end_leg.len());

            // when the vehicle moves from link1 to link2, it will be placed on an out link.
            // the stored vehicles on out links should be collected during move links.
            if now == 1 {
                assert_eq!(1, res.vehicles_exit_partition.len());
            } else {
                assert!(
                    res.vehicles_exit_partition.is_empty(),
                    "There should be no vehicles on the out link at timestep {now}"
                );
            }
        }
    }

    #[deterministic_id_test]
    fn move_nodes_enter_exit_constraint() {
        let mut env = Default::default();
        let global_net = Network::from_file_with_partitioning(
            "./assets/3-links/3-links-network.xml",
            1,
            &PartitionMethod::Metis(MetisOptions::default()),
        );
        let mut network =
            SimNetworkPartition::from_network_for_test(&global_net, 0, &test_utils::config());

        // place 100 vehicles on first link
        for i in 0..100 {
            let agent = test_utils::create_agent(i, vec!["link1"]);
            let vehicle = SimulationVehicle::from_parts(i, 0, 10., 1., agent);
            network.send_veh_en_route(vehicle, None, 0);
        }

        // all vehicles only have to traverse link1. they enter and directly exit
        for now in 0..2 {
            network.move_nodes(&mut env, now);
            let res = network.move_links(&mut env, now);
            if now == 0 {
                assert_eq!(100, res.vehicles_end_leg.len());
            } else {
                assert_eq!(0, res.vehicles_end_leg.len());
            }
        }
    }

    /// Test that vehicles are released from out links in case there is no stuck timer.
    #[deterministic_id_test]
    fn move_nodes_storage_cap_constraint() {
        let mut env = ThreadLocalComputationalEnvironment::default();
        let mut global_net = Network::from_file_with_partitioning(
            "./assets/3-links/3-links-network.xml",
            1,
            &PartitionMethod::Metis(MetisOptions::default()),
        );
        global_net.set_effective_cell_size(10.);

        let id_1: Id<Link> = Id::get_from_ext("link1");
        let id_2: Id<Link> = Id::get_from_ext("link2");
        let id_3: Id<Link> = Id::get_from_ext("link3");
        let mut config = test_utils::config();
        config.qsim_mut().stuck_threshold = u32::MAX;
        let mut network = SimNetworkPartition::from_network_for_test(&global_net, 0, &config);

        // Place 10 vehicles on link1. They will be released every 10s because PCE is 10 and flow_cap is 1.
        // Since they are super slow, they will leave link2 after 1000s.
        // They will traverse link3 in 100 s, after that they will leave link3. Since link3 has a storage cap of 10 and
        // PCE of the vehicle is 10 only one vehicle per time can be present on link3.
        for i in 0..10 {
            let agent =
                test_utils::create_agent(i, vec![id_1.external(), id_2.external(), "link3"]);
            let vehicle = SimulationVehicle::from_parts(i, 0, 1., 10., agent);
            network.send_veh_en_route(vehicle, None, 0);
        }

        for now in 0..2012 {
            network.move_nodes(&mut env, now);
            network.move_links(&mut env, now);

            let link3_available = network
                .links
                .get_mut(&id_3)
                .unwrap()
                .update_and_is_available(now);
            let link1 = network.links.get(&id_1).unwrap();
            let link2 = network.links.get(&id_2).unwrap();

            // at 10, 20, 30, ... link1 offers a vehicle
            if now < 91 && (0..91).step_by(10).collect::<Vec<u32>>().contains(&now) {
                assert!(
                    link1.offers_veh().is_some(),
                    "No vehicle offered at timestep {now}"
                );
            } else {
                assert!(
                    link1.offers_veh().is_none(),
                    "Vehicle offered at timestep {now}"
                );
            }

            // From 1002, no vehicle if offered by link1
            if (1002..1911).contains(&now) {
                // once the last vehicle has moved, link1 has nothing to offer.
                assert!(link1.offers_veh().is_none());

                // veh0 reaches buffer at 1001 and is released immediately.
                // veh1 reaches buffer at 1011 and is released at 1102; flow cap is refilled after 10
                // veh2 reaches q end at 1021 and buffer at 1112 (because of flow cap refill from before) and is released at 1203; flow cap is refilled after 10
                // ...
                if (1011..=1102).contains(&now)
                    || (1112..=1203).contains(&now)
                    || (1213..=1304).contains(&now)
                    || (1314..=1405).contains(&now)
                    || (1415..=1506).contains(&now)
                    || (1516..=1607).contains(&now)
                    || (1617..=1708).contains(&now)
                    || (1718..=1809).contains(&now)
                    || (1819..=1910).contains(&now)
                {
                    assert!(
                        link2.offers_veh().is_some(),
                        "No vehicle offered at timestep {now}"
                    );
                    if !(now == 1102
                        || now == 1203
                        || now == 1304
                        || now == 1405
                        || now == 1506
                        || now == 1607
                        || now == 1708
                        || now == 1809
                        || now == 1910)
                    {
                        assert!(!link3_available, "Storage cap reached at timestep {now}");
                    }
                } else {
                    assert!(
                        link2.offers_veh().is_none(),
                        "Vehicle offered at timestep {now}"
                    );
                }
            }
        }
    }

    /// Tests that vehicles are released from out links when stuck timer is reached.
    #[deterministic_id_test]
    fn move_nodes_stuck_threshold() {
        let mut env = ThreadLocalComputationalEnvironment::default();
        XmlEventsWriter::register_fn("test_output/test.xml")(&mut env.events_manager_borrow_mut());
        let mut global_net = Network::from_file_with_partitioning(
            "./assets/3-links/3-links-network.xml",
            1,
            &PartitionMethod::Metis(MetisOptions::default()),
        );
        global_net.set_effective_cell_size(10.);

        let id_1: Id<Link> = Id::get_from_ext("link1");
        let id_2: Id<Link> = Id::get_from_ext("link2");
        let id_3: Id<Link> = Id::get_from_ext("link3");
        let mut config = test_utils::config();
        config.qsim_mut().stuck_threshold = 10;
        let mut network = SimNetworkPartition::from_network_for_test(&global_net, 0, &config);

        // Place 10 vehicles on link1. They will be released every 10s because PCE is 10 and flow_cap is 1.
        // Since they are super slow, they will leave link2 after 1000s.
        // They will traverse link3 in 100 s, after that they will leave link3. Since link3 has a storage cap of 10 and
        // PCE of the vehicle is 10 only one vehicle per time can be present on link3.
        // But, since we enabled the stuck timer, they will be put onto link3 after being stuck for 10s.
        for i in 0..10 {
            let agent =
                test_utils::create_agent(i, vec![id_1.external(), id_2.external(), "link3"]);
            let vehicle = SimulationVehicle::from_parts(i, 0, 1., 10., agent);
            network.send_veh_en_route(vehicle, None, 0);
        }

        for now in 0..3300 {
            network.move_nodes(&mut env, now);
            network.move_links(&mut env, now);

            let link3_available = network
                .links
                .get_mut(&id_3)
                .unwrap()
                .update_and_is_available(now);
            let link1 = network.links.get(&id_1).unwrap();
            let link2 = network.links.get(&id_2).unwrap();

            // at 10, 20, 30, ... link1 offers a vehicle
            if now < 91 && (0..91).step_by(10).collect::<Vec<u32>>().contains(&now) {
                assert!(
                    link1.offers_veh().is_some(),
                    "No vehicle offered at timestep {now}"
                );
            } else {
                assert!(
                    link1.offers_veh().is_none(),
                    "Vehicle offered at timestep {now}"
                );
            }

            // From 1002, no vehicle if offered by link1
            if (1002..1911).contains(&now) {
                // once the last vehicle has moved, link1 has nothing to offer.
                assert!(link1.offers_veh().is_none());

                // veh0 reaches buffer at 1001 and is released immediately.
                // veh1 reaches the buffer at 1011 and is released at 1021.
                // Each following vehicle enters the buffer after nine refill ticks and is released
                // ten ticks later at the inclusive stuck threshold.
                // ...
                if (1011..1021).contains(&now)
                    || (1030..1040).contains(&now)
                    || (1049..1059).contains(&now)
                    || (1068..1078).contains(&now)
                    || (1087..1097).contains(&now)
                    || (1106..1116).contains(&now)
                    || (1125..1135).contains(&now)
                    || (1144..1154).contains(&now)
                    || (1163..1173).contains(&now)
                {
                    assert!(
                        link2.offers_veh().is_some(),
                        "No vehicle offered at timestep {now}"
                    );
                    assert!(!link3_available, "Storage cap reached at timestep {now}");
                } else {
                    assert!(
                        link2.offers_veh().is_none(),
                        "Vehicle offered at timestep {now}"
                    );
                }
            }
        }
        env.events_manager_borrow_mut().finish();
    }

    /// Tests that move_node produces outcome as expected with different link loadings.
    #[deterministic_id_test]
    fn move_nodes_transition_logic() {
        let mut net = Network::new();
        let node1 = Node {
            coord: Coordinate::default(),
            id: Id::create("node1"),
            in_links: vec![],
            out_links: vec![],
            partition: 0,
            cmp_weight: 1,
        };
        let node2 = Node {
            id: Id::create("node2"),
            ..node1.clone()
        };
        let node3 = Node {
            id: Id::create("node3"),
            ..node1.clone()
        };
        let node4 = Node {
            id: Id::create("node4"),
            ..node1.clone()
        };
        let node5 = Node {
            id: Id::create("node5"),
            ..node1.clone()
        };
        net.add_node(node1);
        net.add_node(node2);
        net.add_node(node3);
        net.add_node(node4);
        net.add_node(node5);

        net.add_link(Link {
            id: Id::create("link1"),
            from: Id::create("node1"),
            to: Id::create("node3"),
            length: 1.0,
            capacity: 3600.,
            freespeed: 100.,
            permlanes: 1.0,
            modes: Default::default(),
            partition: 0,
            attributes: Default::default(),
        });
        net.add_link(Link {
            id: Id::create("link2"),
            from: Id::create("node2"),
            to: Id::create("node3"),
            length: 1.0,
            capacity: 7200.,
            freespeed: 100.0,
            permlanes: 1.0,
            modes: Default::default(),
            partition: 0,
            attributes: Default::default(),
        });
        net.add_link(Link {
            id: Id::create("link3"),
            from: Id::create("node3"),
            to: Id::create("node4"),
            length: 75.,
            capacity: 3600.,
            freespeed: 100.0,
            permlanes: 1.0,
            modes: Default::default(),
            partition: 0,
            attributes: Default::default(),
        });
        net.add_link(Link {
            id: Id::create("link4"),
            from: Id::create("node4"),
            to: Id::create("node5"),
            length: 75.,
            capacity: 3600.,
            freespeed: 100.0,
            permlanes: 1.0,
            modes: Default::default(),
            partition: 0,
            attributes: Default::default(),
        });
        let mut sim_net =
            SimNetworkPartition::from_network_for_test(&net, 0, &test_utils::config());

        // Place 1000 vehicles on link1. Flow cap: 1 veh/s
        for i in 0..1000 {
            let agent = test_utils::create_agent(i, vec!["link1", "link3", "link4"]);
            let vehicle = SimulationVehicle::from_parts(i, 0, 100., 1., agent);
            sim_net.send_veh_en_route(vehicle, None, 0);
        }

        // Place 1000 vehicles on link2. Flow cap: 2 veh/s
        for i in 1000..2000 {
            let agent = test_utils::create_agent(i, vec!["link2", "link3", "link4"]);
            let vehicle = SimulationVehicle::from_parts(i, 0, 100., 1., agent);
            sim_net.send_veh_en_route(vehicle, None, 0);
        }

        let mut env = ThreadLocalComputationalEnvironment::default();
        XmlEventsWriter::register_fn("test_output/test.xml")(&mut env.events_manager_borrow_mut());

        for now in 0..1000 {
            sim_net.move_nodes(&mut env, now);
            sim_net.move_links(&mut env, now);
            if let Local(l) = sim_net.links.get(&Id::create("link1")).unwrap() {
                println!("Time {}, link1 veh_count: {}", now, l.veh_count());
            }
            if let Local(l) = sim_net.links.get(&Id::create("link2")).unwrap() {
                println!("Time {}, link2 veh_count: {}", now, l.veh_count());
            }
        }

        let link1 = if let Local(l) = sim_net.links.get(&Id::create("link1")).unwrap() {
            l.veh_count()
        } else {
            unreachable!()
        };
        let link2 = if let Local(l) = sim_net.links.get(&Id::create("link2")).unwrap() {
            l.veh_count()
        } else {
            unreachable!()
        };

        // Not 1000 but 993 because at the beginning link3 is not saturated and its first
        // queue-to-buffer transition now observes the minimum travel tick.
        assert_eq!(link1 + link2, 993);

        // link1 has flow cap of 1 veh/s, link2 has flow cap of 2 veh/s.
        // Since all go from link1 and link2 to link3 (flow cap: 1 veh/s), there is only one vehicle per time step moved over the node.
        // This is why we expect link1 to have roughly twice the vehicles as link2.
        print!(
            "Link 1 vehicle count: {}; Link 2 vehicle count: {}",
            link1, link2
        );
        assert!(
            (link2 * 2).abs_diff(link1) <= 100,
            "values differ by more than 100"
        );
    }

    #[deterministic_id_test]
    fn storage_cap_over_boundaries() {
        // use programmed network here, to avoid instabilities with metis algorithm for small
        // network graphs
        let mut network = Network::new();
        let mut sim_nets = create_three_node_sim_network_with_partition(&mut network);
        let net2 = sim_nets.get_mut(1).unwrap();
        let mut env = Default::default();

        let split_link_id: Id<Link> = Id::get_from_ext("link2");
        let agent = test_utils::create_agent(1, vec![split_link_id.external()]);
        let vehicle = SimulationVehicle::from_parts(1, 0, 10., 100., agent);

        // Network is empty, so no storage cap updates should be collected
        let res = net2.move_links(&mut Default::default(), 0);
        assert!(res.storage_cap_updates.is_empty());

        // NOTE: We are using push_veh and manually set active and veh_counter in order to not use
        // send_veh_en_route. This is because send_veh_en_route would put the vehicle into the waiting list and not into the queue.
        // But for this test, we need it to be inserted into the queue directly.
        net2.links
            .get_mut(&Id::create("link2"))
            .unwrap()
            .push_veh(vehicle, QStart, 0);
        net2.active_links.activate(Id::create("link2"));
        net2.veh_counter = 1;

        // now, in the next time step, nothing has changed on the link. It should therefore not
        // report any storage capacities
        net2.move_nodes(&mut env, 0);
        let res = net2.move_links(&mut Default::default(), 0);
        assert!(res.storage_cap_updates.is_empty());

        // After 10 steps, the vehicle can leave. As this is the end of the route, it is directly removed from the link, no move_nodes is required.
        let res = net2.move_links(&mut Default::default(), 10);

        assert_eq!(1, res.storage_cap_updates.len());
        let storage_cap = res.storage_cap_updates.first().unwrap();
        assert_eq!(split_link_id, storage_cap.link_id);
        assert_approx_eq!(100., storage_cap.released, 0.00001);
    }

    #[deterministic_id_test]
    fn kinematic_split_sends_future_releases_before_the_empty_link_deactivates() {
        let mut global = Network::new();
        let _ = create_three_node_sim_network_with_partition(&mut global);
        let link_id = Id::get_from_ext("link2");
        global.get_link_mut(&link_id).length = 25.;
        let mut config = test_utils::config();
        config.qsim_mut().traffic_dynamics =
            crate::simulation::config::TrafficDynamics::KinematicWaves;
        let mut upstream = SimNetworkPartition::from_network_for_test(&global, 0, &config);
        let mut downstream = SimNetworkPartition::from_network_for_test(&global, 1, &config);
        assert_eq!(
            upstream.links[&link_id].max_storage(),
            downstream.links[&link_id].max_storage()
        );
        for (id, pce, now) in [(1, 1., 0), (2, 2.5, 1)] {
            let make_vehicle = || {
                SimulationVehicle::from_parts(
                    id,
                    0,
                    100.,
                    pce,
                    test_utils::create_agent(id, vec!["link2"]),
                )
            };
            upstream
                .links
                .get_mut(&link_id)
                .unwrap()
                .push_veh(make_vehicle(), QStart, now);
            downstream
                .links
                .get_mut(&link_id)
                .unwrap()
                .push_veh(make_vehicle(), QStart, now);
        }
        downstream.active_links.activate(link_id.clone());
        downstream.veh_counter = 2;
        let first = downstream.move_links(&mut Default::default(), 2);
        let second = downstream.move_links(&mut Default::default(), 3);
        assert_eq!(1, first.storage_cap_updates.len());
        assert_eq!(1, second.storage_cap_updates.len());
        assert_eq!(
            super::Tick::new(8),
            first.storage_cap_updates[0].available_at
        );
        assert_eq!(
            super::Tick::new(9),
            second.storage_cap_updates[0].available_at
        );
        assert_eq!(0, downstream.active_links());
        assert!(
            downstream
                .move_links(&mut Default::default(), 100)
                .storage_cap_updates
                .is_empty()
        );
        let updates = first
            .storage_cap_updates
            .into_iter()
            .chain(second.storage_cap_updates)
            .collect();
        upstream.apply_storage_cap_updates(updates);
        let mirror = upstream.links.get_mut(&link_id).unwrap();
        for (now, expected) in [(7, 3.5), (8, 2.5), (8, 2.5), (9, 0.), (100, 0.)] {
            let _ = mirror.update_and_is_available(now);
            assert_eq!(expected, mirror.used_storage());
        }
    }

    #[deterministic_id_test]
    fn neighbors() {
        let mut net = Network::new();
        let node = Node::new(Id::create("node-1"), Coordinate::default(), 0, 1);
        let node_1_1 = Node::new(Id::create("node-1-1"), Coordinate::default(), 1, 1);
        let node_1_2 = Node::new(Id::create("node-1-2"), Coordinate::default(), 1, 1);

        let node_2_1 = Node::new(Id::create("node-2-1"), Coordinate::default(), 2, 1);
        let node_3_1 = Node::new(Id::create("node-3-1"), Coordinate::default(), 3, 1);
        let node_4_1 = Node::new(Id::create("not-a-neighbor"), Coordinate::default(), 4, 1);

        // create in links from partitions 1 and 2. 2 incoming links from partition 1, one incoming from
        // partition 2
        let in_link_1_1 = Link::new_with_default(Id::create("in-link-1-1"), &node_1_1, &node);
        let in_link_1_2 = Link::new_with_default(Id::create("in-link-1-2"), &node_1_2, &node);
        let in_link_2_1 = Link::new_with_default(Id::create("in-link-2-1"), &node_2_1, &node);

        // create out links to partitions 1 and 3
        let out_link_1_1 = Link::new_with_default(Id::create("out-link-1-1"), &node, &node_1_1);
        let out_link_1_2 = Link::new_with_default(Id::create("out-link-1-2"), &node, &node_1_2);
        let out_link_3_1 = Link::new_with_default(Id::create("out-link-3-1"), &node, &node_3_1);

        net.add_node(node);
        net.add_node(node_1_1);
        net.add_node(node_1_2);
        net.add_node(node_2_1);
        net.add_node(node_3_1);
        net.add_node(node_4_1);
        net.add_link(in_link_1_1);
        net.add_link(in_link_1_2);
        net.add_link(in_link_2_1);
        net.add_link(out_link_1_1);
        net.add_link(out_link_1_2);
        net.add_link(out_link_3_1);

        let sim_net = SimNetworkPartition::from_network_for_test(&net, 0, &test_utils::config());

        let neighbors = sim_net.neighbors();
        assert_eq!(3, neighbors.len());
        assert!(neighbors.contains(&1));
        assert!(neighbors.contains(&2));
        assert!(neighbors.contains(&3));
        assert!(!neighbors.contains(&4));
    }

    #[deterministic_id_test]
    fn neighbors_only_over_links_with_a_main_mode() {
        let mut net = Network::new();
        let node = Node::new(Id::create("node"), Coordinate::default(), 0, 1);
        let car_node = Node::new(Id::create("car-node"), Coordinate::default(), 1, 1);
        let pt_node = Node::new(Id::create("pt-node"), Coordinate::default(), 2, 1);
        let other_node = Node::new(Id::create("other-node"), Coordinate::default(), 3, 1);

        let mut car_link = Link::new_with_default(Id::create("car-link"), &car_node, &node);
        car_link.modes = IntSet::from_iter([Id::create("car"), Id::create("bike")]);
        let mut pt_link = Link::new_with_default(Id::create("pt-link"), &node, &pt_node);
        pt_link.modes = IntSet::from_iter([Id::create("pt")]);
        let link_without_modes =
            Link::new_with_default(Id::create("link-without-modes"), &other_node, &node);

        net.add_node(node);
        net.add_node(car_node);
        net.add_node(pt_node);
        net.add_node(other_node);
        net.add_link(car_link);
        net.add_link(pt_link);
        net.add_link(link_without_modes);

        // main modes: car
        let config = test_utils::config();
        let sim_net = SimNetworkPartition::from_network_for_test(&net, 0, &config);
        assert_eq!(IntSet::from_iter([1, 3]), sim_net.neighbors());

        // a partition whose split links carry no vehicles keeps them as neighbors
        let pt_net = SimNetworkPartition::from_network_for_test(&net, 2, &config);
        assert_eq!(IntSet::from_iter([0]), pt_net.neighbors());

        // without main modes, every split link counts
        let mut config = test_utils::config();
        config.qsim_mut().main_modes = vec![];
        let sim_net = SimNetworkPartition::from_network_for_test(&net, 0, &config);
        assert_eq!(IntSet::from_iter([1, 2, 3]), sim_net.neighbors());
    }

    fn create_three_node_sim_network_with_partition(
        network: &mut Network,
    ) -> Vec<SimNetworkPartition> {
        let node1 = Node::new(Id::create("node1"), Coordinate::new_2d(-100., 0.), 0, 1);
        let node2 = Node::new(Id::create("node2"), Coordinate::new_2d(0., 0.), 0, 1);
        let mut node3 = Node::new(Id::create("node3"), Coordinate::new_2d(100., 0.), 0, 1);
        node3.partition = 1;
        let mut link1 = Link::new_with_default(Id::create("link1"), &node1, &node2);
        link1.capacity = 3600.;
        link1.freespeed = 10.;
        let mut link2 = Link::new_with_default(Id::create("link2"), &node2, &node3);
        link2.capacity = 3600.;
        link2.freespeed = 10.;
        link2.partition = 1;

        network.add_node(node1);
        network.add_node(node2);
        network.add_node(node3);
        network.add_link(link1);
        network.add_link(link2);

        vec![
            SimNetworkPartition::from_network_for_test(network, 0, &test_utils::config()),
            SimNetworkPartition::from_network_for_test(network, 1, &test_utils::config()),
        ]
    }
}
