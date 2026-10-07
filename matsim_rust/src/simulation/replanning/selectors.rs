use crate::simulation::id::Id;
use crate::simulation::random::get_rng;
use crate::simulation::replanning::{
    BEST_SCORE_STRATEGY_NAME, GenericPlanStrategy, KEEP_LAST_SELECTED_STRATEGY_NAME, PlanSelector,
    PlanStrategy, RANDOM_SELECTOR_RNG_PURPOSE, ReplanningContext, SELECT_EXP_BETA_STRATEGY_NAME,
    SELECT_RANDOM_STRATEGY_NAME, WORST_SCORE_STRATEGY_NAME,
};
use crate::simulation::scenario::population::InternalPerson;
use rand::RngExt;
use std::fmt;
use std::str::FromStr;

const SELECT_EXP_BETA_SELECTOR_RNG_PURPOSE: &str = "replanning.selector.select_exp_beta";

pub struct KeepLastSelector;

impl PlanSelector for KeepLastSelector {
    fn select(&self, person: &InternalPerson, _context: &ReplanningContext) -> usize {
        let mut selected = person
            .plans()
            .iter()
            .enumerate()
            .filter(|(_, plan)| plan.selected);
        let (index, _) = selected
            .next()
            .expect("KeepLastSelector could not find a selected plan.");
        assert!(
            selected.next().is_none(),
            "KeepLastSelector found multiple selected plans."
        );
        index
    }
}

#[allow(dead_code)]
pub struct BestScoreSelector;

impl PlanSelector for BestScoreSelector {
    fn select(&self, person: &InternalPerson, _context: &ReplanningContext) -> usize {
        person
            .plans()
            .iter()
            .enumerate()
            .filter_map(|(index, plan)| plan.score.map(|score| (index, score)))
            .max_by(|(_, left), (_, right)| left.total_cmp(right))
            .map(|(index, _)| index)
            .expect("BestScoreSelector could not find a scored plan.")
    }
}

#[allow(dead_code)]
pub struct RandomSelector;

impl PlanSelector for RandomSelector {
    fn select(&self, person: &InternalPerson, context: &ReplanningContext) -> usize {
        let plan_count = person.plans().len();
        assert!(plan_count > 0, "RandomSelector could not find a plan.");
        let stream_id = format!("{}:{}", context.iteration, person.id().external());
        let mut rng = get_rng(context.base_seed, RANDOM_SELECTOR_RNG_PURPOSE, &stream_id);
        rng.random_range(0..plan_count)
    }
}

pub struct WorstScoreSelector;

impl PlanSelector for WorstScoreSelector {
    fn select(&self, person: &InternalPerson, _context: &ReplanningContext) -> usize {
        let plans = person.plans();
        let prefer_unselected = plans.iter().any(|plan| !plan.selected);
        let mut worst_index = None;

        for (index, plan) in plans.iter().enumerate() {
            if prefer_unselected && plan.selected {
                continue;
            }

            let Some(current_worst_index) = worst_index else {
                worst_index = Some(index);
                continue;
            };

            if plan_is_worse(plan.score, plans[current_worst_index].score) {
                worst_index = Some(index);
            }
        }

        worst_index.expect("WorstSelector could not find a removable plan.")
    }
}

fn plan_is_worse(candidate: Option<f64>, current: Option<f64>) -> bool {
    match (score_for_ordering(candidate), score_for_ordering(current)) {
        (None, Some(_)) => true,
        (Some(_), None) | (None, None) => false,
        (Some(candidate), Some(current)) => candidate < current,
    }
}

fn score_for_ordering(score: Option<f64>) -> Option<f64> {
    score.filter(|score| !score.is_nan())
}

#[allow(dead_code)]
pub struct SelectExpBetaSelector {
    beta: f64,
}

#[allow(dead_code)]
impl SelectExpBetaSelector {
    pub fn new(beta: f64) -> Self {
        assert!(
            beta.is_finite() && beta >= 0.0,
            "SelectExpBetaSelector beta must be finite and non-negative."
        );
        Self { beta }
    }
}

impl Default for SelectExpBetaSelector {
    fn default() -> Self {
        Self::new(1.0)
    }
}

