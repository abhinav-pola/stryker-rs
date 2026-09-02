//! Composite runner: one mutant, several test runtimes.
//!
//! Some repos have source files covered by both bun:test suites and
//! `.cfw.test.ts` vitest (workers) suites. The composite runner performs a
//! dry run on every sub-runner, remembers which runner owns which test id,
//! and routes each mutant's covering tests to their owners. A mutant
//! survives only if it survives EVERY runtime.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use stryker_core::{MutantCoverage, TestId};

use crate::{
    Capabilities, DryRunOptions, DryRunResult, MutantRunOptions, MutantRunOutcome, TestResult,
    TestRunner,
};

pub struct CompositeRunner {
    runners: Vec<Box<dyn TestRunner>>,
    /// test id -> index into `runners`, learned during the dry run. SHARED
    /// across instances: the dry run happens on one runner instance while
    /// mutants execute on fresh per-worker instances.
    owner: Arc<RwLock<HashMap<TestId, usize>>>,
}

impl CompositeRunner {
    pub fn new(runners: Vec<Box<dyn TestRunner>>, owner: SharedOwnership) -> Self {
        Self { runners, owner }
    }
}

/// Create one per run and pass a clone to every CompositeRunner instance.
pub type SharedOwnership = Arc<RwLock<HashMap<TestId, usize>>>;

pub fn shared_ownership() -> SharedOwnership {
    Arc::new(RwLock::new(HashMap::new()))
}

fn merge_coverage(into: &mut MutantCoverage, from: MutantCoverage) {
    for (id, hits) in from.static_hits {
        *into.static_hits.entry(id).or_insert(0) += hits;
    }
    for (test, buckets) in from.per_test {
        let entry = into.per_test.entry(test).or_default();
        for (id, hits) in buckets {
            *entry.entry(id).or_insert(0) += hits;
        }
    }
}

#[async_trait]
impl TestRunner for CompositeRunner {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            per_test_coverage: self.runners.iter().all(|r| r.capabilities().per_test_coverage),
        }
    }

    async fn init(&mut self) -> anyhow::Result<()> {
        for runner in &mut self.runners {
            runner.init().await?;
        }
        Ok(())
    }

    async fn dry_run(&mut self, options: &DryRunOptions) -> anyhow::Result<DryRunResult> {
        let mut all_tests: Vec<TestResult> = Vec::new();
        let mut coverage: Option<MutantCoverage> = None;
        let mut gross_ms = 0u64;
        for (index, runner) in self.runners.iter_mut().enumerate() {
            match runner.dry_run(options).await? {
                DryRunResult::Complete { tests, coverage: c, gross_ms: g } => {
                    let mut owner = self.owner.write().expect("ownership lock poisoned");
                    for test in &tests {
                        if let Some(previous) = owner.insert(test.id.clone(), index) {
                            if previous != index {
                                anyhow::bail!(
                                    "test id {:?} produced by two runners ({previous} and {index}); \
                                     scope each runner's test files so they don't overlap",
                                    test.id
                                );
                            }
                        }
                    }
                    drop(owner);
                    all_tests.extend(tests);
                    if let Some(c) = c {
                        merge_coverage(coverage.get_or_insert_with(MutantCoverage::default), c);
                    }
                    gross_ms += g;
                }
                DryRunResult::Error(message) => {
                    return Ok(DryRunResult::Error(format!("runner {index}: {message}")));
                }
                DryRunResult::Timeout => return Ok(DryRunResult::Timeout),
            }
        }
        Ok(DryRunResult::Complete { tests: all_tests, coverage, gross_ms })
    }

    async fn mutant_run(&mut self, options: &MutantRunOptions) -> anyhow::Result<MutantRunOutcome> {
        // Split the filter by owning runner; None = every runner, full suite.
        let filters: Vec<Option<Vec<TestId>>> = match &options.test_filter {
            None => vec![None; self.runners.len()],
            Some(filter) => {
                let owner = self.owner.read().expect("ownership lock poisoned");
                let mut split: Vec<Vec<TestId>> = vec![Vec::new(); self.runners.len()];
                for test in filter {
                    match owner.get(test) {
                        Some(&index) => split[index].push(test.clone()),
                        None => tracing::warn!("no runner owns test {test:?}; skipping"),
                    }
                }
                split.into_iter().map(Some).collect()
            }
        };

        let mut killed_by: Vec<TestId> = Vec::new();
        let mut tests_ran = 0u32;
        let mut failure: Option<String> = None;
        let mut timeout: Option<Option<String>> = None;
        let mut error: Option<String> = None;

        for (runner, filter) in self.runners.iter_mut().zip(filters) {
            if let Some(f) = &filter {
                if f.is_empty() && options.test_filter.is_some() {
                    continue; // no covering tests in this runtime
                }
            }
            let sub_options = MutantRunOptions { test_filter: filter, ..options.clone() };
            match runner.mutant_run(&sub_options).await? {
                MutantRunOutcome::Killed { killed_by: k, tests_ran: t, failure: f } => {
                    killed_by.extend(k);
                    tests_ran += t;
                    failure = failure.or(f);
                    // Killed in one runtime decides the mutant; stop early.
                    return Ok(MutantRunOutcome::Killed { killed_by, tests_ran, failure });
                }
                MutantRunOutcome::Survived { tests_ran: t } => tests_ran += t,
                MutantRunOutcome::Timeout { reason } => timeout = Some(reason),
                MutantRunOutcome::Error(message) => error = Some(message),
            }
        }
        if let Some(reason) = timeout {
            return Ok(MutantRunOutcome::Timeout { reason });
        }
        if let Some(message) = error {
            return Ok(MutantRunOutcome::Error(message));
        }
        Ok(MutantRunOutcome::Survived { tests_ran })
    }

    async fn dispose(&mut self) -> anyhow::Result<()> {
        for runner in &mut self.runners {
            runner.dispose().await?;
        }
        Ok(())
    }
}
