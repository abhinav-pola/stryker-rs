//! Native bun:test runner.
//!
//! bun has no programmatic API, so every dry run / mutant run is a fresh
//! `bun test` process. That makes activation trivially correct (the env var
//! is read before any module loads) and `reloadEnvironment` a no-op.
//!
//! Per-test identity uses ORDINAL CORRELATION (see js/bun-preload.ts):
//! the preload counts executed tests; the junit XML lists all testcases in
//! document order with `<skipped>` marking non-executed ones. The n-th
//! executed testcase is ordinal n. Coverage buckets keyed by ordinal are
//! re-keyed to stable test ids after each run.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;
use stryker_core::{MutantCoverage, TestId};

use crate::process::run_with_timeout;
use crate::{
    Capabilities, DryRunOptions, DryRunResult, MutantRunOptions, MutantRunOutcome, TestResult,
    TestRunner,
};

const PRELOAD_SOURCE: &str = include_str!("../../../js/bun-preload.ts");
pub const ACTIVE_MUTANT_ENV: &str = "__STRYKER_ACTIVE_MUTANT__";
pub const HIT_LIMIT_ENV: &str = "__STRYKER_HIT_LIMIT__";
/// Keep `-t` regex alternations under this size; beyond it, filter by file
/// only (over-running tests is safe, under-running is not).
const MAX_NAME_PATTERN_BYTES: usize = 6000;

/// Handoff file written by the preload's afterAll.
#[derive(Debug, Deserialize)]
struct Handoff {
    #[serde(default)]
    ordinals: u32,
    #[serde(default, rename = "perTest")]
    per_test: HashMap<String, HashMap<String, u64>>,
    #[serde(default, rename = "static")]
    static_hits: HashMap<String, u64>,
    #[serde(default, rename = "hitCount")]
    hit_count: Option<u64>,
}

pub struct BunRunner {
    cwd: Utf8PathBuf,
    temp_dir: Utf8PathBuf,
    /// Extra `bun test` args from config.
    extra_args: Vec<String>,
    /// Explicit test files (empty = bun's own discovery).
    test_files: Vec<String>,
    /// Extra env for every invocation.
    extra_env: Vec<(String, String)>,
    /// Unique per instance so concurrent workers don't collide on artifacts.
    worker_id: u64,
    run_counter: u64,
    preload_path: Utf8PathBuf,
}

impl BunRunner {
    pub fn new(
        cwd: Utf8PathBuf,
        temp_dir: Utf8PathBuf,
        extra_args: Vec<String>,
        test_files: Vec<String>,
        extra_env: Vec<(String, String)>,
    ) -> Self {
        static NEXT_WORKER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let worker_id = NEXT_WORKER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let preload_path = temp_dir.join("stryker-preload.ts");
        Self {
            cwd,
            temp_dir,
            extra_args,
            test_files,
            extra_env,
            worker_id,
            run_counter: 0,
            preload_path,
        }
    }

    fn artifact(&mut self, kind: &str) -> Utf8PathBuf {
        self.run_counter += 1;
        self.temp_dir.join(format!("{kind}-w{}-{}.tmp", self.worker_id, self.run_counter))
    }

    fn base_command(
        &self,
        junit_path: &Utf8Path,
        coverage_path: &Utf8Path,
    ) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("bun");
        cmd.arg("test")
            .arg("--preload")
            .arg(self.preload_path.as_str())
            .arg("--reporter=junit")
            .arg(format!("--reporter-outfile={junit_path}"))
            .args(&self.extra_args)
            .current_dir(&self.cwd)
            .env("STRYKER_COVERAGE_FILE", coverage_path.as_str())
            // Ordinal correlation requires sequential execution.
            .env("BUN_TEST_NO_CONCURRENT", "1");
        cmd.envs(self.extra_env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        cmd
    }
}

#[async_trait]
impl TestRunner for BunRunner {
    fn capabilities(&self) -> Capabilities {
        Capabilities { per_test_coverage: true }
    }

