use crate::simulation::config;
use crate::simulation::id::Id;
use crate::simulation::random::get_rng;
use crate::simulation::replanning::routing::TripRouter;
use crate::simulation::scenario::ScenarioCore;
use crate::simulation::scenario::population::{DEFAULT_SUBPOPULATION, InternalPerson, Population};
use crate::simulation::scenario::prepare_for_sim::{
    PrepareForSimContext, TripPreparationError, route_trip,
};
use crate::simulation::scenario::trip_structure_utils::{
    get_trip_spans_default, identify_main_mode,
};
use derive_builder::Builder;
use nohash_hasher::IntMap;
use rand::RngExt;
use rayon::prelude::*;
use selectors::{DefaultSelector, KeepLastSelector, WorstScoreSelector};
use std::fmt;
use std::str::FromStr;

pub mod routing;
pub mod selectors;

const STRATEGY_RNG_PURPOSE: &str = "replanning.strategy";
const RANDOM_SELECTOR_RNG_PURPOSE: &str = "replanning.selector.random";
pub const KEEP_LAST_SELECTED_STRATEGY_NAME: &str = "KeepLastSelected";
pub const BEST_SCORE_STRATEGY_NAME: &str = "BestScore";
pub const SELECT_RANDOM_STRATEGY_NAME: &str = "SelectRandom";
pub const WORST_SCORE_STRATEGY_NAME: &str = "WorstScore";
pub const RE_ROUTE_STRATEGY_NAME: &str = "ReRoute";
pub const SELECT_EXP_BETA_STRATEGY_NAME: &str = "SelectExpBeta";

#[allow(dead_code)]
/// This is responsible for picking a plan, copying it, and replanning it.
trait PlanStrategy: Send + Sync {
    fn name(&self) -> &Id<String>;
    fn handle(&self, person: &mut InternalPerson, context: &ReplanningContext);
}

#[allow(dead_code)]
/// This is the smallest replanning unit (e.g., routes a plan).
trait PlanStrategyModule: Send + Sync {
    fn handle(&self, person: &mut InternalPerson, plan_index: usize);
}

/// This is responsible for selecting a plan from a person's available plans.
trait PlanSelector: Send + Sync {
    fn select(&self, person: &InternalPerson, context: &ReplanningContext) -> usize;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefaultStrategy {
    ReRoute,
}

impl DefaultStrategy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReRoute => RE_ROUTE_STRATEGY_NAME,
        }
    }

    fn as_generic_plan_strategy(
        self,
        trip_router: TripRouter,
        scenario_core: ScenarioCore,
    ) -> Box<dyn PlanStrategy> {
        match self {
            Self::ReRoute => Box::new(GenericPlanStrategy {
                name: Id::create(self.as_str()),
                selector: Box::new(KeepLastSelector),
                modules: vec![Box::new(ReRouteModule::new(trip_router, scenario_core))],
            }),
        }
    }
}

impl fmt::Display for DefaultStrategy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for DefaultStrategy {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            RE_ROUTE_STRATEGY_NAME => Ok(Self::ReRoute),
            _ => Err(format!("Unknown DefaultStrategy: {value}")),
        }
    }
}

/// Performs multithreaded replanning of the population. Rayon pool is started in the controller.
pub(crate) fn replan_population(
    population: Population,
    iteration: u32,
    base_seed: u64,
    strategy_manager: &StrategyManager,
    innovation_disabled: bool,
) -> Population {
    let persons = population
        .persons
        .into_par_iter()
        .map(|(id, mut person)| {
            strategy_manager.run(iteration, base_seed, innovation_disabled, &mut person);
            (id, person)
        })
        .collect();

    Population { persons }
}

