use crate::simulation::events::{EventTrait, EventsManager};
use crate::simulation::framework_events::{MobsimEvent, MobsimEventsManager, QSimId};
use crate::simulation::id::Id;
use crate::simulation::messaging::partition_change::PartitionChangeExtensionsManager;
use crate::simulation::pt::schedule::TransitSimSchedule;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scoring::backpacking::backpack::PersonExperience;
use crate::simulation::scoring::backpacking::backpacking_data_collector::{
    BackpackingAttachment, BackpackingDataCollector,
};
use nohash_hasher::IntMap;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::Sender;

pub(crate) struct BackpackingWorkerResult {
    pub(crate) rank: QSimId,
    pub(crate) iteration: u32,
    pub(crate) experienced_plans: IntMap<Id<InternalPerson>, PersonExperience>,
}

pub struct BackpackingEngine {
    backpacking_data_collector: BackpackingDataCollector,
}

impl BackpackingEngine {
    pub fn new(
        home_person_ids: Vec<Id<InternalPerson>>,
        transit_schedule: Arc<TransitSimSchedule>,
    ) -> Self {
        Self {
            backpacking_data_collector: BackpackingDataCollector::new(
                home_person_ids,
                transit_schedule,
            ),
        }
    }

    pub(crate) fn register(
        engine: Rc<RefCell<Self>>,
        events: &mut EventsManager,
        mobsim_events: &mut MobsimEventsManager,
        partition_change_extensions: &mut PartitionChangeExtensionsManager,
        result_sender: Sender<BackpackingWorkerResult>,
        rank: QSimId,
    ) {
        Self::register_event_handlers(engine.clone(), events);

        let send_engine = engine.clone();
        let receive_engine = engine.clone();
        partition_change_extensions.register::<BackpackingAttachment, _, _>(
            move |entity, _| {
                Some(
                    send_engine
                        .borrow_mut()
                        .backpacking_data_collector
                        .send(entity),
                )
            },
            move |entity, attachment, _| {
                receive_engine
                    .borrow_mut()
                    .backpacking_data_collector
                    .receive(
                        entity,
                        attachment.expect("Backpacking attachment is missing on arrival."),
                    );
            },
        );

        mobsim_events.on_event(move |event| {
            if event.payload == MobsimEvent::BeforeCleanup {
                let backpacks = engine.borrow_mut().finish();
                result_sender
                    .send(BackpackingWorkerResult {
                        rank,
                        iteration: event.meta.iteration,
                        experienced_plans: backpacks,
                    })
                    .unwrap_or_else(|error| {
                        panic!(
                            "Backpacking worker rank {rank} failed to send iteration {} result: {error}",
                            event.meta.iteration
                        )
                    });
            }
        });
    }

    fn register_event_handlers(engine: Rc<RefCell<Self>>, events: &mut EventsManager) {
        let reset_engine = engine.clone();
        events.on_reset_iteration(move |_| {
            reset_engine
                .borrow_mut()
                .backpacking_data_collector
                .reset_iteration();
        });

        // Subscribe only to the event types that change a backpack. All other events, e.g. link
        // leave events, which are about half of all events, then don't reach backpacking at all.
        use BackpackingDataCollector as Collector;
        Self::forward(&engine, events, Collector::activity_end);
        Self::forward(&engine, events, Collector::person_departure);
        Self::forward(&engine, events, Collector::person_enters_vehicle);
        Self::forward(&engine, events, Collector::vehicle_enters_traffic);
        Self::forward(&engine, events, Collector::link_enter);
        Self::forward(&engine, events, Collector::vehicle_leaves_traffic);
        Self::forward(&engine, events, Collector::person_leaves_vehicle);
        Self::forward(&engine, events, Collector::teleportation_arrival);
        Self::forward(&engine, events, Collector::pt_teleportation_arrival);
        Self::forward(&engine, events, Collector::person_arrival);
        Self::forward(&engine, events, Collector::activity_start);
        Self::forward(&engine, events, Collector::person_stuck);
        Self::forward(&engine, events, Collector::transit_driver_starts);
        Self::forward(&engine, events, Collector::vehicle_arrives_at_facility);
    }

    fn forward<E: EventTrait>(
        engine: &Rc<RefCell<Self>>,
        events: &mut EventsManager,
        handle: fn(&mut BackpackingDataCollector, &E),
    ) {
        let engine = engine.clone();
        events.on::<E, _>(move |event| {
            handle(&mut engine.borrow_mut().backpacking_data_collector, event);
        });
    }

