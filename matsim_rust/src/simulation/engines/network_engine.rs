use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::engines::emit_partition_leave_events_for_vehicle;
use crate::simulation::messaging::messages::VehicleMessage;
use crate::simulation::messaging::partition_change::{
    PartitionChangeContext, PartitionChangeEntity,
};
use crate::simulation::messaging::sim_communication::SimCommunicator;
use crate::simulation::messaging::sim_communication::message_broker::NetMessageBroker;
use crate::simulation::network::sim_network::SimNetworkPartition;
use crate::simulation::time::{SimClock, Tick};
use crate::simulation::vehicles::SimulationVehicle;
use tracing::instrument;

pub(crate) struct NetworkEngine {
    pub(crate) network: SimNetworkPartition,
    comp_env: ThreadLocalComputationalEnvironment,
    clock: SimClock,
}

impl NetworkEngine {
    pub fn new(
        network: SimNetworkPartition,
        comp_env: ThreadLocalComputationalEnvironment,
        clock: SimClock,
    ) -> Self {
        NetworkEngine {
            network,
            comp_env,
            clock,
        }
    }

    pub(crate) fn drain(&mut self) -> Vec<SimulationAgent> {
        self.network.drain()
    }

    pub(crate) fn receive_vehicle(
        &mut self,
        now: Tick,
        vehicle: SimulationVehicle,
        route_begin: bool,
    ) {
        let events = if route_begin {
            //if route has just begun, no link enter event should be published
            None
        } else {
            //if route is already in progress, this method gets vehicles from another partition and should publish link enter event
            //this is because the receiving partition is the owner of this link and should publish the event
            Some(self.comp_env.events_manager())
        };
        self.network.send_veh_en_route(vehicle, events, now)
    }

    #[instrument(level = "trace", skip(self), fields(rank = self.network.partition()))]
    pub(super) fn move_nodes(&mut self, now: Tick) {
        self.network.move_nodes(&mut self.comp_env, now)
    }

    #[instrument(level = "trace", skip(self, net_message_broker), fields(rank = self.network.partition()))]
    pub(super) fn move_links<C: SimCommunicator>(
        &mut self,
        now: Tick,
        net_message_broker: &mut NetMessageBroker<C>,
    ) -> Vec<SimulationVehicle> {
        let move_links_result = self.network.move_links(&mut self.comp_env, now);

        for veh in move_links_result.vehicles_exit_partition {
            let to = net_message_broker.rank_for_link(
                veh.curr_link_id()
                    .expect("Vehicles leaving a partition must have a destination link"),
            );
            emit_partition_leave_events_for_vehicle(
                &mut self.comp_env,
                &veh,
                to,
                self.clock.tick_to_time(now),
            );
            let context = PartitionChangeContext {
                time: self.clock.tick_to_time(now),
                from: self.network.partition(),
                to,
            };
            let attachments = self
                .comp_env
                .partition_migration_extensions_manager_borrow_mut()
                .send(PartitionChangeEntity::Vehicle(&veh), &context);
            net_message_broker.add_veh(VehicleMessage::with_attachments(veh, attachments), now);
        }

        for cap in move_links_result.storage_cap_updates {
            net_message_broker.add_cap_update(cap, now);
        }

        move_links_result.vehicles_end_leg
    }
}