#[allow(dead_code)]
#[derive(Builder)]
#[builder(pattern = "owned")]
/// Manages replanning. This is the registry for all the replanning strategies.
pub(crate) struct StrategyManager {
    #[builder(default = "default_weights_per_subpopulation()")]
    weights_per_subpopulation: IntMap<Id<String>, StrategyWeights>,
    #[builder(default = "default_max_memory_size()")]
    max_memory_size: usize,
    #[builder(default = "default_plan_remover()")]
    plan_remover: Box<dyn PlanSelector>,
    #[builder(default = "default_strategies(TripRouter::default(), ScenarioCore::default())")]
    strategies: IntMap<Id<String>, Box<dyn PlanStrategy>>,
}

impl StrategyManager {
    pub(crate) fn from_replanning_config(
        replanning: &config::Replanning,
        trip_router: TripRouter,
        scenario_core: &ScenarioCore,
    ) -> Self {
        let weights_per_subpopulation =
            weights_per_subpopulation_from_settings(&replanning.strategy_settings);

        StrategyManagerBuilder::default()
            .weights_per_subpopulation(weights_per_subpopulation)
            .max_memory_size(replanning.max_agent_plan_memory as usize)
            .plan_remover(plan_selector_from_config_name(
                &replanning.plan_selector_for_removal,
            ))
            .strategies(default_strategies(trip_router, scenario_core.clone()))
            .build()
            .unwrap()
    }

    fn run(
        &self,
        iteration: u32,
        base_seed: u64,
        innovation_disabled: bool,
        person: &mut InternalPerson,
    ) {
        let context = ReplanningContext {
            iteration,
            base_seed,
            innovation_disabled,
        };

        if let Some(strategy) = self.choose_strategy(&context, person) {
            strategy.handle(person, &context);
        }
        self.remove_plans_if_needed(person, &context);
    }

    /// Chooses a strategy and runs it.
    fn choose_strategy(
        &self,
        context: &ReplanningContext,
        person: &InternalPerson,
    ) -> Option<&dyn PlanStrategy> {
        let weights = self.weights_per_subpopulation.get(person.subpopulation())?;
        let allowed_entries = weights
            .entries
            .iter()
            .filter(|entry| entry.weight > 0.0)
            .filter(|entry| {
                !context.innovation_disabled || is_non_innovative_strategy(&entry.strategy_name)
            })
            .collect::<Vec<_>>();

        let total_weight: f64 = allowed_entries.iter().map(|entry| entry.weight).sum();
        if total_weight <= 0.0 {
            return None;
        }

        let stream_id = format!("{}:{}", context.iteration, person.id().external());
        let mut rng = get_rng(context.base_seed, STRATEGY_RNG_PURPOSE, &stream_id);

        // Weighted random selection over positive strategy weights.
        let mut draw = rng.random_range(0.0..total_weight);
        for entry in &allowed_entries {
            if draw < entry.weight {
                return Some(self.strategy_by_name(&entry.strategy_name));
            }
            draw -= entry.weight;
        }

        allowed_entries
            .into_iter()
            .rev()
            .map(|entry| self.strategy_by_name(&entry.strategy_name))
            .next()
    }

    fn remove_plans_if_needed(&self, person: &mut InternalPerson, context: &ReplanningContext) {
        while person.plans().len() > self.max_memory_size {
            let index = self.plan_remover.select(person, context);
            person.plans_mut().remove(index);
        }
    }

    fn strategy_by_name(&self, strategy_name: &Id<String>) -> &dyn PlanStrategy {
        self.strategies
            .get(strategy_name)
            .map(Box::as_ref)
            .unwrap_or_else(|| panic!("No replanning strategy registered for {strategy_name}"))
    }
}

impl Default for StrategyManager {
    fn default() -> Self {
        StrategyManagerBuilder::default().build().unwrap()
    }
}

fn default_weights_per_subpopulation() -> IntMap<Id<String>, StrategyWeights> {
    let mut weights_per_subpopulation = IntMap::default();
    weights_per_subpopulation.insert(
        Id::create(DEFAULT_SUBPOPULATION),
        StrategyWeights::new(vec![StrategyWeight::new(
            Id::create(DefaultSelector::KeepLastSelected.as_str()),
            1.0,
        )]),
    );
    weights_per_subpopulation
}

