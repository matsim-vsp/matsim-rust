use crate::simulation::Identifiable;
use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::agents::{SimulationAgentLogic, SimulationAgentState};
use crate::simulation::config::QSim;
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::engines::emit_partition_enter_events_for_vehicle;
use crate::simulation::engines::leg_engine::ResponsibleEngine::{Leg, Teleportation};
use crate::simulation::engines::network_engine::NetworkEngine;
use crate::simulation::engines::teleportation_engine::TeleportationEngine;
use crate::simulation::events::{
    PersonArrivalEventBuilder, PersonDepartureEventBuilder, PersonEntersVehicleEventBuilder,
    PersonLeavesVehicleEventBuilder,
};
use crate::simulation::id::Id;
use crate::simulation::messaging::messages::InternalSyncMessage;
use crate::simulation::messaging::partition_change::{
    PartitionChangeContext, PartitionChangeEntity,
};
use crate::simulation::messaging::sim_communication::SimCommunicator;
use crate::simulation::messaging::sim_communication::message_broker::NetMessageBroker;
use crate::simulation::network::sim_network::SimNetworkPartition;
use crate::simulation::scenario::population::InternalRoute;
use crate::simulation::scenario::vehicles::Garage;
use crate::simulation::time::{SimClock, SimTime, Tick};
use crate::simulation::vehicles::SimulationVehicle;
use nohash_hasher::IntSet;
use std::sync::Arc;
use tracing::instrument;

enum ResponsibleEngine {
    Leg,
    Teleportation,
}

pub struct LegEngine<C: SimCommunicator> {
    teleportation_engine: TeleportationEngine,
    network_engine: NetworkEngine,
    garage: Arc<Garage>,
    net_message_broker: NetMessageBroker<C>,
    departure_handler: VehicularDepartureHandler,
    main_modes: IntSet<Id<String>>,
    comp_env: ThreadLocalComputationalEnvironment,
    clock: SimClock,
}

impl<C: SimCommunicator> LegEngine<C> {
    pub fn new(
        network: SimNetworkPartition,
        garage: Arc<Garage>,
        net_message_broker: NetMessageBroker<C>,
        config: &QSim,
        comp_env: ThreadLocalComputationalEnvironment,
    ) -> Self {
        let clock = SimClock::new(config.ticks_per_second);
        let main_modes: IntSet<Id<String>> = config
            .main_modes
            .iter()
            .map(|m| Id::<String>::get_from_ext(m))
            .collect();

        let departure_handler = VehicularDepartureHandler {
            comp_env: comp_env.clone(),
            main_modes: main_modes.clone(),
        };

        LegEngine {
            teleportation_engine: TeleportationEngine::new(comp_env.clone(), clock),
            network_engine: NetworkEngine::new(network, comp_env.clone(), clock),
            garage,
            net_message_broker,
            departure_handler,
            main_modes,
            comp_env,
            clock,
        }
    }

    pub(crate) fn drain(&mut self) -> Vec<SimulationAgent> {
        self.network_engine
            .drain()
            .into_iter()
            .chain(self.teleportation_engine.drain())
            .collect()
    }

