use crate::simulation::agents::{
    LegAgent, LegLogic, NetworkDriverAgent, PassengerAgent, TeleportationAgent, TransitDriverAgent,
};
use crate::simulation::config::{self, QSim, TransitSimulation};
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::engines::emit_partition_enter_events_for_vehicle;
use crate::simulation::engines::leg_engine::ResponsibleEngine::{
    Network, Teleportation, Transit, TransitDriver,
};
use crate::simulation::engines::network_engine::NetworkEngine;
use crate::simulation::engines::teleportation_engine::TeleportationEngine;
use crate::simulation::engines::transit_engine::TransitEngine;
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
use crate::simulation::pt::schedule::TransitSimSchedule;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::vehicles::Garage;
use crate::simulation::time::{SimClock, SimTime, Tick};
use crate::simulation::vehicles::SimulationVehicle;
use nohash_hasher::IntSet;
use std::sync::Arc;
use tracing::instrument;

enum ResponsibleEngine {
    Network(NetworkDriverAgent),
    Teleportation(TeleportationAgent),
    Transit(PassengerAgent),
    TransitDriver(TransitDriverAgent),
}

/// The engine that the leg engine prefers for the mode of a leg.
#[derive(Clone, Copy)]
enum PreferredEngine {
    Network,
    Transit,
    Teleportation,
}

/// Policy belongs to the engine; capability conversion belongs to the current agent.
fn select_execution(agent: LegAgent, preferred: PreferredEngine) -> ResponsibleEngine {
    // Transit drivers drive their vehicles in the transit engine, whatever the mode of their route.
    let agent = match agent.try_into_transit_driver() {
        Ok(driver) => return TransitDriver(driver),
        Err(agent) => agent,
    };
    // A main mode prefers the network, a transit passenger mode the transit engine. Only an
    // available role permits that. Otherwise, and for all other modes, the leg is teleported.
    let agent = match preferred {
        PreferredEngine::Network => match agent.try_into_network() {
            Ok(driver) => return Network(driver),
            Err(agent) => agent,
        },
        PreferredEngine::Transit => match agent.try_into_passenger() {
            Ok(passenger) => return Transit(passenger),
            Err(agent) => agent,
        },
        PreferredEngine::Teleportation => agent,
    };
    match agent.try_into_teleportation() {
        Ok(agent) => Teleportation(agent),
        Err(agent) => panic!(
            "Agent {}: no permitted leg execution capability",
            agent.id()
        ),
    }
}

pub struct LegEngine<C: SimCommunicator> {
    teleportation_engine: TeleportationEngine,
    network_engine: NetworkEngine,
    transit_engine: TransitEngine,
    garage: Arc<Garage>,
    net_message_broker: NetMessageBroker<C>,
    main_modes: IntSet<Id<String>>,
    transit_modes: IntSet<Id<String>>,
    comp_env: ThreadLocalComputationalEnvironment,
    clock: SimClock,
}

impl<C: SimCommunicator> LegEngine<C> {
    pub fn new(
        network: SimNetworkPartition,
        garage: Arc<Garage>,
        transit_schedule: Arc<TransitSimSchedule>,
        net_message_broker: NetMessageBroker<C>,
        config: &QSim,
        transit: &config::Transit,
        comp_env: ThreadLocalComputationalEnvironment,
    ) -> Self {
        let clock = SimClock::new(config.ticks_per_second);
        let main_modes: IntSet<Id<String>> = config
            .main_modes
            .iter()
            .map(|m| Id::<String>::get_from_ext(m))
            .collect();
        // A mode without an id has no legs.
        let transit_modes: IntSet<Id<String>> = match transit.simulation {
            TransitSimulation::Teleported => IntSet::default(),
            TransitSimulation::ScheduleBasedTeleportation => transit
                .passenger_modes
                .iter()
                .filter_map(|mode| Id::try_get_from_ext(mode))
                .collect(),
        };
        assert!(
            transit_modes.is_disjoint(&main_modes),
            "A mode cannot be both a main mode and a transit passenger mode."
        );

        LegEngine {
            teleportation_engine: TeleportationEngine::new(comp_env.clone(), clock),
            network_engine: NetworkEngine::new(network, comp_env.clone(), clock),
            transit_engine: TransitEngine::new(
                transit_schedule,
                net_message_broker.rank(),
                comp_env.clone(),
                clock,
            ),
            garage,
            net_message_broker,
            main_modes,
            transit_modes,
            comp_env,
            clock,
        }
    }