fn default_max_memory_size() -> usize {
    5
}

fn default_plan_remover() -> Box<dyn PlanSelector> {
    Box::new(WorstScoreSelector)
}

fn default_strategies(
    trip_router: TripRouter,
    scenario_core: ScenarioCore,
) -> IntMap<Id<String>, Box<dyn PlanStrategy>> {
    let mut strategies = IntMap::default();
    for selector in [
        DefaultSelector::KeepLastSelected,
        DefaultSelector::BestScore,
        DefaultSelector::SelectRandom,
        DefaultSelector::WorstScore,
        DefaultSelector::SelectExpBeta,
    ] {
        strategies.insert(
            Id::create(selector.as_str()),
            selector.as_generic_plan_strategy(),
        );
    }
    for strategy in [DefaultStrategy::ReRoute] {
        strategies.insert(
            Id::create(strategy.as_str()),
            strategy.as_generic_plan_strategy(trip_router.clone(), scenario_core.clone()),
        );
    }
    strategies
}

fn weights_per_subpopulation_from_settings(
    settings: &[config::StrategySetting],
) -> IntMap<Id<String>, StrategyWeights> {
    let mut weights_per_subpopulation = IntMap::default();
    for setting in settings {
        assert_known_strategy_name(&setting.name);
        weights_per_subpopulation
            .entry(Id::create(&setting.subpopulation))
            .or_insert_with(|| StrategyWeights::new(Vec::new()))
            .entries
            .push(StrategyWeight::new(
                Id::create(&setting.name),
                setting.weight,
            ));
    }
    weights_per_subpopulation
}

fn assert_known_strategy_name(name: &str) {
    if DefaultSelector::from_str(name).is_ok() || DefaultStrategy::from_str(name).is_ok() {
        return;
    }
    panic!("Unknown replanning strategy or selector configured: {name}");
}

fn plan_selector_from_config_name(name: &str) -> Box<dyn PlanSelector> {
    DefaultSelector::from_str(name)
        .map(DefaultSelector::as_plan_selector)
        .unwrap_or_else(|_| panic!("Unknown plan_selector_for_removal configured: {name}"))
}

fn is_non_innovative_strategy(strategy_name: &Id<String>) -> bool {
    DefaultSelector::from_str(strategy_name.external()).is_ok()
}

struct StrategyWeights {
    entries: Vec<StrategyWeight>,
}

impl StrategyWeights {
    fn new(entries: Vec<StrategyWeight>) -> Self {
        Self { entries }
    }
}

struct StrategyWeight {
    strategy_name: Id<String>,
    weight: f64,
}

impl StrategyWeight {
    fn new(strategy_name: Id<String>, weight: f64) -> Self {
        Self {
            strategy_name,
            weight,
        }
    }
}

// Different modules can be combined to create a strategy.
struct GenericPlanStrategy {
    name: Id<String>,
    selector: Box<dyn PlanSelector + Send + Sync>,
    modules: Vec<Box<dyn PlanStrategyModule + Send + Sync>>,
}

impl PlanStrategy for GenericPlanStrategy {
    fn name(&self) -> &Id<String> {
        &self.name
    }

    fn handle(&self, person: &mut InternalPerson, context: &ReplanningContext) {
        let plan_index = self.selector.select(person, context);
        person.mark_plan_as_selected(plan_index);

        if self.modules.is_empty() {
            return;
        }

        let new_plan = person
            .plans()
            .get(plan_index)
            .cloned()
            .unwrap_or_else(|| panic!("Selected plan index {plan_index} does not exist."));
        let new_plan_index = person.add_new_plan_as_selected(new_plan);

        for module in &self.modules {
            module.handle(person, new_plan_index);
        }
    }
}

