use macros::deterministic_id_test;
use matsim_rust::simulation::config::{CommandLineArgs, Config, WriteEvents};
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::scenario::Scenario;
use matsim_rust::simulation::scenario::population::{InternalPlanElement, Population};

fn run_sim(config_path: &str, num_parts: u32, global_sync: bool, output_dir: &str) -> Population {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(config_path));
    config.partitioning_mut().num_parts = num_parts;
    config.computational_setup_mut().global_sync = global_sync;
    config.output_mut().output_dir = output_dir.into();
    config.output_mut().write_events = WriteEvents::None;
    let (_, population) = ControllerBuilder::default_with_scenario(Scenario::load(config))
        .build()
        .unwrap()
        .run();
    population
}

#[deterministic_id_test(matsim_rust)]
fn partition_invariance_and_witness_completion() {
    let path = "./tests/resources/pt_simulated/queue_stranded.yml";
    let base_population = run_sim(path, 1, false, "./test_output/abandoned_legs/stranded_p1");

    let count_completed_and_aborted = |pop: &Population| -> (usize, usize) {
        let mut completed = 0;
        let mut aborted = 0;
        for person in pop.persons.values() {
            let Some(plan) = person.selected_plan() else {
                continue;
            };
            for element in &plan.elements {
                if let InternalPlanElement::Leg(leg) = element {
                    if leg.attributes.get::<bool>("aborted") == Some(true) {
                        aborted += 1;
                    } else if leg.trav_time.is_some() {
                        completed += 1;
                    }
                }
            }
        }
        (completed, aborted)
    };

    let (base_completed, base_aborted) = count_completed_and_aborted(&base_population);
    assert_eq!(base_completed, 14);
    assert_eq!(base_aborted, 0);

    // Witness check on person 102
    let person_102 = base_population
        .persons
        .values()
        .find(|p| p.id().external() == "102")
        .unwrap();
    let plan_102 = person_102.selected_plan().unwrap();
    let legs_102: Vec<_> = plan_102
        .elements
        .iter()
        .filter_map(|e| match e {
            InternalPlanElement::Leg(leg) => Some(leg),
            _ => None,
        })
        .collect();
    assert_eq!(legs_102.len(), 7);
    for leg in &legs_102 {
        assert!(
            leg.trav_time.is_some(),
            "Witness 102 completed leg must have travel time"
        );
        assert_eq!(
            leg.attributes.get::<bool>("aborted"),
            None,
            "Witness 102 completed leg must not be aborted"
        );
    }

    // Verify invariance across partition counts and synchronization barrier settings
    for num_parts in [2, 4] {
        for global_sync in [false, true] {
            let output_dir =
                format!("./test_output/abandoned_legs/stranded_p{num_parts}_sync_{global_sync}");
            let pop = run_sim(path, num_parts, global_sync, &output_dir);
            let (completed, aborted) = count_completed_and_aborted(&pop);
            assert_eq!(
                completed, base_completed,
                "Completed leg count must be partition-invariant (parts={num_parts}, sync={global_sync})"
            );
            assert_eq!(
                aborted, base_aborted,
                "Aborted leg count must be partition-invariant (parts={num_parts}, sync={global_sync})"
            );

            let p102 = pop
                .persons
                .values()
                .find(|p| p.id().external() == "102")
                .unwrap();
            let p102_plan = p102.selected_plan().unwrap();
            let p102_legs: Vec<_> = p102_plan
                .elements
                .iter()
                .filter_map(|e| match e {
                    InternalPlanElement::Leg(leg) => Some(leg),
                    _ => None,
                })
                .collect();
            assert_eq!(p102_legs.len(), 7);
            for leg in &p102_legs {
                assert!(
                    leg.trav_time.is_some(),
                    "Multi-partition completed leg must keep travel time"
                );
                assert_eq!(
                    leg.attributes.get::<bool>("aborted"),
                    None,
                    "Multi-partition completed leg must not be aborted"
                );
            }
        }
    }
}
