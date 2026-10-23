use crate::simulation::agents::SimulationAgentLogic;
use crate::simulation::agents::agent_logic::PlanBasedSimulationLogic;
use crate::simulation::id::Id;
use crate::simulation::pt::schedule::SimRouteStop;
use crate::simulation::scenario::population::InternalPtRouteDescription;
use crate::simulation::scenario::transit::{TransitLine, TransitRoute, TransitStopFacility};

/// Decisions of an agent travelling with schedule-based transit. The transit engine moves the
/// vehicles and asks the passengers whether they board and alight, like the network engine asks
/// drivers for their next link.
///
/// The engine calls [`Self::access_stop`], [`Self::egress_stop`] and [`Self::waiting_for_line`]
/// once per leg, when the agent arrives at its stop, and keeps the results.
pub trait TransitPassengerLogic {
    /// The stop where the agent waits.
    fn access_stop(&self) -> Id<TransitStopFacility>;

    /// The stop where the agent wants to alight, only used for the events.
    fn egress_stop(&self) -> Id<TransitStopFacility>;

    /// A hint for the engine: with `Some`, the agent is only asked about vehicles of this line,
    /// with `None` about all vehicles.
    fn waiting_for_line(&self) -> Option<Id<TransitLine>>;

    /// Whether the agent boards a vehicle of `line` and `route`, which still serves `upcoming`
    /// stops after the current one.
    fn wants_to_board(
        &self,
        line: &Id<TransitLine>,
        route: &Id<TransitRoute>,
        upcoming: &[SimRouteStop],
    ) -> bool;

    /// Whether the agent alights at `stop`.
    fn wants_to_alight(&self, stop: &Id<TransitStopFacility>) -> bool;
}

impl PlanBasedSimulationLogic {
    /// The transit route description of the current leg, if the agent is on a transit leg.
    pub(crate) fn pt_description(&self) -> Option<&InternalPtRouteDescription> {
        self.curr_leg()
            .route
            .as_ref()
            .and_then(|route| route.as_pt())
            .map(|route| &route.description)
    }

    fn expect_pt_description(&self) -> &InternalPtRouteDescription {
        self.pt_description().unwrap_or_else(|| {
            panic!(
                "Agent {} is asked about transit, but its current leg has no transit route.",
                crate::simulation::Identifiable::id(self)
            )
        })
    }
}

fn resolve<T: crate::simulation::id::serializable_type::StableTypeId>(
    external: &str,
    what: &str,
) -> Id<T> {
    Id::try_get_from_ext(external).unwrap_or_else(|| {
        panic!("The transit route of a leg references the unknown {what} {external}.")
    })
}

/// MATSim's rule for passengers (`TransitAgentImpl` with `checkLineAndStop`): board vehicles of the
/// planned line, regardless of the route, if the planned egress stop is still served. Ids are
/// compared by their external strings, so that no id needs to be looked up per vehicle.
impl TransitPassengerLogic for PlanBasedSimulationLogic {
    fn access_stop(&self) -> Id<TransitStopFacility> {
        resolve(
            &self.expect_pt_description().access_facility_id,
            "access stop",
        )
    }

    fn egress_stop(&self) -> Id<TransitStopFacility> {
        resolve(
            &self.expect_pt_description().egress_facility_id,
            "egress stop",
        )
    }

    fn waiting_for_line(&self) -> Option<Id<TransitLine>> {
        Some(resolve(
            &self.expect_pt_description().transit_line_id,
            "line",
        ))
    }

    fn wants_to_board(
        &self,
        line: &Id<TransitLine>,
        _route: &Id<TransitRoute>,
        upcoming: &[SimRouteStop],
    ) -> bool {
        let description = self.expect_pt_description();
        line.external() == description.transit_line_id
            && upcoming
                .iter()
                .any(|stop| stop.facility.external() == description.egress_facility_id)
    }

    fn wants_to_alight(&self, stop: &Id<TransitStopFacility>) -> bool {
        stop.external() == self.expect_pt_description().egress_facility_id
    }
}

#[cfg(test)]
mod tests {
    use crate::simulation::agents::agent::SimulationAgent;
    use crate::simulation::agents::{AgentEvent, EnvironmentalEventObserver, SimulationAgentLogic};
    use crate::simulation::id::Id;
    use crate::simulation::pt::schedule::SimRouteStop;
    use crate::simulation::scenario::network::Link;
    use crate::simulation::scenario::transit::{TransitLine, TransitRoute, TransitStopFacility};
    use crate::simulation::time::SimTime;
    use crate::test_utils::{create_agent, create_pt_agent};
    use macros::deterministic_id_test;
    use std::time::Duration;