#[allow(dead_code)]
struct ReRouteModule {
    router: TripRouter,
    scenario_core: ScenarioCore,
}

impl ReRouteModule {
    fn new(router: TripRouter, scenario_core: ScenarioCore) -> Self {
        Self {
            router,
            scenario_core,
        }
    }
}

impl PlanStrategyModule for ReRouteModule {
    fn handle(&self, person: &mut InternalPerson, plan_index: usize) {
        let context = PrepareForSimContext {
            network: &self.scenario_core.network,
            garage: &self.scenario_core.garage,
            config: &self.scenario_core.config,
        };
        let trip_count = get_trip_spans_default(&person.plans()[plan_index].elements).len();

        // Route complete trips so access/egress legs and stage activities change together.
        // Recompute spans after each replacement because routing can change their lengths.
        for trip_index in 0..trip_count {
            let (span, new_elements) = {
                let plan = &person.plans()[plan_index];
                let span = get_trip_spans_default(&plan.elements)[trip_index];
                let trip_elements = span.trip_elements(&plan.elements);
                let result = if !trip_elements
                    .iter()
                    .any(|element| element.as_leg().is_some())
                {
                    Err(TripPreparationError::NoLegs)
                } else {
                    identify_main_mode(trip_elements)
                        .map(|mode| Id::get_from_ext(&mode))
                        .ok_or(TripPreparationError::AmbiguousMainMode)
                        .and_then(|mode| {
                            route_trip(&context, person, plan, span, &mode, &self.router)
                        })
                };
                let new_elements = result.unwrap_or_else(|error| {
                    panic!(
                        "ReRoute failed for person {}, plan {plan_index}, trip {trip_index}: {error}",
                        person.id().external()
                    )
                });
                (span, new_elements)
            };
            span.replace_trip_elements(&mut person.plans_mut()[plan_index].elements, new_elements);
        }

        person.plans_mut()[plan_index].score = None;
    }
}

struct ReplanningContext {
    iteration: u32,
    base_seed: u64,
    innovation_disabled: bool,
}

#[cfg(test)]
mod tests {
    use super::{
        DefaultStrategy, GenericPlanStrategy, PlanStrategy, PlanStrategyModule, ReplanningContext,
        StrategyManager,
    };
    use crate::simulation::config::{Config, Replanning, StrategySetting};
    use crate::simulation::id::Id;
    use crate::simulation::replanning::routing::TripRouter;
    use crate::simulation::replanning::routing::teleportation::TeleportationRoutingModule;
    use crate::simulation::replanning::routing::{RoutingError, RoutingModule, RoutingRequest};
    use crate::simulation::replanning::selectors::{DefaultSelector, KeepLastSelector};
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::ScenarioCore;
    use crate::simulation::scenario::population::{
        InternalActivity, InternalGenericRoute, InternalLeg, InternalNetworkRoute, InternalPerson,
        InternalPlan, InternalPlanElement, InternalRoute,
    };
    use crate::simulation::scenario::trip_structure_utils::get_trip_spans_default;
    use crate::simulation::scenario::vehicles::{Garage, InternalVehicle, InternalVehicleType};
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use nohash_hasher::IntMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[deterministic_id_test]
    fn default_selectors_create_generic_strategies_with_matching_names() {
        for selector in [
            DefaultSelector::KeepLastSelected,
            DefaultSelector::BestScore,
            DefaultSelector::SelectRandom,
        ] {
            let strategy = selector.as_generic_plan_strategy();

            assert_eq!(&Id::create(selector.as_str()), strategy.name());
        }
    }

    #[deterministic_id_test]
    fn config_manager_uses_memory_limit_and_removal_selector() {
        let replanning = Replanning {
            max_agent_plan_memory: 1,
            plan_selector_for_removal: DefaultSelector::BestScore.as_str().to_string(),
            ..Replanning::default()
        };
        let manager = StrategyManager::from_replanning_config(
            &replanning,
            TripRouter::default(),
            &ScenarioCore::default(),
        );
        let mut person = person_with_scores([Some(1.0), Some(2.0)]);

        manager.run(0, 42, false, &mut person);

        assert_eq!(1, person.plans().len());
        assert_eq!(Some(1.0), person.plans()[0].score);
    }

