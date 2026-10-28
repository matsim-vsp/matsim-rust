use super::*;
use crate::simulation::config::TrafficDynamics;
use crate::test_utils;
use macros::deterministic_id_test;

fn link(length: f64, sample: f64, ticks: u32) -> LocalLink {
    let mut config = test_utils::qsim_config();
    config.traffic_dynamics = TrafficDynamics::KinematicWaves;
    config.sample_size = sample;
    config.ticks_per_second = ticks;
    // q_in = (1 / 10) / (0.24 + 0.16) = 0.25 PCE/s before sampling.
    LocalLink::build(Id::create("waves-link"), 900., 6.25, 1., length, 10., &config,
        Id::create("from"), Id::create("to"))
}

fn vehicle(id: u64, pce: f64, arrives: bool) -> SimulationVehicle {
    let route = if arrives { vec!["waves-link"] } else { vec!["waves-link", "next"] };
    SimulationVehicle::from_parts(id, 0, 100., pce, test_utils::create_agent(id, route))
}

#[deterministic_id_test]
fn inflow_is_exhausted_with_free_storage_and_refills_lazily() {
    for sample in [1., 0.5] {
        for ticks in [1, 4] {
            let mut link = link(100., sample, ticks);
            assert!(link.is_available(0));
            link.push_veh(vehicle(1, 1., false), 0, LinkPosition::QStart);
            assert!(link.storage_cap.is_available(Tick::zero()));
            let next = (4. / sample) as u64 * ticks as u64;
            assert!(!link.is_available(next - 1));
            assert!(!link.is_available(next - 1));
            assert!(link.is_available(next));
            link.push_veh(vehicle(2, 1., false), next, LinkPosition::QStart);
            assert!(!link.is_available(next));
            assert!(link.is_available(1_000_000));
            assert!(link.is_available(1_000_000));
        }
    }
}

#[deterministic_id_test]
fn destination_and_buffer_both_create_gaps_without_keeping_empty_links_active() {
    for arrives in [false, true] {
        let mut link = link(25., 1., 1);
        link.push_veh(vehicle(1, 2.5, arrives), 0, LinkPosition::QStart);
        let ended = link.do_sim_step(4, &mut Default::default());
        assert_eq!(usize::from(arrives), ended.len());
        assert!(!link.is_available(9));
        if !arrives {
            assert_eq!(1, link.buffer.len());
            assert!(link.pop_veh_and_restart_stuck_timer(5).is_some());
        }
        assert!(!link.is_active());
        // No link steps between release and availability.
        assert!(link.is_available(10));
        assert!(link.is_available(10));
        assert_eq!(0., link.storage_cap.used());
    }
}

#[deterministic_id_test]
fn admission_query_updates_a_sleeping_link_without_waking_it() {
    let mut link = link(25., 1., 1);
    link.push_veh(vehicle(1, 2.5, true), 0, LinkPosition::QStart);
    assert_eq!(1, link.do_sim_step(4, &mut Default::default()).len());
    link.push_veh(vehicle(2, 1., false), 100, LinkPosition::QStart);
    link.do_sim_step(101, &mut Default::default());
    assert_eq!(Some(Tick::new(104)), link.fall_asleep(Tick::new(101)));
    assert!(link.is_sleeping());
    assert!(!link.is_available(102));
    assert_eq!(1., link.storage_cap.used());
    assert!(link.is_sleeping());
}

#[deterministic_id_test]
fn departures_use_neither_storage_nor_inflow_budget() {
    let mut link = link(25., 1., 1);
    let inflow = link.inflow_cap.as_ref().unwrap().accumulated();
    link.push_veh(vehicle(1, 2.5, false), 0, LinkPosition::Waiting);
    link.do_sim_step(0, &mut Default::default());
    assert_eq!(1, link.buffer.len());
    assert_eq!(0., link.storage_cap.used());
    assert_eq!(inflow, link.inflow_cap.as_ref().unwrap().accumulated());
    assert!(link.is_available(0));
}

#[deterministic_id_test]
fn local_and_split_storage_admit_on_identical_ticks() {
    for ticks in [1, 4, 10] {
        for length in [0., 0.01, 25., 25.01] {
            for sample in [0.1, 1.] {
                let mut local = link(length, sample, ticks);
                let mut incoming = SplitInLink::new(0, link(length, sample, ticks));
                let mut outgoing = SplitOutLink {
                    id: local.id.clone(), from: local.from.clone(), to_part: 1,
                    q: VecDeque::new(), storage_cap: local.storage_cap.clone(),
                    inflow_cap: local.inflow_cap.clone(),
                };
                // Fill storage, including oversubscription by a large vehicle. Both admission
                // sides consume the identical entry budget, the receiver only moves the queue.
                let pce = local.storage_cap.max().max(2.5);
                local.push_veh(vehicle(1, pce, true), 0, LinkPosition::QStart);
                outgoing.push_veh(vehicle(1, pce, true), LinkPosition::QStart, Tick::zero());
                assert_eq!(1, outgoing.take_veh().len());
                incoming.local_link.push_veh(vehicle(1, pce, true), 0, LinkPosition::QStart);
                let due = local.q.front().unwrap().earliest_exit_time;
                local.do_sim_step(due, &mut Default::default());
                incoming.local_link.do_sim_step(due, &mut Default::default());
                assert!(!incoming.local_link.is_active());
                let releases: Vec<_> = incoming.drain_storage_releases().unwrap().collect();
                assert_eq!(1, releases.len());
                // The message is applied after this tick's node/link phases, as in LegEngine.
                for release in releases {
                    outgoing.apply_storage_cap_update(release.pce, release.available_at);
                }
                assert!(incoming.drain_storage_releases().unwrap().next().is_none());
                let mut first = None;
                for now in due.value() + 1..=10_000 {
                    let available = local.is_available(now);
                    assert_eq!(available, outgoing.is_available(Tick::new(now)),
                        "length={length}, ticks={ticks}, sample={sample}, now={now}");
                    if available { first = Some(now); break; }
                }
                assert!(first.is_some());
                assert_eq!(0., local.storage_cap.used());
                assert_eq!(0., outgoing.storage_cap.used());
            }
        }
    }
}

#[deterministic_id_test]
fn inflow_refills_while_the_downstream_buffer_remains_occupied() {
    let mut link = link(100., 1., 1);
    link.push_veh(vehicle(1, 1., false), 0, LinkPosition::QStart);
    link.do_sim_step(16, &mut Default::default());
    assert_eq!(1, link.buffer.len());
    assert!(link.is_available(20));
    assert_eq!(0.25, link.inflow_cap.as_ref().unwrap().accumulated());
}
