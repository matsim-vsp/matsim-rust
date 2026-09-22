mod test_simulation;

use crate::test_simulation::TestExecutorBuilder;

use macros::integration_test;
use rust_qsim::simulation::config::ScoringPlansCollectionType::{HomeSending, Mapping};
use rust_qsim::simulation::config::{CommandLineArgs, Config};
use rust_qsim::simulation::io;
use rust_qsim::simulation::scenario::network::Network;
use rust_qsim::simulation::scenario::population::{InternalPlan, InternalPlanElement, Population};
use rust_qsim::simulation::scenario::vehicles::Garage;
use std::path::PathBuf;
use std::sync::Arc;

#[integration_test(rust_qsim)]
fn test_scoring_backpacking() {
    let single_config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1-scoring.yml",
    ));
    let two_config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-2-scoring.yml",
    ));
    run_and_verify(single_config, two_config);
}

#[integration_test(rust_qsim)]
fn test_scoring_homesending() {
    let mut single_config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1-scoring.yml",
    ));
    single_config.scoring_mut().plans_collection_type = HomeSending;

    let mut two_config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-2-scoring.yml",
    ));
    two_config.scoring_mut().plans_collection_type = HomeSending;

    run_and_verify(single_config, two_config);
}

#[integration_test(rust_qsim)]
fn test_scoring_mapping() {
    let mut single_config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1-scoring.yml",
    ));
    single_config.scoring_mut().plans_collection_type = Mapping;

    let mut two_config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-2-scoring.yml",
    ));
    two_config.scoring_mut().plans_collection_type = Mapping;
    two_config.scoring_mut().collector_threads = 2;

    run_and_verify(single_config, two_config);
}

fn run_and_verify(single_config: Config, two_config: Config) {
    // Single thread
    let single_config = Arc::new(single_config);
    let single_output_dir =
        io::resolve_path(single_config.context(), &single_config.output().output_dir);

    TestExecutorBuilder::default()
        .config(single_config)
        .expected_events(None)
        .build()
        .unwrap()
        .execute();

    // Two threads
    let two_config = Arc::new(two_config);
    let two_output_dir = io::resolve_path(two_config.context(), &two_config.output().output_dir);

    TestExecutorBuilder::default()
        .config(two_config)
        .expected_events(None)
        .build()
        .unwrap()
        .execute();

    let network = Network::from_file_as_is(&single_output_dir.join("equil-network.1.xml"));

    let single_thread_population = Population::from_file(
        &single_output_dir.join("plans/output_plans_0.binpb"),
        &mut Garage::new(),
    );
    let two_thread_population_0 = Population::from_file(
        &two_output_dir.join("plans/output_plans_0.binpb"),
        &mut Garage::new(),
    );
    let two_thread_population_1 = Population::from_file(
        &two_output_dir.join("plans/output_plans_1.binpb"),
        &mut Garage::new(),
    );

    for (person_id, person_a) in &single_thread_population.persons {
        // Search the person in the two-thread population
        let person_b = if two_thread_population_0.persons.contains_key(&person_id) {
            two_thread_population_0.persons.get(&person_id).unwrap()
        } else {
            two_thread_population_1.persons.get(&person_id).unwrap()
        };

        // Make sure, that both have only pne plan
        assert_eq!(
            person_a.plans().len(),
            1,
            "Person (Single partition) {} has {} plans, but test assumes exactly one!",
            person_id,
            person_a.plans().len()
        );
        assert_eq!(
            person_b.plans().len(),
            1,
            "Person (Two partitions) {} has {} plans, but test assumes exactly one!",
            person_id,
            person_b.plans().len()
        );

        // Extract plans
        let person_a_plan = person_a.plans().get(0).unwrap();
        let person_b_plan = person_b.plans().get(0).unwrap();

        // Check if both plans pass integrity check
        check_plan_integrity(person_a_plan, &network);
        check_plan_integrity(person_b_plan, &network);

        // Make sure that the plans are equal -> Number of thread should not change the plans
        check_equal_plans(person_a_plan, person_b_plan);

        // Make sure, that the plans lie in their home partition
        check_plans_at_home(&two_output_dir, &two_thread_population_0, 0);
        check_plans_at_home(&two_output_dir, &two_thread_population_1, 1);
    }
}