    /// Performs a sim step for the leg engine. Note that vehicles that leave a link and move to another link are always processed one time step later.
    /// This is in line with the Java reference implementation. The reason is that the order is:
    ///
    /// 1. `move_nodes`
    /// 2. `move_links`
    /// 3. `send_recv`
    ///
    /// Let's say, a vehicle's earliest exit time is `x`. The `move_links` call puts it into the buffer
    /// at time step `x` (assuming it is free), and the `move_nodes` call at time step `x+1` puts it onto the next link.
    /// The corresponding LinkEnter and LinkLeave events have time step `x+1`
    ///
    /// Vehicle's earliest exit time is always >=1 time step. This is required because then the partitioning doesn't matter.
    /// Let's say, a vehicle starts in step `x` and has travel time 0 time steps. A normal link would put it in the buffer during `x` in `move_links`
    /// and move it in `move_nodes` during `x+1`.
    /// A split link would send it during `x` (prepared in `move_links` and executed in `send_recv`), put into the buffer in `move_links`
    /// during `x+1` and moved over node in `move_nodes` during `x+2`.
    ///
    /// So, minimal time on a link is `2` steps. Thus, in the upper case without partitions, the link travel time is 1 time step + 1 time step due to
    /// `move_nodes`. For all travel times greater than this, it is the same.
    #[instrument(level = "trace", skip(self, agents), fields(rank=self.net_message_broker.rank()))]
    pub(crate) fn do_step(
        &mut self,
        now: Tick,
        agents: Vec<SimulationAgent>,
    ) -> Vec<SimulationAgent> {
        self.receive_agents(now, agents);

        self.network_engine.move_nodes(now);
        let network_vehicles = self
            .network_engine
            .move_links(now, &mut self.net_message_broker);

        let sync_messages = self.send_recv(now);

        for mut msg in sync_messages {
            let from = msg.from_process();
            let to = msg.to_process();
            let migration_time = self.clock.tick_to_time(msg.time());
            self.network_engine
                .network
                .apply_storage_cap_updates(msg.take_storage_capacities());

            let context = PartitionChangeContext {
                time: migration_time,
                from,
                to,
            };

            for vehicle_message in msg.take_vehicles() {
                let (veh, attachments) = vehicle_message.into_parts();
                self.comp_env
                    .partition_migration_extensions_manager_borrow_mut()
                    .receive(PartitionChangeEntity::Vehicle(&veh), attachments, &context);
                emit_partition_enter_events_for_vehicle(
                    &mut self.comp_env,
                    &veh,
                    from,
                    self.clock.tick_to_time(now),
                );
                self.pass_to_leg_vehicle(now, veh, false);
            }

            for mut teleportation in msg.take_teleportations() {
                let attachments = teleportation.take_attachments();
                self.comp_env
                    .partition_migration_extensions_manager_borrow_mut()
                    .receive(
                        PartitionChangeEntity::TeleportationAgent(teleportation.agent()),
                        attachments,
                        &context,
                    );
                self.teleportation_engine
                    .receive_remote_agent(now, teleportation.into(), from, to);
            }
        }

        let teleported_vehicles = self.teleportation_engine.do_step(now);

        let mut agents = vec![];
        agents.extend(self.publish_vehicular_end_events(now, network_vehicles));
        agents.extend(self.publish_teleported_end_events(now, teleported_vehicles));
        agents
    }

    #[instrument(level = "trace", skip(self), fields(rank=self.net_message_broker.rank()))]
    fn send_recv(&mut self, now: Tick) -> Vec<InternalSyncMessage> {
        self.net_message_broker.send_recv(now)
    }

    fn receive_agents(&mut self, now: Tick, agents: Vec<SimulationAgent>) {
        for agent in agents {
            self.receive_agent(now, agent);
        }
    }

    fn publish_vehicular_end_events(
        &mut self,
        now: Tick,
        vehicles: Vec<SimulationVehicle>,
    ) -> Vec<SimulationAgent> {
        let now_time = self.clock.tick_to_time(now);
        let mut agents = vec![];
        for veh in vehicles {
            //in case of teleportation, do not publish leave vehicle events
            self.comp_env.events_manager_borrow_mut().process_event(
                &PersonLeavesVehicleEventBuilder::default()
                    .time(now_time)
                    .vehicle(veh.id().clone())
                    .person(veh.driver().id().clone())
                    .build()
                    .unwrap(),
            );
            for passenger in veh.passengers() {
                self.comp_env.events_manager_borrow_mut().process_event(
                    &PersonLeavesVehicleEventBuilder::default()
                        .time(now_time)
                        .vehicle(veh.id().clone())
                        .person(passenger.id().clone())
                        .build()
                        .unwrap(),
                );
            }

            let leg = veh.driver().curr_leg();
            self.comp_env.events_manager_borrow_mut().process_event(
                &PersonArrivalEventBuilder::default()
                    .time(now_time)
                    .person(veh.driver().id().clone())
                    .link(veh.curr_link_id().unwrap().clone())
                    .leg_mode(leg.mode.clone())
                    .build()
                    .unwrap(),
            );
            for passenger in veh.passengers() {
                self.publish_person_arrival(now_time.clone(), passenger);
            }

            agents.extend(veh.into_agents());
        }
        agents
    }

    fn publish_teleported_end_events(
        &mut self,
        now: Tick,
        agents: Vec<SimulationAgent>,
    ) -> Vec<SimulationAgent> {
        let now_time = self.clock.tick_to_time(now);
        let mut ret_agents = Vec::with_capacity(agents.len());
        for agent in agents {
            self.publish_person_arrival(now_time.clone(), &agent);
            ret_agents.push(agent);
        }
        ret_agents
    }

