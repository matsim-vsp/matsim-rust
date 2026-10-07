use crate::simulation::Identifiable;
use crate::simulation::agents::SimulationAgentLogic;
use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::framework_events::{
    AgentEntersPartitionEvent, AgentLeavesPartitionEvent, PartitionEvent,
    VehicleEntersPartitionEvent, VehicleLeavesPartitionEvent,
};
use crate::simulation::time::SimTime;
use crate::simulation::vehicles::SimulationVehicle;

pub mod activity_engine;
pub mod leg_engine;
pub mod network_engine;
pub mod teleportation_engine;

fn emit_partition_leave_events_for_vehicle(
    comp_env: &mut ThreadLocalComputationalEnvironment,
    vehicle: &SimulationVehicle,
    to: u32,
    now: SimTime,
) {
    comp_env
        .partition_events_manager_borrow_mut()
        .process_event(PartitionEvent::VehicleLeavesPartition(
            VehicleLeavesPartitionEvent {
                vehicle_id: vehicle.id().clone(),
                to,
                time: now,
            },
        ));
    emit_partition_leave_events_for_agent(comp_env, vehicle.driver(), to, now);
    for passenger in vehicle.passengers() {
        emit_partition_leave_events_for_agent(comp_env, passenger, to, now);
    }
}

fn emit_partition_leave_events_for_agent(
    comp_env: &mut ThreadLocalComputationalEnvironment,
    agent: &SimulationAgent,
    to: u32,
    now: SimTime,
) {
    comp_env
        .partition_events_manager_borrow_mut()
        .process_event(PartitionEvent::AgentLeavesPartition(
            AgentLeavesPartitionEvent {
                agent_id: agent.id().clone(),
                to,
                time: now,
            },
        ));
}

fn emit_partition_enter_events_for_vehicle(
    comp_env: &mut ThreadLocalComputationalEnvironment,
    vehicle: &SimulationVehicle,
    from: u32,
    now: SimTime,
) {
    comp_env
        .partition_events_manager_borrow_mut()
        .process_event(PartitionEvent::VehicleEntersPartition(
            VehicleEntersPartitionEvent {
                vehicle_id: vehicle.id().clone(),
                network_mode: vehicle.driver().curr_leg().mode.clone(),
                from,
                time: now,
            },
        ));
    emit_partition_enter_events_for_agent(comp_env, vehicle.driver(), from, now);
    for passenger in vehicle.passengers() {
        emit_partition_enter_events_for_agent(comp_env, passenger, from, now);
    }
}

fn emit_partition_enter_events_for_agent(
    comp_env: &mut ThreadLocalComputationalEnvironment,
    agent: &SimulationAgent,
    from: u32,
    now: SimTime,
) {
    comp_env
        .partition_events_manager_borrow_mut()
        .process_event(PartitionEvent::AgentEntersPartition(
            AgentEntersPartitionEvent {
                agent_id: agent.id().clone(),
                from,
                time: now,
            },
        ));
}
