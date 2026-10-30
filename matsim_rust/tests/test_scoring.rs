use macros::deterministic_id_test;
use matsim_rust::simulation::config::{CommandLineArgs, Config, ScoringMode};
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::id::Id;
use matsim_rust::simulation::io;
use matsim_rust::simulation::scenario::Scenario;
use matsim_rust::simulation::scenario::network::Network;
use matsim_rust::simulation::scenario::population::{
    InternalLeg, InternalPerson, InternalPlan, InternalPlanElement, InternalRoute, Population,
};
use matsim_rust::simulation::scenario::vehicles::Garage;
use matsim_rust::simulation::scoring::{OnlyTravelTimeDependentScoring, PlanScorer};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// This is just a regression test to ensure backpacking produces experienced plans.
#[deterministic_id_test(matsim_rust)]
fn backpacking_produces_partition_independent_experienced_plans() {
    let single = run_and_load("./tests/resources/equil/equil-config-1-scoring.yml");
    let partitioned = run_and_load("./tests/resources/equil/equil-config-2-scoring.yml");
    let network = Network::from_file(Path::new("./assets/equil/equil-network.xml"));

    assert!(!single.persons.is_empty());
    assert_eq!(single, partitioned);

    for person in single.persons.values() {
        assert_eq!(person.plans().len(), 1);
        check_plan_integrity(&person.plans()[0], &network);
    }
}

/// Compares the experienced plans of all 100 equil agents with a stored reference, so that changes
/// to the event handling of backpacking can't alter the reconstructed plans or their scores
/// unnoticed. After an intended change of the result, replace the reference with the decompressed
/// `test_output/simulation/scoring_reference_1/output_experienced_plans.xml.zst`.
#[deterministic_id_test(matsim_rust)]
fn full_equil_population_matches_reference_experienced_plans() {
    let mut experienced = Vec::new();
    for parts in [1, 2] {
        let mut config = Config::from_args(CommandLineArgs::new_with_path(format!(
            "./tests/resources/equil/equil-config-{parts}-scoring.yml"
        )));
        config.population_mut().path = Some("./assets/equil/equil-plans.xml.gz".into());
        config.output_mut().output_dir =
            format!("./test_output/simulation/scoring_reference_{parts}").into();
        experienced.push(run_config_and_load(config));
    }
    // Load the reference after the runs, so that it doesn't change the order in which ids are created.
    let expected = load_population(&PathBuf::from(
        "./tests/resources/equil/expected_experienced_plans.xml",
    ));

    assert_eq!(expected.persons.len(), 100);
    for population in experienced {
        assert_eq!(population, expected);
    }
}

#[deterministic_id_test(matsim_rust)]
fn controller_builder_uses_custom_travel_time_scorer() {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1-scoring.yml",
    ));
    config.controller_mut().last_iteration = 0;
    config.output_mut().output_dir = "./test_output/simulation/scoring_travel_time_override".into();
    let output_dir = io::resolve_path(config.context(), &config.output().output_dir);
    let scenario = Scenario::load(config);
    ControllerBuilder::default_with_scenario(scenario)
        .scoring_function(Box::new(OnlyTravelTimeDependentScoring))
        .build()
        .unwrap()
        .run();

    let experienced = load_population(&output_dir.join("output_experienced_plans.xml.zst"));
    let selected = load_population(&output_dir.join("output_plans.xml.zst"));
    assert!(!experienced.persons.is_empty());

    let mut has_trip = false;
    for (person_id, person) in &experienced.persons {
        let score = person.selected_plan().unwrap().score.unwrap();
        assert!(score <= 0.0);
        has_trip |= score < 0.0;
        assert_eq!(
            Some(score),
            selected
                .persons
                .get(person_id)
                .unwrap()
                .selected_plan()
                .unwrap()
                .score
        );
    }
    assert!(has_trip, "Expected at least one completed trip");
}

#[deterministic_id_test(matsim_rust)]
fn disabled_scoring_skips_backpacking_and_clears_scores() {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1-scoring.yml",
    ));
    config.scoring_mut().mode = ScoringMode::Disabled;
    config.output_mut().output_dir = "./test_output/simulation/scoring_disabled".into();
    let output_dir = io::resolve_path(config.context(), &config.output().output_dir);
    let scenario = Scenario::load(config);
    ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap()
        .run();

    // The input plans carry scores, so this also checks that the selected scores are cleared.
    let selected = load_population(&output_dir.join("output_plans.xml.zst"));
    assert!(!selected.persons.is_empty());
    for person in selected.persons.values() {
        assert_eq!(person.selected_plan().unwrap().score, None);
    }
    for file in [
        output_dir.join("output_experienced_plans.xml.zst"),
        output_dir
            .join("ITERS")
            .join("it.1")
            .join("output_experienced_plans.xml.zst"),
    ] {
        assert!(!file.exists(), "Unexpected {}", file.display());
    }
}

/// Scores each person with the number of times it was scored before, which is the iteration index
/// when the first iteration is 0.
#[derive(Default)]
struct ScoringCountScorer {
    counts: Mutex<HashMap<String, u32>>,
}

impl PlanScorer for ScoringCountScorer {
    fn score(
        &self,
        person_id: &Id<InternalPerson>,
        _subpopulation: &str,
        _experienced_plan: &InternalPlan,
    ) -> Result<f64, String> {
        let mut counts = self.counts.lock().unwrap();
        let count = counts.entry(person_id.external().to_string()).or_default();
        let score = f64::from(*count);
        *count += 1;
        Ok(score)
    }
}