    fn publish_person_arrival(&mut self, now_time: SimTime, agent: &SimulationAgent) {
        let leg = agent.curr_leg();
        self.comp_env.events_manager_borrow_mut().process_event(
            &PersonArrivalEventBuilder::default()
                .time(now_time)
                .person(agent.id().clone())
                .link(agent.curr_link_id().unwrap().clone())
                .leg_mode(leg.mode.clone())
                .build()
                .unwrap(),
        );
    }

    pub(crate) fn receive_agent(&mut self, now: Tick, mut agent: SimulationAgent) {
        let now_time = self.clock.tick_to_time(now);
        agent.advance_plan(now_time);

        let leg = agent.curr_leg();
        let route = leg.route.as_ref().unwrap();

        self.comp_env.events_manager_borrow_mut().process_event(
            &PersonDepartureEventBuilder::default()
                .time(now_time)
                .person(agent.id().clone())
                .link(route.start_link().clone())
                .leg_mode(leg.mode.clone())
                .routing_mode(
                    leg.routing_mode
                        .as_ref()
                        .unwrap_or_else(|| panic!("Missing routing mode for leg {:?}", leg))
                        .clone(),
                )
                .build()
                .unwrap(),
        );

        match self.find_responsible_engine(&agent) {
            Leg => self.pass_to_leg(now, agent, true),
            Teleportation => self.pass_to_teleportation(now, agent),
        }
    }

    fn find_responsible_engine(&self, agent: &SimulationAgent) -> ResponsibleEngine {
        let leg = agent.curr_leg();

        // If mode of leg is not main mode, teleport vehicle in every case
        if !self.main_modes.contains(&leg.mode) {
            return Teleportation;
        }

        // Otherwise, make the decision based on the route type
        match leg.route.as_ref().unwrap() {
            InternalRoute::Network(_) => Leg,
            _ => Teleportation,
        }
    }

    fn pass_to_teleportation(&mut self, now: Tick, agent: SimulationAgent) {
        self.teleportation_engine
            .receive_agent(now, agent, &mut self.net_message_broker);
    }

    fn pass_to_leg(&mut self, now: Tick, agent: SimulationAgent, route_begin: bool) {
        let now_time = self.clock.tick_to_time(now);

        let agent_id = agent.id().clone();

        let vehicle = self
            .departure_handler
            .handle_departure(now_time, agent, &self.garage)
            .unwrap_or_else(|| panic!("Failed to handle departure for agent {}", agent_id));

        self.pass_to_leg_vehicle(now, vehicle, route_begin);
    }

    fn pass_to_leg_vehicle(&mut self, now: Tick, vehicle: SimulationVehicle, route_begin: bool) {
        self.network_engine
            .receive_vehicle(now, vehicle, route_begin)
    }

    pub fn net_message_broker(&self) -> &NetMessageBroker<C> {
        &self.net_message_broker
    }

    pub fn network(&self) -> &SimNetworkPartition {
        &self.network_engine.network
    }
}

struct VehicularDepartureHandler {
    comp_env: ThreadLocalComputationalEnvironment,
    main_modes: IntSet<Id<String>>,
}

impl VehicularDepartureHandler {
    fn handle_departure(
        &mut self,
        now: SimTime,
        agent: SimulationAgent,
        garage: &Garage,
    ) -> Option<SimulationVehicle> {
        assert_eq!(agent.state(), SimulationAgentState::LEG);

        let leg = agent.curr_leg();
        let route = leg
            .route
            .as_ref()
            .unwrap_or_else(|| panic!("Missing route for agent {} at leg {:?}", agent.id(), leg));

        let veh_id = if let Some(v) = route.as_generic().vehicle().as_ref() {
            v.clone()
        } else {
            Id::get_from_ext(&format!(
                "{}_{}",
                agent.id().external(),
                leg.mode.external()
            ))
        };

        if self.main_modes.contains(&leg.mode) {
            assert!(
                route.as_network().is_some(),
                "{} is set as main mode but route is not network route",
                leg.mode
            );
            self.comp_env.events_manager_borrow_mut().process_event(
                &PersonEntersVehicleEventBuilder::default()
                    .time(now)
                    .person(agent.id().clone())
                    .vehicle(veh_id.clone())
                    .build()
                    .unwrap(),
            );
        }

        Some(garage.unpark_veh(agent, veh_id))
    }
}