    pub(crate) fn drain(&mut self) -> Vec<LegAgent> {
        self.network_engine
            .drain()
            .into_iter()
            .chain(self.teleportation_engine.drain())
            .chain(self.transit_engine.drain())
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
    #[hotpath::measure]
    #[instrument(level = "trace", skip(self, agents), fields(rank=self.net_message_broker.rank()))]
    pub(crate) fn do_step(&mut self, now: Tick, agents: Vec<LegAgent>) -> Vec<LegAgent> {
        self.receive_agents(now, agents);

        // 1: move nodes
        self.network_engine.move_nodes(now);

        // 2: move links
        let network_vehicles = self
            .network_engine
            .move_links(now, &mut self.net_message_broker);

        // 3: transit
        let transit = self.transit_engine.do_step(now);
        for (to, message) in transit.leaving {
            self.net_message_broker
                .add_transit_vehicle(message, to, now);
        }

        // 4: send/receive messages
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
                self.pass_to_network_vehicle(now, veh, false);
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

            for message in msg.take_transit_vehicles() {
                let (vehicle, attachments) = message.into_parts();
                self.comp_env
                    .partition_migration_extensions_manager_borrow_mut()
                    .receive(
                        PartitionChangeEntity::TransitVehicle(&vehicle),
                        attachments,
                        &context,
                    );
                self.transit_engine
                    .receive_remote_vehicle(now, vehicle, from);
            }
        }

        // 5: teleportation
        let teleported_vehicles = self.teleportation_engine.do_step(now);

        let mut agents = vec![];
        agents.extend(self.publish_vehicular_end_events(now, network_vehicles));
        agents.extend(self.publish_teleported_end_events(now, teleported_vehicles));
        // Passengers and drivers have left their vehicles already, so they only arrive, like
        // teleported agents.
        agents.extend(self.publish_teleported_end_events(now, transit.arrived));
        agents
    }

    #[instrument(level = "trace", skip(self), fields(rank=self.net_message_broker.rank()))]
    fn send_recv(&mut self, now: Tick) -> Vec<InternalSyncMessage> {
        self.net_message_broker.send_recv(now)
    }

    fn receive_agents(&mut self, now: Tick, agents: Vec<LegAgent>) {
        for agent in agents {
            self.receive_agent(now, agent);
        }
    }