#[deterministic_id_test(matsim_rust)]
fn score_msa_averages_selected_plan_scores_but_not_experienced_scores() {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1-scoring.yml",
    ));
    config.controller_mut().last_iteration = 2;
    // MSA starts in iteration (2 - 0) * 0.5 + 0 = 1.
    config
        .scoring_mut()
        .fraction_of_iterations_to_start_score_msa = Some(0.5);
    config.output_mut().output_dir = "./test_output/simulation/scoring_msa".into();
    let output_dir = io::resolve_path(config.context(), &config.output().output_dir);
    let scenario = Scenario::load(config);
    ControllerBuilder::default_with_scenario(scenario)
        .scoring_function(Box::new(ScoringCountScorer::default()))
        .build()
        .unwrap()
        .run();

    let experienced = load_population(&output_dir.join("output_experienced_plans.xml.zst"));
    let selected = load_population(&output_dir.join("output_plans.xml.zst"));
    assert!(!selected.persons.is_empty());
    for (person_id, person) in &selected.persons {
        // Iteration 0 replaces the input score with 0. MSA then averages the scores 1 and 2 of
        // iterations 1 and 2; the first MSA contribution discards the score of iteration 0.
        assert_eq!(Some(1.5), person.selected_plan().unwrap().score);
        // Experienced plans keep the raw score of the last iteration.
        assert_eq!(
            Some(2.0),
            experienced
                .persons
                .get(person_id)
                .unwrap()
                .selected_plan()
                .unwrap()
                .score
        );
    }
}

fn run_and_load(config_path: &str) -> Population {
    let config = Config::from_args(CommandLineArgs::new_with_path(config_path));
    run_config_and_load(config)
}

fn run_config_and_load(config: Config) -> Population {
    let output_dir = io::resolve_path(config.context(), &config.output().output_dir);
    let scenario = Scenario::load(config);
    run_scenario_and_load(scenario, output_dir)
}

fn run_scenario_and_load(scenario: Scenario, output_dir: PathBuf) -> Population {
    ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap()
        .run();

    let root_file = output_dir.join("output_experienced_plans.xml.zst");
    let iteration_file = output_dir
        .join("ITERS")
        .join("it.1")
        .join("output_experienced_plans.xml.zst");
    assert!(root_file.exists(), "Missing {}", root_file.display());
    assert!(
        iteration_file.exists(),
        "Missing {}",
        iteration_file.display()
    );

    let root_population = load_population(&root_file);
    let iteration_population = load_population(&iteration_file);
    assert_eq!(root_population, iteration_population);
    let output_population = load_population(&output_dir.join("output_plans.xml.zst"));
    for (person_id, experienced_person) in &root_population.persons {
        assert_eq!(
            experienced_person.selected_plan().unwrap().score,
            output_population
                .persons
                .get(person_id)
                .unwrap()
                .selected_plan()
                .unwrap()
                .score
        );
    }
    root_population
}

fn load_population(path: &PathBuf) -> Population {
    Population::from_file(path, &mut Garage::new())
}

fn check_plan_integrity(plan: &InternalPlan, network: &Network) {
    assert!(!plan.elements.is_empty(), "Experienced plan is empty");
    assert!(plan.score.is_some_and(f64::is_finite));
    assert!(matches!(
        plan.elements.first(),
        Some(InternalPlanElement::Activity(activity)) if activity.start_time.is_none()
    ));
    assert!(matches!(
        plan.elements.last(),
        Some(InternalPlanElement::Activity(activity)) if activity.end_time.is_none()
    ));

    for (index, pair) in plan.elements.windows(2).enumerate() {
        match (&pair[0], &pair[1]) {
            (InternalPlanElement::Activity(activity), InternalPlanElement::Leg(leg)) => {
                assert_eq!(
                    activity.end_time, leg.dep_time,
                    "Activity/leg times differ at element {index}"
                );
                check_route_integrity(leg, network);
            }
            (InternalPlanElement::Leg(leg), InternalPlanElement::Activity(activity)) => {
                let departure = leg
                    .dep_time
                    .unwrap_or_else(|| panic!("Leg at element {index} has no departure time"));
                let travel_time = leg
                    .trav_time
                    .unwrap_or_else(|| panic!("Leg at element {index} has no travel time"));
                // The activity starts in the tick in which the leg ends (`T1`).
                assert_eq!(
                    Some(departure.saturating_add(travel_time)),
                    activity.start_time,
                    "Leg/activity times differ at element {index}"
                );
            }
            _ => panic!("Experienced plan does not alternate at element {index}"),
        }
    }
}

fn check_route_integrity(leg: &InternalLeg, network: &Network) {
    let Some(InternalRoute::Network(route)) = &leg.route else {
        return;
    };
    let links = route.route();
    let start_link = route.generic_delegate().start_link();
    let end_link = route.generic_delegate().end_link();

    assert!(!links.is_empty(), "Network route is empty");
    assert_eq!(links.first(), Some(start_link));
    assert_eq!(links.last(), Some(end_link));
    if start_link == end_link {
        assert_eq!(links.len(), 1, "Same-link route must contain one link");
    }

    for pair in links.windows(2) {
        assert_eq!(
            network.get_link(&pair[0]).to,
            network.get_link(&pair[1]).from,
            "Network route is disconnected between {} and {}",
            pair[0],
            pair[1]
        );
    }
}
