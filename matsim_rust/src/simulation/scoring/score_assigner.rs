use crate::simulation::config::Config;
use crate::simulation::scenario::population::InternalPlan;

/// Port of MATSim's `NewScoreAssignerImpl` with an implicit learning rate of 1.
///
/// Before score MSA (method of successive averages) starts, a new score replaces the previous
/// score of the selected plan. From the MSA start iteration on, the plan's score is the running
/// mean of all scores it received since then. The number of contributions is stored on the plan
/// itself, which corresponds to MATSim's counter keyed by plan identity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScoreAssigner {
    msa_start_iteration: Option<u32>,
}

impl ScoreAssigner {
    pub(crate) fn from_config(config: &Config) -> Self {
        let controller = config.controller();
        Self {
            msa_start_iteration: Self::msa_start_iteration(
                controller.first_iteration,
                controller.last_iteration,
                config.scoring().fraction_of_iterations_to_start_score_msa,
            ),
        }
    }

    /// Returns the first iteration that averages scores, or `None` if score MSA never starts.
    fn msa_start_iteration(first: u32, last: u32, fraction: Option<f64>) -> Option<u32> {
        let fraction = fraction?;
        if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
            panic!(
                "scoring.fraction_of_iterations_to_start_score_msa must be a finite value in [0, 1], but was {fraction}."
            );
        }
        let start = last.saturating_sub(first) as f64 * fraction + first as f64;
        Some(start as u32)
    }

    /// Assigns the score of the experienced plan in `iteration` to the selected `plan`.
    pub(crate) fn assign(&self, iteration: u32, plan: &mut InternalPlan, score: f64) {
        let Some(old_score) = plan.score else {
            // As in MATSim, a plan without a score takes the new score without counting an MSA
            // contribution, so the next contribution in the MSA phase replaces it.
            plan.score = Some(score);
            return;
        };

        match self.msa_start_iteration {
            Some(start) if iteration >= start => {
                // With n earlier contributions, old_score is their mean. Weighting the new score
                // with alpha = 1 / (n + 1) yields (n * old_score + score) / (n + 1), the mean of
                // all n + 1 contributions. The first contribution has alpha = 1 and discards the
                // score from before MSA started. The expression keeps MATSim's form and operation
                // order so that both implementations produce the same bits.
                let contributions = plan.msa_contributions;
                plan.msa_contributions = contributions.saturating_add(1);
                let alpha = 1.0 / (f64::from(contributions) + 1.0);
                plan.score = Some(alpha * score + (1.0 - alpha) * old_score);
            }
            // MATSim blends with `learningRate * score + (1 - learningRate) * old_score` before
            // MSA starts. With the implicit learning rate of 1, the new score replaces the old one.
            _ => plan.score = Some(score),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ScoreAssigner;
    use crate::simulation::config::{Config, Scoring};
    use crate::simulation::scenario::population::InternalPlan;

    fn assigner(msa_start_iteration: Option<u32>) -> ScoreAssigner {
        ScoreAssigner {
            msa_start_iteration,
        }
    }

    fn plan_with(score: Option<f64>, msa_contributions: u32) -> InternalPlan {
        InternalPlan {
            score,
            msa_contributions,
            ..Default::default()
        }
    }

    // Mirrors the MSA test in MATSim's `EventsToScoreTest`: iterations 10 to 110, MSA from 90 %
    // of the iterations on, and a score of `iteration - 98` in each iteration.
    #[test]
    fn averages_scores_like_matsim_events_to_score_test() {
        let mut config = Config::default();
        config.controller_mut().first_iteration = 10;
        config.controller_mut().last_iteration = 110;
        config
            .scoring_mut()
            .fraction_of_iterations_to_start_score_msa = Some(0.9);
        let assigner = ScoreAssigner::from_config(&config);
        assert_eq!(Some(100), assigner.msa_start_iteration);

        let mut plan = InternalPlan::default();
        let mut assigned_scores = Vec::new();
        for iteration in 10..=110_u32 {
            let score = f64::from(iteration) - 98.0;
            assigner.assign(iteration, &mut plan, score);

            // From iteration 100 on, the score is the mean of 2, 3, ..., iteration - 98.
            let expected = if iteration < 100 {
                score
            } else {
                (f64::from(iteration) - 96.0) / 2.0
            };
            assert_eq!(Some(expected), plan.score, "iteration {iteration}");
            assigned_scores.push(plan.score.unwrap());
        }

        // Iterations 99 to 103, which MATSim asserts with a delta of 0.
        assert_eq!(&[1.0, 2.0, 2.5, 3.0, 3.5], &assigned_scores[89..94]);
        assert_eq!(11, plan.msa_contributions);
    }

    #[test]
    fn msa_start_iteration_is_none_without_fraction() {
        assert_eq!(None, ScoreAssigner::msa_start_iteration(10, 110, None));
        assert_eq!(
            None,
            ScoreAssigner::from_config(&Config::default()).msa_start_iteration
        );
    }

    #[test]
    fn msa_start_iteration_matches_matsim() {
        assert_eq!(
            Some(100),
            ScoreAssigner::msa_start_iteration(10, 110, Some(0.9))
        );
        // 9 * 0.5 = 4.5 is truncated like Java's `(int)` cast.
        assert_eq!(Some(4), ScoreAssigner::msa_start_iteration(0, 9, Some(0.5)));
        assert_eq!(
            Some(10),
            ScoreAssigner::msa_start_iteration(10, 110, Some(0.0))
        );
        assert_eq!(
            Some(110),
            ScoreAssigner::msa_start_iteration(10, 110, Some(1.0))
        );
        assert_eq!(Some(5), ScoreAssigner::msa_start_iteration(5, 5, Some(0.5)));
    }

    #[test]
    #[should_panic(expected = "must be a finite value in [0, 1], but was NaN")]
    fn msa_start_iteration_rejects_nan() {
        ScoreAssigner::msa_start_iteration(0, 10, Some(f64::NAN));
    }

    #[test]
    #[should_panic(expected = "must be a finite value in [0, 1], but was inf")]
    fn msa_start_iteration_rejects_infinity() {
        ScoreAssigner::msa_start_iteration(0, 10, Some(f64::INFINITY));
    }

    #[test]
    #[should_panic(expected = "must be a finite value in [0, 1], but was -0.1")]
    fn msa_start_iteration_rejects_negative_fraction() {
        ScoreAssigner::msa_start_iteration(0, 10, Some(-0.1));
    }

    #[test]
    #[should_panic(expected = "must be a finite value in [0, 1], but was 1.1")]
    fn msa_start_iteration_rejects_fraction_above_one() {
        ScoreAssigner::msa_start_iteration(0, 10, Some(1.1));
    }

    #[test]
    #[should_panic(expected = "must be a finite value in [0, 1], but was NaN")]
    fn from_config_rejects_invalid_fraction() {
        let mut config = Config::default();
        config.set_scoring(Scoring {
            fraction_of_iterations_to_start_score_msa: Some(f64::NAN),
            ..Scoring::default()
        });
        ScoreAssigner::from_config(&config);
    }

    #[test]
    fn missing_old_score_is_taken_without_msa_contribution() {
        let assigner = assigner(Some(0));
        let mut plan = InternalPlan::default();

        assigner.assign(5, &mut plan, 3.0);
        assert_eq!(Some(3.0), plan.score);
        assert_eq!(0, plan.msa_contributions);

        // The first contribution has alpha = 1 and replaces the score.
        assigner.assign(6, &mut plan, 7.0);
        assert_eq!(Some(7.0), plan.score);
        assert_eq!(1, plan.msa_contributions);

        assigner.assign(7, &mut plan, 1.0);
        assert_eq!(Some(4.0), plan.score);
        assert_eq!(2, plan.msa_contributions);
    }

    #[test]
    fn score_is_replaced_before_msa_start() {
        let assigner = assigner(Some(10));
        let mut plan = plan_with(Some(1.0), 3);

        assigner.assign(9, &mut plan, 5.0);

        assert_eq!(Some(5.0), plan.score);
        assert_eq!(3, plan.msa_contributions);
    }

    #[test]
    fn score_is_always_replaced_without_msa() {
        let assigner = assigner(None);
        let mut plan = plan_with(Some(1.0), 0);

        assigner.assign(u32::MAX, &mut plan, 5.0);

        assert_eq!(Some(5.0), plan.score);
        assert_eq!(0, plan.msa_contributions);
    }

    #[test]
    fn msa_continues_with_existing_contributions() {
        let assigner = assigner(Some(10));
        // A mean of 4.0 over three contributions plus 8.0 gives a mean of 5.0 over four.
        let mut plan = plan_with(Some(4.0), 3);

        assigner.assign(12, &mut plan, 8.0);

        assert_eq!(Some(5.0), plan.score);
        assert_eq!(4, plan.msa_contributions);
    }
}