    fn stop(facility: &str) -> SimRouteStop {
        SimRouteStop {
            facility: Id::create(facility),
            link: Id::create("link"),
            partition: 0,
            arrival_offset: Duration::ZERO,
            departure_offset: Duration::ZERO,
        }
    }

    fn agent() -> SimulationAgent {
        // Creates the ids which the passenger logic resolves.
        Id::<TransitLine>::create("blue");
        Id::<TransitLine>::create("red");
        Id::<TransitRoute>::create("1to3");
        Id::<TransitRoute>::create("3to1");
        for facility in ["1", "2", "3"] {
            Id::<TransitStopFacility>::create(facility);
        }
        create_pt_agent(1, "blue", "1to3", "1", "3", "l1", "l3")
    }

    #[deterministic_id_test]
    fn pt_leg_exposes_stops_and_line() {
        let agent = agent();
        let passenger = agent.as_transit_passenger().unwrap();

        assert_eq!("1", passenger.access_stop().external());
        assert_eq!("3", passenger.egress_stop().external());
        assert_eq!(
            Some("blue"),
            passenger.waiting_for_line().as_ref().map(Id::external)
        );
    }

    #[deterministic_id_test]
    fn boards_vehicle_of_planned_line_if_egress_stop_follows() {
        let agent = agent();
        let passenger = agent.as_transit_passenger().unwrap();
        let upcoming = [stop("2"), stop("3")];

        assert!(passenger.wants_to_board(
            &Id::get_from_ext("blue"),
            &Id::get_from_ext("1to3"),
            &upcoming
        ));
        // Like MATSim, the route does not matter, only the line.
        assert!(passenger.wants_to_board(
            &Id::get_from_ext("blue"),
            &Id::get_from_ext("3to1"),
            &upcoming
        ));
    }

    #[deterministic_id_test]
    fn does_not_board_other_line_or_vehicle_which_does_not_serve_egress_stop() {
        let agent = agent();
        let passenger = agent.as_transit_passenger().unwrap();

        assert!(!passenger.wants_to_board(
            &Id::get_from_ext("red"),
            &Id::get_from_ext("1to3"),
            &[stop("3")]
        ));
        assert!(!passenger.wants_to_board(
            &Id::get_from_ext("blue"),
            &Id::get_from_ext("1to3"),
            &[stop("2")]
        ));
        assert!(!passenger.wants_to_board(
            &Id::get_from_ext("blue"),
            &Id::get_from_ext("1to3"),
            &[]
        ));
    }

    #[deterministic_id_test]
    fn alights_only_at_egress_stop() {
        let agent = agent();
        let passenger = agent.as_transit_passenger().unwrap();

        assert!(!passenger.wants_to_alight(&Id::get_from_ext("2")));
        assert!(passenger.wants_to_alight(&Id::get_from_ext("3")));
    }

    #[deterministic_id_test]
    fn non_transit_leg_is_no_transit_passenger() {
        let agent = create_agent(1, vec!["a", "b"]);

        assert!(agent.as_transit_passenger().is_none());
    }

    #[deterministic_id_test]
    fn leaving_transit_vehicle_moves_agent_to_end_link() {
        let mut agent = agent();
        assert_eq!(Some(&Id::<Link>::get_from_ext("l1")), agent.curr_link_id());

        agent.notify_event(&mut AgentEvent::TransitVehicleLeft(), SimTime::from_secs(1));

        assert_eq!(Some(&Id::<Link>::get_from_ext("l3")), agent.curr_link_id());
    }

    #[deterministic_id_test]
    fn adaptive_agent_delegates_transit_decisions() {
        agent();
        let person = create_pt_agent(2, "blue", "1to3", "1", "3", "l1", "l3")
            .into_person()
            .unwrap();
        let mut adaptive = SimulationAgent::new_adaptive_plan_based(person);
        adaptive.advance_plan(SimTime::default());

        let passenger = adaptive.as_transit_passenger().unwrap();
        assert!(passenger.wants_to_alight(&Id::get_from_ext("3")));
    }
}
