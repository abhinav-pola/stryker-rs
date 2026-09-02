//! Turn mutants + dry-run results into test plans.

use std::time::Duration;

use crate::config::{CoverageAnalysis, StrykerConfig};
use crate::model::{Mutant, MutantCoverage, MutantStatus, MutantTestPlan, TestId};

/// Same factor as stryker-js: generous so slow-but-terminating mutants don't
/// get misreported as Timeout.
pub const HIT_LIMIT_FACTOR: u64 = 100;

pub struct DryRunTiming {
    /// Sum of individual test times.
    pub net_ms: u64,
    /// Gross dry-run time minus net (process boot, transpile, ...).
    pub overhead_ms: u64,
}

pub struct PlanInput<'a> {
    pub coverage: Option<&'a MutantCoverage>,
    pub coverage_mode: CoverageAnalysis,
    pub timing: &'a DryRunTiming,
    /// test id -> time_ms, for netTime of covered subsets.
    pub test_times: &'a [(TestId, f64)],
}

pub fn plan_mutants(
    mutants: &[Mutant],
    config: &StrykerConfig,
    input: &PlanInput<'_>,
) -> Vec<MutantTestPlan> {
    mutants.iter().map(|m| plan_one(m, config, input)).collect()
}

fn plan_one(mutant: &Mutant, config: &StrykerConfig, input: &PlanInput<'_>) -> MutantTestPlan {
    if let Some(reason) = &mutant.ignored {
        return MutantTestPlan::EarlyResult {
            mutant: mutant.id,
            status: MutantStatus::Ignored,
            reason: reason.clone(),
        };
    }

    let full_suite_timeout = timeout_for(config, input.timing.net_ms as f64, input.timing.overhead_ms);

    match (input.coverage_mode, input.coverage) {
        (CoverageAnalysis::Off, _) | (_, None) => MutantTestPlan::Run {
            mutant: mutant.id,
            test_filter: None,
            timeout: full_suite_timeout,
            hit_limit: None,
            reload_environment: true,
        },
        (_, Some(coverage)) => {
            let hit_limit = Some(coverage.total_hits(mutant.id).max(1) * HIT_LIMIT_FACTOR);
            if coverage.is_static(mutant.id) {
                // Static mutant: runs at module load; only a full fresh run
                // can exercise it.
                return MutantTestPlan::Run {
                    mutant: mutant.id,
                    test_filter: None,
                    timeout: full_suite_timeout,
                    hit_limit,
                    reload_environment: true,
                };
            }
            let covering: Vec<TestId> =
                coverage.covering_tests(mutant.id).into_iter().cloned().collect();
            if covering.is_empty() {
                return MutantTestPlan::EarlyResult {
                    mutant: mutant.id,
                    status: MutantStatus::NoCoverage,
                    reason: "No tests cover this mutant".into(),
                };
            }
            let net: f64 = input
                .test_times
                .iter()
                .filter(|(id, _)| covering.contains(id))
                .map(|(_, t)| *t)
                .sum();
            MutantTestPlan::Run {
                mutant: mutant.id,
                test_filter: Some(covering),
                timeout: timeout_for(config, net, input.timing.overhead_ms),
                hit_limit,
                reload_environment: false,
            }
        }
    }
}

fn timeout_for(config: &StrykerConfig, net_ms: f64, overhead_ms: u64) -> Duration {
    let ms = config.timeout_factor * net_ms + config.timeout_ms as f64 + overhead_ms as f64;
    Duration::from_millis((ms as u64).max(1000))
}