    #[deterministic_id_test]
    fn config_manager_groups_strategy_weights_by_subpopulation() {
        let replanning = Replanning {
            strategy_settings: vec![
                StrategySetting {
                    name: DefaultSelector::KeepLastSelected.as_str().to_string(),
                    weight: 0.3,
                    subpopulation: "person".to_string(),
                },
                StrategySetting {
                    name: DefaultSelector::BestScore.as_str().to_string(),
                    weight: 0.7,
                    subpopulation: "freight".to_string(),
                },
            ],
            ..Replanning::default()
        };
        let manager = StrategyManager::from_replanning_config(
            &replanning,
            TripRouter::default(),
            &ScenarioCore::default(),
        );

        let person_weights = manager
            .weights_per_subpopulation
            .get(&Id::create("person"))
            .unwrap();
        let freight_weights = manager
            .weights_per_subpopulation
            .get(&Id::create("freight"))
            .unwrap();

        assert_eq!(1, person_weights.entries.len());
        assert_eq!(0.3, person_weights.entries[0].weight);
        assert_eq!(1, freight_weights.entries.len());
        assert_eq!(0.7, freight_weights.entries[0].weight);
    }

    #[deterministic_id_test]
    fn innovation_filter_keeps_selectors_and_excludes_default_strategies() {
        let replanning = Replanning {
            strategy_settings: vec![
                StrategySetting {
                    name: "ReRoute".to_string(),
                    weight: 1.0,
                    subpopulation: "person".to_string(),
                },
                StrategySetting {
                    name: DefaultSelector::BestScore.as_str().to_string(),
                    weight: 1.0,
                    subpopulation: "person".to_string(),
                },
            ],
            ..Replanning::default()
        };
        let manager = StrategyManager::from_replanning_config(
            &replanning,
            TripRouter::default(),
            &ScenarioCore::default(),
        );
        let person = person_with_scores([Some(1.0), Some(2.0)]);
        let context = ReplanningContext {
            innovation_disabled: true,
            ..context()
        };

        let strategy = manager.choose_strategy(&context, &person).unwrap();

        assert_eq!(
            &Id::create(DefaultSelector::BestScore.as_str()),
            strategy.name()
        );
    }

    #[deterministic_id_test]
    fn default_strategy_is_named_generic_keep_last_selected() {
        let manager = StrategyManager::default();
        let strategy_name = Id::create(DefaultSelector::KeepLastSelected.as_str());
        let strategy = manager.strategies.get(&strategy_name).unwrap();
        let default_weight = &manager
            .weights_per_subpopulation
            .get(&Id::create("person"))
            .unwrap()
            .entries[0];

        assert_eq!(&strategy_name, strategy.name());
        assert_eq!(strategy.name(), &default_weight.strategy_name);
    }

    #[deterministic_id_test]
    fn generic_strategy_without_modules_does_not_copy_plan() {
        let strategy = GenericPlanStrategy {
            name: Id::create("KeepLastSelected"),
            selector: Box::new(KeepLastSelector),
            modules: Vec::new(),
        };
        let mut person = person_with_scores([Some(1.0)]);

        strategy.handle(&mut person, &context());

        assert_eq!(1, person.plans().len());
        assert!(person.plans()[0].selected);
        assert_eq!(Some(1.0), person.plans()[0].score);
    }