    async fn init(&mut self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.temp_dir)?;
        // Always overwrite: a stale preload from an older binary must never win.
        std::fs::write(&self.preload_path, PRELOAD_SOURCE)?;
        Ok(())
    }

    async fn dry_run(&mut self, options: &DryRunOptions) -> anyhow::Result<DryRunResult> {
        let junit_path = self.artifact("junit");
        let coverage_path = self.artifact("coverage");
        let mut cmd = self.base_command(&junit_path, &coverage_path);
        cmd.args(&self.test_files);

        let output = run_with_timeout(cmd, options.timeout).await?;
        if output.timed_out {
            return Ok(DryRunResult::Timeout);
        }
        let junit_xml = std::fs::read_to_string(&junit_path).ok();
        let handoff = read_handoff(&coverage_path);
        cleanup(&[&junit_path, &coverage_path]);

        let Some(junit_xml) = junit_xml else {
            return Ok(DryRunResult::Error(format!(
                "bun test produced no junit output (exit {:?}):\n{}",
                output.exit_code,
                output.diagnostic_tail()
            )));
        };
        let cases = crate::junit::parse_junit(&junit_xml)?;
        let executed: Vec<&crate::junit::JunitCase> =
            cases.iter().filter(|c| !c.skipped).collect();

        let tests: Vec<TestResult> = executed
            .iter()
            .map(|c| TestResult {
                id: c.test_id(),
                name: c.test_id(),
                file: c.file.clone().map(Utf8PathBuf::from),
                time_ms: c.time_ms,
                failed: c.failed,
                failure: c.failed.then(|| output.diagnostic_tail()),
            })
            .collect();

        // Re-key ordinal coverage to test ids.
        let coverage = if options.collect_coverage {
            let handoff = handoff.ok_or_else(|| {
                anyhow::anyhow!(
                    "coverage handoff file missing — did another preload override afterAll?"
                )
            })?;
            if handoff.ordinals as usize != executed.len() {
                anyhow::bail!(
                    "ordinal mismatch: preload counted {} executed tests, junit shows {} — \
                     is something running tests concurrently?",
                    handoff.ordinals,
                    executed.len()
                );
            }
            let mut coverage = MutantCoverage::default();
            for (id, hits) in &handoff.static_hits {
                if let Ok(id) = id.parse::<u32>() {
                    coverage.static_hits.insert(id, *hits);
                }
            }
            for (ordinal, buckets) in &handoff.per_test {
                let Ok(index) = ordinal.parse::<usize>() else { continue };
                let Some(case) = executed.get(index) else { continue };
                let entry: &mut HashMap<u32, u64> =
                    coverage.per_test.entry(case.test_id()).or_default();
                for (id, hits) in buckets {
                    if let Ok(id) = id.parse::<u32>() {
                        entry.insert(id, *hits);
                    }
                }
            }
            Some(coverage)
        } else {
            None
        };

        Ok(DryRunResult::Complete {
            tests,
            coverage,
            gross_ms: output.elapsed.as_millis() as u64,
        })
    }

    async fn mutant_run(&mut self, options: &MutantRunOptions) -> anyhow::Result<MutantRunOutcome> {
        let junit_path = self.artifact("junit");
        let coverage_path = self.artifact("coverage");
        let mut cmd = self.base_command(&junit_path, &coverage_path);
        cmd.env(ACTIVE_MUTANT_ENV, options.active_mutant.to_string());
        if let Some(limit) = options.hit_limit {
            cmd.env(HIT_LIMIT_ENV, limit.to_string());
        }

        match &options.test_filter {
            Some(filter) => {
                let files = covering_files(filter);
                if files.is_empty() {
                    // Test ids without a file part; fall back to full run.
                    cmd.args(&self.test_files);
                } else {
                    cmd.args(&files);
                }
                if let Some(pattern) = name_pattern(filter) {
                    cmd.arg("-t").arg(pattern);
                }
            }
            None => {
                cmd.args(&self.test_files);
            }
        }

        let output = run_with_timeout(cmd, options.timeout).await?;
        let junit_xml = std::fs::read_to_string(&junit_path).ok();
        let handoff = read_handoff(&coverage_path);
        cleanup(&[&junit_path, &coverage_path]);

        if output.timed_out {
            return Ok(MutantRunOutcome::Timeout { reason: Some("wall-clock timeout".into()) });
        }

        // Hit limit: preferred signal is the handoff hitCount; fall back to
        // the error text if the process died before afterAll.
        if let (Some(limit), Some(handoff)) = (options.hit_limit, handoff.as_ref()) {
            if handoff.hit_count.is_some_and(|hits| hits > limit) {
                return Ok(MutantRunOutcome::Timeout {
                    reason: Some(format!(
                        "Hit limit reached ({}/{})",
                        handoff.hit_count.unwrap_or(0),
                        limit
                    )),
                });
            }
        }
        if output.stderr.contains("Stryker: Hit limit reached") {
            return Ok(MutantRunOutcome::Timeout { reason: Some("Hit limit reached".into()) });
        }

        let Some(junit_xml) = junit_xml else {
            return Ok(if output.success() {
                MutantRunOutcome::Survived { tests_ran: 0 }
            } else {
                MutantRunOutcome::Error(format!(
                    "bun test crashed without junit output (exit {:?}):\n{}",
                    output.exit_code,
                    output.diagnostic_tail()
                ))
            });
        };
        let cases = crate::junit::parse_junit(&junit_xml)?;
        let executed: Vec<&crate::junit::JunitCase> =
            cases.iter().filter(|c| !c.skipped).collect();
        let killed_by: Vec<TestId> =
            executed.iter().filter(|c| c.failed).map(|c| c.test_id()).collect();

        if killed_by.is_empty() {
            if !output.success() {
                // Process failed without a failing test: module-load crash
                // (typical for static mutants breaking imports) — the tests
                // did detect the mutant in the broadest sense.
                return Ok(MutantRunOutcome::Killed {
                    killed_by: vec![],
                    tests_ran: executed.len() as u32,
                    failure: Some(output.diagnostic_tail()),
                });
            }
            Ok(MutantRunOutcome::Survived { tests_ran: executed.len() as u32 })
        } else {
            Ok(MutantRunOutcome::Killed {
                killed_by,
                tests_ran: executed.len() as u32,
                failure: Some(output.diagnostic_tail()),
            })
        }
    }
}