impl PlanSelector for SelectExpBetaSelector {
    fn select(&self, person: &InternalPerson, context: &ReplanningContext) -> usize {
        let plans = person.plans();
        assert!(
            !plans.is_empty(),
            "SelectExpBetaSelector could not find a plan."
        );

        let stream_id = format!("{}:{}", context.iteration, person.id().external());
        let mut rng = get_rng(
            context.base_seed,
            SELECT_EXP_BETA_SELECTOR_RNG_PURPOSE,
            &stream_id,
        );

        // Handle beta zero directly so extreme finite scores cannot produce 0 * -infinity below.
        if self.beta == 0.0 {
            return rng.random_range(0..plans.len());
        }

        let mut max_score = f64::NEG_INFINITY;
        for plan in plans {
            let Some(score) = plan.score.filter(|score| score.is_finite()) else {
                return 0;
            };
            max_score = max_score.max(score);
        }

        // Subtracting the maximum leaves MNL probabilities unchanged and prevents exp overflow.
        let weights = plans
            .iter()
            .map(|plan| (self.beta * (plan.score.unwrap() - max_score)).exp())
            .collect::<Vec<_>>();

        // A maximum-score plan has weight 1, so the sum remains positive and finite.
        let total_weight = weights.iter().sum::<f64>();
        let draw = rng.random::<f64>() * total_weight;
        let mut cumulative_weight = 0.0;

        for (index, weight) in weights.into_iter().enumerate() {
            cumulative_weight += weight;
            if draw < cumulative_weight {
                return index;
            }
        }

        // Defend against a final-boundary miss caused by floating-point rounding.
        plans.len() - 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultSelector {
    KeepLastSelected,
    BestScore,
    SelectRandom,
    WorstScore,
    SelectExpBeta,
}

impl DefaultSelector {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::KeepLastSelected => KEEP_LAST_SELECTED_STRATEGY_NAME,
            Self::BestScore => BEST_SCORE_STRATEGY_NAME,
            Self::SelectRandom => SELECT_RANDOM_STRATEGY_NAME,
            Self::WorstScore => WORST_SCORE_STRATEGY_NAME,
            Self::SelectExpBeta => SELECT_EXP_BETA_STRATEGY_NAME,
        }
    }

    pub(super) fn as_plan_selector(self) -> Box<dyn PlanSelector> {
        match self {
            Self::KeepLastSelected => Box::new(KeepLastSelector),
            Self::BestScore => Box::new(BestScoreSelector),
            Self::SelectRandom => Box::new(RandomSelector),
            Self::WorstScore => Box::new(WorstScoreSelector),
            Self::SelectExpBeta => Box::new(SelectExpBetaSelector::default()),
        }
    }

    pub(super) fn as_generic_plan_strategy(self) -> Box<dyn PlanStrategy> {
        let name = Id::create(self.as_str());

        Box::new(GenericPlanStrategy {
            name,
            selector: self.as_plan_selector(),
            modules: Vec::new(),
        })
    }
}