    #[deterministic_id_test]
    fn generic_strategy_with_modules_copies_plan_and_runs_modules_on_copy() {
        let strategy = GenericPlanStrategy {
            name: Id::create("ReRoute"),
            selector: Box::new(KeepLastSelector),
            modules: vec![Box::new(MarkCopiedPlanModule)],
        };
        let mut person = person_with_scores([Some(1.0)]);

        strategy.handle(&mut person, &context());

        assert_eq!(2, person.plans().len());
        assert!(!person.plans()[0].selected);
        assert_eq!(Some(1.0), person.plans()[0].score);
        assert!(person.plans()[1].selected);
        assert_eq!(Some(99.0), person.plans()[1].score);
    }

    #[deterministic_id_test]
    fn reroute_copies_and_replaces_single_teleported_trip() {
        let mut person = InternalPerson::new(
            Id::create("person-1"),
            routed_plan("walk", &["home", "work"], &["a", "b"]),
        );
        let original = person.plans()[0].clone();
        let mut modules: IntMap<_, Arc<dyn RoutingModule>> = IntMap::default();
        let mode = Id::create("walk");
        modules.insert(
            mode.clone(),
            Arc::new(TeleportationRoutingModule::new(mode, 1.0, 1.0)),
        );
        let strategy = reroute_strategy(&ScenarioCore::default(), TripRouter::new(modules));

        assert_eq!(1, person.plans().len());

        strategy.handle(&mut person, &context());

        assert_eq!(2, person.plans().len());
        assert_eq!(&original.elements, &person.plans()[0].elements);
        assert!(!person.plans()[0].selected);
        assert_eq!(Some(9.0), person.plans()[0].score);

        let replanned = &person.plans()[1];
        assert!(replanned.selected);
        assert_eq!(None, replanned.score);
        assert_eq!(1, get_trip_spans_default(&replanned.elements).len());
        let leg = replanned.legs()[0];
        assert_eq!(Some(Id::create("walk")), leg.routing_mode);
        assert_eq!(Some(SimTime::from_secs(10)), leg.dep_time);
        assert_ne!(original.legs()[0].route, leg.route);
    }

    #[deterministic_id_test]
    fn reroute_replaces_multistage_trips_and_uses_updated_departure_time_and_vehicle() {
        let mut core = ScenarioCore::default();
        let mut config = Config::default();
        config.qsim_mut().main_modes = vec!["car".to_string()];
        core.config = Arc::new(config);

        let vehicle_id = Id::create("person-1_car");
        let mut garage = Garage::default();
        garage.add_veh(InternalVehicle {
            id: vehicle_id.clone(),
            max_v: 10.0,
            pce: 1.0,
            vehicle_type: Id::<InternalVehicleType>::create("car-type"),
            attributes: Default::default(),
        });
        core.garage = Arc::new(garage);

        let departures = Arc::new(Mutex::new(Vec::new()));
        let mut modules: IntMap<_, Arc<dyn RoutingModule>> = IntMap::default();
        modules.insert(
            Id::create("car"),
            Arc::new(TestNetworkRoutingModule {
                mode: Id::create("car"),
                departures: departures.clone(),
            }),
        );
        let strategy = reroute_strategy(&core, TripRouter::new(modules));
        let mut person = InternalPerson::new(
            Id::create("person-1"),
            routed_plan("car", &["home", "work", "shop"], &["a", "b", "c"]),
        );
        let original = person.plans()[0].clone();

        strategy.handle(&mut person, &context());

        assert_eq!(&original.elements, &person.plans()[0].elements);
        assert!(!person.plans()[0].selected);
        let replanned = &person.plans()[1];
        assert!(replanned.selected);
        assert_eq!(None, replanned.score);
        let spans = get_trip_spans_default(&replanned.elements);
        assert_eq!(2, spans.len());
        assert_eq!(
            vec![5, 5],
            spans
                .iter()
                .map(|s| s.trip_elements(&replanned.elements).len())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            vec![SimTime::from_secs(10), SimTime::from_secs(19)],
            *departures.lock().unwrap()
        );
        for span in spans {
            let legs: Vec<_> = span.legs(&replanned.elements).collect();
            assert_eq!(
                vec!["walk", "car", "walk"],
                legs.iter()
                    .map(|leg| leg.mode.external())
                    .collect::<Vec<_>>()
            );
            assert!(
                legs.iter()
                    .all(|leg| leg.routing_mode == Some(Id::create("car")))
            );
            assert_eq!(
                Some(&vehicle_id),
                legs[1]
                    .route
                    .as_ref()
                    .unwrap()
                    .as_generic()
                    .vehicle()
                    .as_ref()
            );
            assert_eq!(
                2,
                span.trip_elements(&replanned.elements)
                    .iter()
                    .filter_map(InternalPlanElement::as_activity)
                    .count()
            );
        }
    }