    fn publish_vehicular_end_events(
        &mut self,
        now: Tick,
        vehicles: Vec<SimulationVehicle>,
    ) -> Vec<LegAgent> {
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

            // Passengers do not move along their own route, so everyone arrives on the vehicle's link.
            let link = veh.curr_link_id().unwrap();
            self.publish_person_arrival(now_time, veh.driver(), link);
            for passenger in veh.passengers() {
                self.publish_person_arrival(now_time, passenger.as_ref(), link);
            }

            agents.extend(veh.into_agents());
        }
        agents
    }

    fn publish_teleported_end_events(&mut self, now: Tick, agents: Vec<LegAgent>) -> Vec<LegAgent> {
        let now_time = self.clock.tick_to_time(now);
        let mut ret_agents = Vec::with_capacity(agents.len());
        for agent in agents {
            self.publish_person_arrival(now_time, agent.as_ref(), agent.curr_link_id().unwrap());
            ret_agents.push(agent);
        }
        ret_agents
    }

    fn publish_person_arrival(&mut self, now_time: SimTime, agent: &dyn LegLogic, link: &Id<Link>) {
        self.comp_env.events_manager_borrow_mut().process_event(
            &PersonArrivalEventBuilder::default()
                .time(now_time)
                .person(agent.id().clone())
                .link(link.clone())
                .leg_mode(agent.mode().clone())
                .build()
                .unwrap(),
        );
    }

    pub(crate) fn receive_agent(&mut self, now: Tick, agent: LegAgent) {
        let preferred = if self.transit_modes.contains(agent.mode()) {
            PreferredEngine::Transit
        } else if self.main_modes.contains(agent.mode()) {
            PreferredEngine::Network
        } else {
            PreferredEngine::Teleportation
        };
        // The engine is selected before the departure is emitted, so that the transit engine can
        // emit the departure of a driver after the start of its departure, as MATSim does.
        match select_execution(agent, preferred) {
            Network(agent) => {
                self.publish_departure(now, agent.as_ref());
                self.pass_to_network(now, agent, true);
            }
            Teleportation(agent) => {
                self.publish_departure(now, agent.as_ref());
                self.pass_to_teleportation(now, agent);
            }
            Transit(agent) => {
                self.publish_departure(now, agent.as_ref());
                self.transit_engine.receive_agent(agent);
            }
            TransitDriver(driver) => self.transit_engine.receive_driver(now, driver),
        }
    }

    fn publish_departure(&mut self, now: Tick, leg: &dyn LegLogic) {
        self.comp_env.events_manager_borrow_mut().process_event(
            &PersonDepartureEventBuilder::default()
                .time(self.clock.tick_to_time(now))
                .person(leg.id().clone())
                .link(leg.departure_link().clone())
                .leg_mode(leg.mode().clone())
                .routing_mode(Some(
                    leg.routing_mode()
                        .unwrap_or_else(|| {
                            panic!(
                                "Missing routing mode for {} leg of agent {}",
                                leg.mode(),
                                leg.id()
                            )
                        })
                        .clone(),
                ))
                .build()
                .unwrap(),
        );
    }

    fn pass_to_teleportation(&mut self, now: Tick, agent: TeleportationAgent) {
        self.teleportation_engine
            .receive_agent(now, agent, &mut self.net_message_broker);
    }

    /// The driver enters its planned vehicle, or the vehicle `<person>_<mode>` if it plans none,
    /// which leaves the garage for the network.
    fn pass_to_network(&mut self, now: Tick, agent: NetworkDriverAgent, route_begin: bool) {
        let veh_id = match agent.planned_vehicle_id() {
            Some(vehicle) => vehicle.clone(),
            None => Id::get_from_ext(&format!(
                "{}_{}",
                agent.id().external(),
                agent.mode().external()
            )),
        };

        self.comp_env.events_manager_borrow_mut().process_event(
            &PersonEntersVehicleEventBuilder::default()
                .time(self.clock.tick_to_time(now))
                .person(agent.id().clone())
                .vehicle(veh_id.clone())
                .build()
                .unwrap(),
        );

        let vehicle = self.garage.unpark_veh(agent, veh_id);
        self.pass_to_network_vehicle(now, vehicle, route_begin);
    }

    fn pass_to_network_vehicle(
        &mut self,
        now: Tick,
        vehicle: SimulationVehicle,
        route_begin: bool,
    ) {
        self.network_engine
            .receive_vehicle(now, vehicle, route_begin)
    }

    pub fn net_message_broker(&self) -> &NetMessageBroker<C> {
        &self.net_message_broker
    }

    pub fn net_message_broker_mut(&mut self) -> &mut NetMessageBroker<C> {
        &mut self.net_message_broker
    }

    pub fn network(&self) -> &SimNetworkPartition {
        &self.network_engine.network
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::Identifiable;
    use crate::simulation::agents::agent_logic::PlanBasedSimulationLogic;
    use crate::simulation::agents::{NextPhase, PassengerAgent, SimulationAgentLogic};
    use crate::simulation::scenario::population::{
        InternalGenericRoute, InternalPerson, InternalRoute,
    };
    use crate::test_utils::create_agent;
    use macros::deterministic_id_test;

    #[deterministic_id_test]
    fn main_mode_uses_network_when_available() {
        let agent: LegAgent = create_agent(1, vec!["a", "b"]);
        let address = std::ptr::from_ref(agent.as_ref()).cast::<()>();
        let Network(driver) = select_execution(agent, PreferredEngine::Network) else {
            panic!("Expected network")
        };
        assert_eq!(address, std::ptr::from_ref(driver.as_ref()).cast::<()>());
    }

    #[deterministic_id_test]
    fn non_main_mode_teleports_even_a_network_route() {
        let agent: LegAgent = create_agent(1, vec!["a", "b"]);
        let address = std::ptr::from_ref(agent.as_ref()).cast::<()>();
        let Teleportation(agent) = select_execution(agent, PreferredEngine::Teleportation) else {
            panic!("Expected teleportation")
        };
        assert_eq!(agent.destination_link().external(), "b");
        assert_eq!(address, std::ptr::from_ref(agent.as_ref()).cast::<()>());
    }

    #[deterministic_id_test]
    fn main_mode_falls_back_to_an_available_teleportation_role() {
        let mut person = create_agent(1, vec!["a", "b"]).into_person().unwrap();
        person.plans_mut()[0].elements[1]
            .as_leg_mut()
            .unwrap()
            .route = Some(InternalRoute::Generic(InternalGenericRoute::new(
            Id::get_from_ext("a"),
            Id::get_from_ext("b"),
            None,
            None,
            None,
        )));
        let agent = PlanBasedSimulationLogic::new_agent(person)
            .finish_activity(SimTime::default())
            .expect_leg();
        let address = std::ptr::from_ref(agent.as_ref()).cast::<()>();
        let Teleportation(agent) = select_execution(agent, PreferredEngine::Network) else {
            panic!("Expected teleportation")
        };
        assert_eq!(address, std::ptr::from_ref(agent.as_ref()).cast::<()>());
    }

    struct Unsupported(LegAgent);
    impl Identifiable<InternalPerson> for Unsupported {
        fn id(&self) -> &Id<InternalPerson> {
            self.0.id()
        }
    }
    impl SimulationAgentLogic for Unsupported {
        fn into_person(self: Box<Self>) -> Option<InternalPerson> {
            self.0.into_person()
        }
    }
    impl LegLogic for Unsupported {
        fn mode(&self) -> &Id<String> {
            self.0.mode()
        }
        fn routing_mode(&self) -> Option<&Id<String>> {
            self.0.routing_mode()
        }
        fn departure_link(&self) -> &Id<Link> {
            self.0.departure_link()
        }
        fn curr_link_id(&self) -> Option<&Id<Link>> {
            self.0.curr_link_id()
        }
        fn try_into_network(self: Box<Self>) -> Result<NetworkDriverAgent, LegAgent> {
            Err(self)
        }
        fn try_into_teleportation(self: Box<Self>) -> Result<TeleportationAgent, LegAgent> {
            Err(self)
        }
        fn try_into_passenger(self: Box<Self>) -> Result<PassengerAgent, LegAgent> {
            Err(self)
        }
        fn try_into_transit_driver(self: Box<Self>) -> Result<TransitDriverAgent, LegAgent> {
            Err(self)
        }
        fn finish_leg(self: Box<Self>, now: SimTime) -> NextPhase {
            self.0.finish_leg(now)
        }
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Agent 1: no permitted leg execution capability")]
    fn missing_execution_capabilities_report_the_agent_id() {
        select_execution(
            Box::new(Unsupported(create_agent(1, vec!["a"]))),
            PreferredEngine::Network,
        );
    }
}