/// Checks if the plan components are in correct order and that start/end times do not overlap.
fn check_plan_integrity(plan: &InternalPlan, network: &Network) {
    let elements = &plan.elements;

    assert!(!elements.is_empty(), "Plan is empty.");

    // Assertion 1: must start with an activity that has no start_time
    match &elements[0] {
        InternalPlanElement::Activity(act) => {
            assert!(
                act.start_time.is_none(),
                "First activity must not have a start_time, got {:?}.",
                act.start_time
            );
        }
        InternalPlanElement::Leg(_) => {
            panic!("First element must be an activity, not a leg.")
        }
    }

    // Assertion 2: elements must alternate act/leg/act/leg/...
    for (i, window) in elements.windows(2).enumerate() {
        match (&window[0], &window[1]) {
            (InternalPlanElement::Activity(_), InternalPlanElement::Activity(_)) => {
                panic!("Two consecutive activities at positions {i} and {}.", i + 1);
            }
            (InternalPlanElement::Leg(_), InternalPlanElement::Leg(_)) => {
                panic!("Two consecutive legs at positions {i} and {}.", i + 1);
            }
            _ => {}
        }
    }

    // Assertion 3: times must be contiguous — no gaps or overlaps
    // act.end_time == next_leg.dep_time
    // leg.dep_time + leg.trav_time + 1 == next_act.start_time
    for i in 0..elements.len().saturating_sub(1) {
        match (&elements[i], &elements[i + 1]) {
            (InternalPlanElement::Activity(act), InternalPlanElement::Leg(leg)) => {
                if let (Some(act_end), Some(dep)) = (act.end_time, leg.dep_time) {
                    assert_eq!(
                        act_end, dep,
                        "Activity end_time ({act_end}) != leg dep_time ({dep}) at position {i}."
                    );
                }
            }
            (InternalPlanElement::Leg(leg), InternalPlanElement::Activity(act)) => {
                if let (Some(dep), Some(trav), Some(act_start)) =
                    (leg.dep_time, leg.trav_time, act.start_time)
                {
                    assert_eq!(
                        dep + trav + 1,
                        act_start,
                        "Leg arrival ({dep} + {trav} + 1 = {}) != activity start_time ({act_start}) at position {i}.",
                        dep + trav + 1
                    );
                }
            }
            _ => {}
        }
    }

    // Assertion 4: network routes must form a valid connected sequence of links
    for (i, elem) in elements.iter().enumerate() {
        if let InternalPlanElement::Leg(leg) = elem {
            check_route_integrity(leg, i, network);
        }
    }
}

/// Checks that a leg's network route forms a valid connected sequence of links.
fn check_route_integrity(
    leg: &rust_qsim::simulation::scenario::population::InternalLeg,
    leg_pos: usize,
    network: &Network,
) {
    use rust_qsim::simulation::scenario::population::InternalRoute;

    let Some(InternalRoute::Network(route)) = &leg.route else {
        return;
    };

    let links = route.route();

    if links.is_empty() {
        // Trip stays on the start link - start and end must be the same
        assert_eq!(
            route.generic_delegate().start_link(),
            route.generic_delegate().end_link(),
            "Leg at position {leg_pos}: empty route but start_link ({:?}) != end_link ({:?}).",
            route.generic_delegate().start_link(),
            route.generic_delegate().end_link()
        );
        return;
    }

    // First route link must be reachable from start_link
    let start_to = network
        .get_link(route.generic_delegate().start_link())
        .to
        .clone();
    let first_from = network.get_link(&links[0]).from.clone();
    assert_eq!(
        start_to,
        first_from,
        "Leg at position {leg_pos}: start_link ({:?}) leads to node {start_to:?}, but first route link ({:?}) starts at {first_from:?}.",
        route.generic_delegate().start_link(),
        links[0]
    );

    // Last route link must equal end_link
    assert_eq!(
        links.last().unwrap(),
        route.generic_delegate().end_link(),
        "Leg at position {leg_pos}: last route link ({:?}) != end_link ({:?}).",
        links.last().unwrap(),
        route.generic_delegate().end_link()
    );

    // Consecutive links must share a node
    for j in 0..links.len() - 1 {
        let a_to = network.get_link(&links[j]).to.clone();
        let b_from = network.get_link(&links[j + 1]).from.clone();
        assert_eq!(
            a_to,
            b_from,
            "Leg at position {leg_pos}: route link {j} ({:?}) ends at {a_to:?}, but link {} ({:?}) starts at {b_from:?}.",
            links[j],
            j + 1,
            links[j + 1]
        );
    }
}

/// Checks that two plans are identical element by element.
fn check_equal_plans(plan_a: &InternalPlan, plan_b: &InternalPlan) {
    assert_eq!(
        plan_a.elements.len(),
        plan_b.elements.len(),
        "Plans have different number of elements ({} vs {}).",
        plan_a.elements.len(),
        plan_b.elements.len()
    );

    for (i, (elem_a, elem_b)) in plan_a
        .elements
        .iter()
        .zip(plan_b.elements.iter())
        .enumerate()
    {
        assert_eq!(elem_a, elem_b, "Plans differ at element {i}.");
    }
}

/// Checks that every person in `population` has their first activity on a link belonging to `expected_partition`.
fn check_plans_at_home(output_dir: &PathBuf, population: &Population, expected_partition: u32) {
    let net = Network::from_file_as_is(&output_dir.join("equil-network.2.xml"));

    for (person_id, person) in &population.persons {
        let plan = person.plans().get(0).unwrap();
        let first_act = plan.elements[0]
            .as_activity()
            .expect("First plan element is not an activity.");
        let partition = net.get_link(&first_act.link_id).partition;
        assert_eq!(
            partition, expected_partition,
            "Person {person_id}: first activity is on link {:?} in partition {partition}, expected {expected_partition}.",
            first_act.link_id
        );
    }
}