    #[deterministic_id_test]
    fn reroute_reports_missing_routing_module_with_person_and_trip() {
        let strategy = reroute_strategy(&ScenarioCore::default(), TripRouter::default());
        let mut person = InternalPerson::new(
            Id::create("person-1"),
            routed_plan("walk", &["home", "work"], &["a", "b"]),
        );

        let message = panic_message(|| strategy.handle(&mut person, &context()));

        assert!(message.contains("person person-1, plan 1, trip 0"));
        assert!(message.contains("No routing module found for mode walk"));
    }

    #[deterministic_id_test]
    fn reroute_reports_missing_vehicle_before_routing() {
        let mut core = ScenarioCore::default();
        let mut config = Config::default();
        config.qsim_mut().main_modes = vec!["car".to_string()];
        core.config = Arc::new(config);
        let strategy = reroute_strategy(&core, TripRouter::default());
        let mut person = InternalPerson::new(
            Id::create("person-1"),
            routed_plan("car", &["home", "work"], &["a", "b"]),
        );

        let message = panic_message(|| strategy.handle(&mut person, &context()));

        assert!(message.contains("person person-1, plan 1, trip 0"));
        assert!(message.contains("person-1_car"));
    }

    #[deterministic_id_test]
    fn reroute_reports_missing_departure_time() {
        let strategy = reroute_strategy(&ScenarioCore::default(), TripRouter::default());
        let mut plan = routed_plan("walk", &["home", "work"], &["a", "b"]);
        plan.acts_mut()[0].end_time = None;
        let mut person = InternalPerson::new(Id::create("person-1"), plan);

        let message = panic_message(|| strategy.handle(&mut person, &context()));

        assert!(message.contains("person person-1, plan 1, trip 0"));
        assert!(message.contains("departure time"));
    }

    #[deterministic_id_test]
    fn reroute_handles_plan_without_trips() {
        let strategy = reroute_strategy(&ScenarioCore::default(), TripRouter::default());
        let mut person = person_with_scores([Some(9.0)]);

        strategy.handle(&mut person, &context());

        assert_eq!(2, person.plans().len());
        assert_eq!(Some(9.0), person.plans()[0].score);
        assert!(!person.plans()[0].selected);
        assert_eq!(None, person.plans()[1].score);
        assert!(person.plans()[1].selected);
    }

    fn reroute_strategy(core: &ScenarioCore, router: TripRouter) -> Box<dyn PlanStrategy> {
        DefaultStrategy::ReRoute.as_generic_plan_strategy(router, core.clone())
    }

    fn routed_plan(mode: &str, activities: &[&str], links: &[&str]) -> InternalPlan {
        let mut plan = InternalPlan {
            score: Some(9.0),
            ..InternalPlan::default()
        };
        for index in 0..activities.len() {
            plan.add_act(InternalActivity::new(
                Some(Coordinate::new_2d(index as f64 * 10.0, 0.0)),
                activities[index],
                Id::create(links[index]),
                None,
                (index == 0).then_some(SimTime::from_secs(10)),
                (index == 1).then_some(Duration::from_secs(5)),
            ));
            if index + 1 < activities.len() {
                let route = InternalRoute::Generic(InternalGenericRoute::new(
                    Id::create(links[index]),
                    Id::create(links[index + 1]),
                    Some(Duration::from_secs(10)),
                    Some(17.0),
                    None,
                ));
                plan.add_leg(InternalLeg::new(
                    route,
                    mode,
                    mode,
                    Duration::from_secs(10),
                    None,
                ));
            }
        }
        plan
    }