impl fmt::Display for DefaultSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DefaultSelector {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            KEEP_LAST_SELECTED_STRATEGY_NAME => Ok(Self::KeepLastSelected),
            BEST_SCORE_STRATEGY_NAME => Ok(Self::BestScore),
            SELECT_RANDOM_STRATEGY_NAME => Ok(Self::SelectRandom),
            WORST_SCORE_STRATEGY_NAME => Ok(Self::WorstScore),
            SELECT_EXP_BETA_STRATEGY_NAME => Ok(Self::SelectExpBeta),
            _ => Err(format!("Unknown DefaultSelector: {value}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::simulation::replanning::PlanSelector;
    use crate::simulation::replanning::selectors::{
        KeepLastSelector, RandomSelector, SelectExpBetaSelector, WorstScoreSelector,
    };
    use crate::simulation::replanning::tests::{context, person_with_scores};
    use macros::deterministic_id_test;

    #[deterministic_id_test]
    fn keep_last_selector_returns_selected_plan_index() {
        let person = person_with_scores([Some(1.0), Some(2.0)]);

        assert_eq!(0, KeepLastSelector.select(&person, &context()));
    }

    #[deterministic_id_test]
    #[should_panic(expected = "KeepLastSelector could not find a selected plan.")]
    fn keep_last_selector_panics_without_selected_plan() {
        let mut person = person_with_scores([Some(1.0), Some(2.0)]);
        for plan in person.plans_mut() {
            plan.selected = false;
        }

        KeepLastSelector.select(&person, &context());
    }

    #[deterministic_id_test]
    #[should_panic(expected = "KeepLastSelector found multiple selected plans.")]
    fn keep_last_selector_panics_with_multiple_selected_plans() {
        let mut person = person_with_scores([Some(1.0), Some(2.0)]);
        person.plans_mut()[1].selected = true;

        KeepLastSelector.select(&person, &context());
    }

    #[deterministic_id_test]
    fn worst_selector_treats_missing_score_as_worst() {
        let person = person_with_scores([Some(1.0), None, Some(-5.0)]);

        assert_eq!(1, WorstScoreSelector.select(&person, &context()));
    }

    #[deterministic_id_test]
    fn worst_selector_prefers_removing_unselected_plans() {
        let mut person = person_with_scores([Some(-100.0), Some(1.0)]);
        person.plans_mut()[0].selected = true;
        person.plans_mut()[1].selected = false;

        assert_eq!(1, WorstScoreSelector.select(&person, &context()));
    }

    #[deterministic_id_test]
    fn random_selector_is_deterministic_for_same_context() {
        let person = person_with_scores([Some(1.0), Some(2.0), Some(3.0)]);
        let context = context();

        let first = RandomSelector.select(&person, &context);
        let second = RandomSelector.select(&person, &context);

        assert_eq!(first, second);
        assert!(first < person.plans().len());
    }

    #[deterministic_id_test]
    fn select_exp_beta_follows_mnl_probabilities() {
        let person = person_with_scores([Some(0.0), Some(1.0)]);
        let selector = SelectExpBetaSelector::default();
        let mut context = context();
        let sample_count = 10_000;
        let mut count_higher_score_selections = 0;

        for iteration in 0..sample_count {
            context.iteration = iteration;
            // return value is 0 for low-score plan, 1 for high-score plan
            count_higher_score_selections += usize::from(selector.select(&person, &context) == 1);
        }

        let actual_probability = count_higher_score_selections as f64 / sample_count as f64;

        // MNL probability: e^0 + e^1
        let expected_probability = std::f64::consts::E / (1.0 + std::f64::consts::E);
        assert!((actual_probability - expected_probability).abs() < 0.02);
    }

    #[deterministic_id_test]
    fn select_exp_beta_prefers_higher_score_when_all_scores_are_negative() {
        let selector = SelectExpBetaSelector::default();
        let sample_count = 10_000;
        // With beta = 1, P(score = -1) = exp(-1) / (exp(-1) + exp(-2)).
        let expected_probability = std::f64::consts::E / (1.0 + std::f64::consts::E);

        // Check both plan orders so a preference for the first plan cannot pass.
        for (scores, higher_score_index) in
            [([Some(-1.0), Some(-2.0)], 0), ([Some(-2.0), Some(-1.0)], 1)]
        {
            let person = person_with_scores(scores);
            let mut context = context();
            let mut selections = [0; 2];

            for iteration in 0..sample_count {
                context.iteration = iteration;
                selections[selector.select(&person, &context)] += 1;
            }

            assert!(selections[higher_score_index] > selections[1 - higher_score_index]);
            let actual_probability = selections[higher_score_index] as f64 / sample_count as f64;
            assert!(
                (actual_probability - expected_probability).abs() < 0.02,
                "Expected probability {expected_probability} for score -1, got {actual_probability}"
            );
        }
    }

    #[deterministic_id_test]
    fn select_exp_beta_is_stable_and_translation_invariant_for_large_scores() {
        let ordinary_scores = person_with_scores([Some(0.0), Some(1.0)]);
        let large_scores = person_with_scores([Some(1000.0), Some(1001.0)]);
        let selector = SelectExpBetaSelector::default();
        let mut context = context();

        for iteration in 0..100 {
            context.iteration = iteration;
            assert_eq!(
                selector.select(&ordinary_scores, &context),
                selector.select(&large_scores, &context)
            );
        }
    }

    #[deterministic_id_test]
    fn select_exp_beta_with_zero_beta_is_uniform() {
        let person = person_with_scores([Some(f64::MIN), Some(f64::MAX)]);
        let selector = SelectExpBetaSelector::new(0.0);
        let mut context = context();
        let sample_count = 10_000;
        let mut second_plan_selections = 0;

        for iteration in 0..sample_count {
            context.iteration = iteration;
            second_plan_selections += usize::from(selector.select(&person, &context) == 1);
        }

        let actual_probability = second_plan_selections as f64 / sample_count as f64;
        assert!((actual_probability - 0.5).abs() < 0.02);
    }

    #[deterministic_id_test]
    fn select_exp_beta_falls_back_to_first_plan_for_invalid_scores() {
        let selector = SelectExpBetaSelector::new(1.0);

        for invalid_score in [
            None,
            Some(f64::NAN),
            Some(f64::INFINITY),
            Some(f64::NEG_INFINITY),
        ] {
            let person = person_with_scores([Some(1.0), invalid_score]);
            assert_eq!(0, selector.select(&person, &context()));
        }
    }

    #[test]
    fn select_exp_beta_rejects_invalid_beta() {
        for beta in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(std::panic::catch_unwind(|| SelectExpBetaSelector::new(beta)).is_err());
        }
    }

    #[deterministic_id_test]
    #[should_panic(expected = "SelectExpBetaSelector could not find a plan.")]
    fn select_exp_beta_panics_without_plans() {
        let mut person = person_with_scores([Some(1.0)]);
        person.plans_mut().clear();

        SelectExpBetaSelector::new(1.0).select(&person, &context());
    }
}