fn read_handoff(path: &Utf8Path) -> Option<Handoff> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn cleanup(paths: &[&Utf8Path]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

/// Distinct file parts of `<file> > ... > <name>` test ids, order-preserving.
fn covering_files(filter: &[TestId]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut files = Vec::new();
    for id in filter {
        let Some((file, _)) = id.split_once(" > ") else { continue };
        if seen.insert(file.to_string()) {
            files.push(file.to_string());
        }
    }
    files
}

/// `-t` regex matching any of the covering tests' LEAF names. Leaf names can
/// over-match same-named tests in other describes — safe (extra tests run),
/// never under-matches. None = filter by file only.
fn name_pattern(filter: &[TestId]) -> Option<String> {
    let mut names: Vec<String> = filter
        .iter()
        .map(|id| id.rsplit(" > ").next().unwrap_or(id))
        .map(escape_regex)
        .collect();
    names.sort();
    names.dedup();
    let pattern = names.join("|");
    if pattern.is_empty() || pattern.len() > MAX_NAME_PATTERN_BYTES {
        None
    } else {
        Some(pattern)
    }
}

fn escape_regex(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 8);
    for c in name.chars() {
        if c.is_alphanumeric() || c == ' ' || c == '_' || c == '-' {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covering_files_dedupes() {
        let filter = vec![
            "src/a.test.ts > x".to_string(),
            "src/a.test.ts > suite > y".to_string(),
            "src/b.test.ts > z".to_string(),
        ];
        assert_eq!(covering_files(&filter), vec!["src/a.test.ts", "src/b.test.ts"]);
    }

    #[test]
    fn name_pattern_escapes() {
        let filter = vec!["a.test.ts > handles (edge) case".to_string()];
        assert_eq!(name_pattern(&filter).unwrap(), "handles \\(edge\\) case");
    }
}