    fn finish(&mut self) -> IntMap<Id<InternalPerson>, PersonExperience> {
        self.backpacking_data_collector.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::events::{
        ActivityEndEventBuilder, ActivityStartEventBuilder, LinkEnterEventBuilder,
        LinkLeaveEventBuilder, PersonArrivalEventBuilder, PersonDepartureEventBuilder,
        PersonEntersVehicleEventBuilder, PersonLeavesVehicleEventBuilder,
        VehicleEntersTrafficEventBuilder, VehicleLeavesTrafficEventBuilder,
    };
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::Link;
    use crate::simulation::scenario::population::{InternalPlanElement, InternalRoute};
    use crate::simulation::scenario::vehicles::InternalVehicle;
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::time::Duration;

    /// Drives a car leg with a driver and a passenger through the events manager, which covers the
    /// subscriptions of all event types of a network leg.
    #[deterministic_id_test]
    fn reconstructs_car_leg_of_driver_and_passenger_from_subscribed_events() {
        let driver = Id::<InternalPerson>::create("driver");
        let passenger = Id::<InternalPerson>::create("passenger");
        let vehicle = Id::<InternalVehicle>::create("vehicle");
        let car = Id::<String>::create("car");
        let link = |id: &str| Id::<Link>::create(id);
        let time = SimTime::from_secs;

        let engine = Rc::new(RefCell::new(BackpackingEngine::new(
            vec![driver.clone(), passenger.clone()],
            Default::default(),
        )));
        let mut events = EventsManager::new();
        BackpackingEngine::register_event_handlers(engine.clone(), &mut events);
        events.reset_iteration(0);
        // Backpacking subscribes to the fourteen event types it needs, but not to all events.
        assert!(format!("{events:?}").contains("per_type: 14, catch_all: 0"));

        for person in [&driver, &passenger] {
            events.process_event(
                &ActivityEndEventBuilder::default()
                    .time(time(10))
                    .person(person.clone())
                    .link(link("start"))
                    .coordinate(Coordinate::default())
                    .act_type(Id::create("home"))
                    .build()
                    .unwrap(),
            );
            events.process_event(
                &PersonDepartureEventBuilder::default()
                    .time(time(10))
                    .person(person.clone())
                    .link(link("start"))
                    .leg_mode(car.clone())
                    .routing_mode(Some(car.clone()))
                    .build()
                    .unwrap(),
            );
            events.process_event(
                &PersonEntersVehicleEventBuilder::default()
                    .time(time(10))
                    .person(person.clone())
                    .vehicle(vehicle.clone())
                    .build()
                    .unwrap(),
            );
        }
        events.process_event(
            &VehicleEntersTrafficEventBuilder::default()
                .time(time(10))
                .vehicle(vehicle.clone())
                .link(link("start"))
                .person(driver.clone())
                .network_mode(car.clone())
                .build()
                .unwrap(),
        );
        for (leave, enter, at) in [("start", "middle", 20), ("middle", "end", 30)] {
            events.process_event(
                &LinkLeaveEventBuilder::default()
                    .time(time(at))
                    .link(link(leave))
                    .vehicle(vehicle.clone())
                    .build()
                    .unwrap(),
            );
            events.process_event(
                &LinkEnterEventBuilder::default()
                    .time(time(at))
                    .link(link(enter))
                    .vehicle(vehicle.clone())
                    .build()
                    .unwrap(),
            );
        }
        events.process_event(
            &VehicleLeavesTrafficEventBuilder::default()
                .time(time(40))
                .vehicle(vehicle.clone())
                .link(link("end"))
                .person(driver.clone())
                .network_mode(car.clone())
                .build()
                .unwrap(),
        );
        for person in [&driver, &passenger] {
            events.process_event(
                &PersonLeavesVehicleEventBuilder::default()
                    .time(time(40))
                    .person(person.clone())
                    .vehicle(vehicle.clone())
                    .build()
                    .unwrap(),
            );
            events.process_event(
                &PersonArrivalEventBuilder::default()
                    .time(time(40))
                    .person(person.clone())
                    .link(link("end"))
                    .leg_mode(car.clone())
                    .build()
                    .unwrap(),
            );
            events.process_event(
                &ActivityStartEventBuilder::default()
                    .time(time(40))
                    .person(person.clone())
                    .link(link("end"))
                    .coordinate(Coordinate::default())
                    .act_type(Id::create("work"))
                    .build()
                    .unwrap(),
            );
        }

        let experiences = engine.borrow_mut().finish();
        for person in [&driver, &passenger] {
            let plan = experiences[person].plan();
            assert_eq!(plan.elements.len(), 3, "Unexpected plan of {person:?}");
            let InternalPlanElement::Leg(leg) = &plan.elements[1] else {
                panic!("Expected a leg for {person:?}");
            };
            assert_eq!(leg.mode, car);
            assert_eq!(leg.dep_time, Some(time(10)));
            assert_eq!(leg.trav_time, Some(Duration::from_secs(30)));
            let Some(InternalRoute::Network(route)) = &leg.route else {
                panic!("Expected a network route for {person:?}");
            };
            assert_eq!(
                route.route(),
                &vec![link("start"), link("middle"), link("end")]
            );
            assert_eq!(route.generic_delegate().vehicle(), &Some(vehicle.clone()));
        }
    }

    /// Runs the transit engine on the tutorial schedule with one passenger. The passenger's own
    /// departure, arrival and activities come from other engines, so the test emits them.
    #[deterministic_id_test]
    fn reconstructs_transit_leg_from_transit_engine_events_and_ignores_drivers() {
        use crate::simulation::Identifiable;
        use crate::simulation::controller::ThreadLocalComputationalEnvironment;
        use crate::simulation::pt::transit_engine::TransitEngine;
        use crate::simulation::scenario::network::Network;
        use crate::simulation::scenario::population::Population;
        use crate::simulation::scenario::transit::TransitSchedule;
        use crate::simulation::scenario::vehicles::Garage;
        use crate::simulation::time::{SimClock, Tick};
        use crate::test_utils::create_pt_agent;
        use std::path::{Path, PathBuf};

        let network = Network::from_file("./assets/pt_tutorial/multimodalnetwork.xml");
        let schedule =
            TransitSchedule::from_file(Path::new("./assets/pt_tutorial/transitschedule.xml"));
        let garage = Garage::from_file(&PathBuf::from("./assets/pt_tutorial/transitVehicles.xml"));
        let schedule = Arc::new(TransitSimSchedule::build(
            &schedule,
            &network,
            &garage,
            &Population::default(),
        ));
        let agent = create_pt_agent(1, "Blue Line", "1to3", "1", "3", "11", "33");
        let person = agent.id().clone();
        let pt = Id::<String>::get_from_ext("pt");
        let time = SimTime::from_secs;

        let engine = Rc::new(RefCell::new(BackpackingEngine::new(
            vec![person.clone()],
            schedule.clone(),
        )));
        let mut comp_env = ThreadLocalComputationalEnvironment::default();
        BackpackingEngine::register_event_handlers(
            engine.clone(),
            &mut comp_env.events_manager_borrow_mut(),
        );
        let mut transit = TransitEngine::new(
            schedule,
            Arc::new(garage),
            0,
            comp_env.clone(),
            SimClock::new(1),
        );

        let mut events = comp_env.events_manager_borrow_mut();
        events.reset_iteration(0);
        events.process_event(
            &ActivityEndEventBuilder::default()
                .time(time(21000))
                .person(person.clone())
                .link(Id::create("11"))
                .coordinate(Coordinate::default())
                .act_type(Id::create("home"))
                .build()
                .unwrap(),
        );
        events.process_event(
            &PersonDepartureEventBuilder::default()
                .time(time(21000))
                .person(person.clone())
                .link(Id::create("11"))
                .leg_mode(pt.clone())
                .routing_mode(Some(pt.clone()))
                .build()
                .unwrap(),
        );
        drop(events);

        transit.receive_agent(agent);
        let alighted: Vec<_> = (21000..=23000)
            .flat_map(|tick| transit.do_step(Tick::new(tick)))
            .collect();
        assert_eq!(1, alighted.len());

        let mut events = comp_env.events_manager_borrow_mut();
        events.process_event(
            &PersonArrivalEventBuilder::default()
                .time(time(22140))
                .person(person.clone())
                .link(Id::create("33"))
                .leg_mode(pt.clone())
                .build()
                .unwrap(),
        );
        events.process_event(
            &ActivityStartEventBuilder::default()
                .time(time(22140))
                .person(person.clone())
                .link(Id::create("33"))
                .coordinate(Coordinate::default())
                .act_type(Id::create("work"))
                .build()
                .unwrap(),
        );
        drop(events);

        let experiences = engine.borrow_mut().finish();
        // Drivers have no backpacks and do not show up.
        assert_eq!(1, experiences.len());
        let plan = experiences[&person].plan();
        let InternalPlanElement::Leg(leg) = &plan.elements[1] else {
            panic!("Expected a leg");
        };
        assert_eq!(Some(Duration::from_secs(1140)), leg.trav_time);
        let Some(InternalRoute::Pt(route)) = &leg.route else {
            panic!("Expected a transit route, got {:?}", leg.route);
        };
        assert_eq!("Blue Line", route.description.transit_line_id);
        assert_eq!("1to3", route.description.transit_route_id);
        assert_eq!("1", route.description.access_facility_id);
        assert_eq!("3", route.description.egress_facility_id);
        assert_eq!(Some(time(21600)), route.description.boarding_time);
        assert_eq!(Some(6100.0), route.generic_delegate().distance());
        assert_eq!(&Id::<Link>::get_from_ext("11"), route.start_link());
        assert_eq!(&Id::<Link>::get_from_ext("33"), route.end_link());
    }
}