    fn panic_message(action: impl FnOnce()) -> String {
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action)).unwrap_err();
        if let Some(message) = panic.downcast_ref::<String>() {
            message.clone()
        } else {
            panic.downcast_ref::<&str>().unwrap().to_string()
        }
    }

    struct TestNetworkRoutingModule {
        mode: Id<String>,
        departures: Arc<Mutex<Vec<SimTime>>>,
    }

    impl RoutingModule for TestNetworkRoutingModule {
        fn calc_route(
            &self,
            request: RoutingRequest,
        ) -> Result<Vec<InternalPlanElement>, RoutingError> {
            self.departures
                .lock()
                .unwrap()
                .push(request.departure_time());
            let from = request.from().link().clone();
            let to = request.to().link().clone();
            let one_second = Duration::from_secs(1);
            let two_seconds = Duration::from_secs(2);
            let access = InternalPlanElement::Leg(InternalLeg::new(
                InternalRoute::Generic(InternalGenericRoute::new(
                    from.clone(),
                    from.clone(),
                    Some(one_second),
                    Some(0.0),
                    None,
                )),
                "walk",
                "walk",
                one_second,
                Some(request.departure_time()),
            ));
            let access_interaction = InternalPlanElement::Activity(InternalActivity::new(
                Some(request.from().coord().clone()),
                "car interaction",
                from.clone(),
                None,
                None,
                Some(Duration::ZERO),
            ));
            let network_leg = InternalPlanElement::Leg(InternalLeg::new(
                InternalRoute::Network(InternalNetworkRoute::new(
                    InternalGenericRoute::new(
                        from.clone(),
                        to.clone(),
                        Some(two_seconds),
                        Some(10.0),
                        request.vehicle().map(|vehicle| vehicle.id().clone()),
                    ),
                    vec![from.clone(), to.clone()],
                )),
                "car",
                "car",
                two_seconds,
                None,
            ));
            let egress_interaction = InternalPlanElement::Activity(InternalActivity::new(
                Some(request.to().coord().clone()),
                "car interaction",
                to.clone(),
                None,
                None,
                Some(Duration::ZERO),
            ));
            let egress = InternalPlanElement::Leg(InternalLeg::new(
                InternalRoute::Generic(InternalGenericRoute::new(
                    to.clone(),
                    to,
                    Some(one_second),
                    Some(0.0),
                    None,
                )),
                "walk",
                "walk",
                one_second,
                None,
            ));
            Ok(vec![
                access,
                access_interaction,
                network_leg,
                egress_interaction,
                egress,
            ])
        }

        fn mode(&self) -> &Id<String> {
            &self.mode
        }
    }

    pub fn person_with_scores<const N: usize>(scores: [Option<f64>; N]) -> InternalPerson {
        let mut person = InternalPerson::new(Id::create("person"), plan(scores[0], true));
        for score in scores.into_iter().skip(1) {
            person.plans_mut().push(plan(score, false));
        }
        person
    }

    fn plan(score: Option<f64>, selected: bool) -> InternalPlan {
        InternalPlan {
            score,
            selected,
            elements: Vec::new(),
        }
    }

    pub fn context() -> ReplanningContext {
        ReplanningContext {
            iteration: 7,
            base_seed: 42,
            innovation_disabled: false,
        }
    }

    struct MarkCopiedPlanModule;

    impl PlanStrategyModule for MarkCopiedPlanModule {
        fn handle(&self, person: &mut InternalPerson, plan_index: usize) {
            person.plans_mut()[plan_index].score = Some(99.0);
        }
    }
}
