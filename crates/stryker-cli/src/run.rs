//! The `stryker run` pipeline: read → instrument → sandbox → dry run → plan
//! → execute → report → restore.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};
use stryker_core::config::{CleanTempDir, StrykerConfig, TestRunnerKind};
use stryker_core::planner::{self, DryRunTiming, PlanInput};
use stryker_core::sandbox::InPlaceSandbox;
use stryker_core::{Mutant, MutantResult, MutantStatus, MutantTestPlan};
use stryker_instrumenter::{InstrumentOptions, instrument_file};
use stryker_reporters::report::{Metrics, ReportInput, build_report};
use stryker_runners::command::CommandRunner;
use stryker_runners::{
    DryRunOptions, DryRunResult, MutantRunOptions, MutantRunOutcome, RunnerFactory,
};

pub struct RunFlags {
    pub config: Option<Utf8PathBuf>,
    pub force_dirty: bool,
    pub dry_run_only: bool,
}

pub async fn run(flags: RunFlags) -> anyhow::Result<i32> {
    let started = Instant::now();
    let cwd = Utf8PathBuf::from_path_buf(std::env::current_dir()?)
        .map_err(|p| anyhow::anyhow!("non-UTF8 cwd: {}", p.display()))?;
    let config = match &flags.config {
        Some(path) => stryker_core::config::load_config(path)?,
        None => match stryker_core::config::discover_config(&cwd) {
            Some(path) => {
                tracing::info!("using config {path}");
                stryker_core::config::load_config(&path)?
            }
            None => anyhow::bail!(
                "no stryker config found in {cwd}; create stryker.config.json or pass --config"
            ),
        },
    };

    let project = stryker_core::project::read_project(&cwd, &config)?;
    tracing::info!(
        "{} files in project, {} mutate targets",
        project.files.len(),
        project.targets.len()
    );
    if project.targets.is_empty() && !config.allow_empty {
        anyhow::bail!("no files matched the `mutate` patterns");
    }

    // ---- instrument ----
    let mut file_sources: BTreeMap<Utf8PathBuf, String> = BTreeMap::new();
    let mut mutants: Vec<Mutant> = Vec::new();
    let mut instrumented_files: Vec<(Utf8PathBuf, String)> = Vec::new();
    let mut next_id = 0u32;
    for target in &project.targets {
        let abs = project.root.join(&target.path);
        let source = match std::fs::read_to_string(&abs) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("cannot read {}: {e}", target.path);
                continue;
            }
        };
        let options = InstrumentOptions {
            excluded_mutators: config.mutator.excluded_mutations.clone(),
            ranges: target.ranges.clone(),
            disable_type_checks: config.disable_type_checks,
            namespace: None,
        };
        match instrument_file(&target.path, &source, next_id, &options) {
            Ok(result) => {
                next_id = mutants.iter().chain(result.mutants.iter()).map(|m| m.id.0 + 1).max().unwrap_or(next_id);
                mutants.extend(result.mutants);
                if let Some(text) = result.instrumented {
                    instrumented_files.push((target.path.clone(), text));
                }
            }
            Err(e) => {
                tracing::warn!("skipping {}: {e}", target.path);
            }
        }
        file_sources.insert(target.path.clone(), source);
    }
    tracing::info!("{} mutants in {} files", mutants.len(), instrumented_files.len());

    let temp_dir = project.root.join(&config.temp_dir_name);
    std::fs::create_dir_all(&temp_dir)?;

    // ---- dirty-file safety check ----
    let touched: Vec<Utf8PathBuf> = instrumented_files.iter().map(|(p, _)| p.clone()).collect();
    if !flags.force_dirty {
        let dirty = stryker_core::sandbox::dirty_files(&project.root, &touched);
        if !dirty.is_empty() {
            anyhow::bail!(
                "refusing to mutate files with uncommitted changes (a failed restore would lose \
                 your work): {}\nCommit/stash them or pass --force-dirty.",
                dirty.iter().take(5).map(|p| p.as_str()).collect::<Vec<_>>().join(", ")
            );
        }
    }

    if !config.in_place {
        tracing::warn!(
            "copy sandbox is not implemented yet; running in-place (originals are backed up to \
             {temp_dir} and restored afterwards)"
        );
    }

    // ---- runner factory ----
    let factory = runner_factory(&config, &project.root, &temp_dir)?;

    // ---- sandbox + dry run + mutation testing (restore on every path) ----
    let mut sandbox = InPlaceSandbox::activate(&project.root, &temp_dir, &instrumented_files)?;
    let setup_ms = started.elapsed().as_millis() as u64;

    // Incremental: load the previous report (it IS a full report).
    let incremental_path = config.effective_incremental_file(&project.root);
    let old_report: Option<stryker_reporters::schema::MutationTestResult> =
        if config.incremental && !config.force && incremental_path.exists() {
            match std::fs::read_to_string(&incremental_path)
                .map_err(anyhow::Error::from)
                .and_then(|t| Ok(serde_json::from_str(&t)?))
            {
                Ok(report) => Some(report),
                Err(e) => {
                    tracing::warn!("ignoring unreadable incremental file {incremental_path}: {e}");
                    None
                }
            }
        } else {
            None
        };
    let command_fingerprint = match config.test_runner {
        TestRunnerKind::Command => config
            .command_runner
            .command
            .as_deref()
            .map(stryker_incremental::hash_content),
        _ => None,
    };

    let execution = tokio::select! {
        result = execute(ExecuteInput {
            config: &config,
            factory: &factory,
            mutants: &mutants,
            dry_run_only: flags.dry_run_only,
            project_root: &project.root,
            file_sources: &file_sources,
            old_report: old_report.as_ref(),
            command_fingerprint: command_fingerprint.as_deref(),
        }) => result,
        _ = tokio::signal::ctrl_c() => {
            tracing::warn!("interrupted; restoring files");
            Err(anyhow::anyhow!("interrupted"))
        }
    };
    let restore_result = sandbox.restore();
    drop(sandbox);
    let execution = execution?;
    restore_result?;

    let Execution { results, initial_run_ms, mutation_ms, test_files, test_file_hashes } =
        execution;
    if flags.dry_run_only {
        println!("Dry run completed successfully.");
        return Ok(0);
    }

    // ---- report ----
    let stryker_rs_config = serde_json::json!({
        "strykerRs": {
            "testCommandFingerprint": command_fingerprint,
            "testFileHashes": test_file_hashes,
        }
    });
    let report = build_report(&ReportInput {
        file_sources: &file_sources,
        mutants: &mutants,
        results: &results,
        thresholds_high: config.thresholds.high,
        thresholds_low: config.thresholds.low,
        project_root: Some(project.root.to_string()),
        config: Some(stryker_rs_config),
        test_files,
        performance: Some(stryker_reporters::schema::Performance {
            setup: setup_ms,
            initial_run: initial_run_ms,
            mutation: mutation_ms,
        }),
    });

    if config.incremental {
        if let Some(parent) = incremental_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&incremental_path, serde_json::to_string(&report)?)?;
        tracing::info!("incremental report written to {incremental_path}");
    }

    for reporter in &config.reporters {
        match reporter.as_str() {
            "clear-text" => print!("{}", stryker_reporters::clear_text::render(&report)),
            "json" => {
                let path = config
                    .json_reporter
                    .file_name
                    .clone()
                    .unwrap_or_else(|| Utf8PathBuf::from("reports/mutation/mutation.json"));
                let path =
                    if path.is_absolute() { path } else { project.root.join(path) };
                stryker_reporters::write_json_report(&report, &path)?;
                tracing::info!("JSON report written to {path}");
            }
            "html" => {
                let path = config
                    .html_reporter
                    .file_name
                    .clone()
                    .unwrap_or_else(|| Utf8PathBuf::from("reports/mutation/mutation.html"));
                let path =
                    if path.is_absolute() { path } else { project.root.join(path) };
                stryker_reporters::html::write(&report, &path)?;
                tracing::info!("HTML report written to {path}");
            }
            "progress" | "dots" => {} // progress bar is always on
            other => tracing::warn!("unknown reporter {other:?}"),
        }
    }

    // ---- clean temp dir ----
    match config.clean_temp_dir {
        CleanTempDir::OnSuccess | CleanTempDir::Always => {
            let _ = std::fs::remove_dir_all(&temp_dir);
        }
        CleanTempDir::Never => {}
    }

    // ---- exit code via thresholds.break ----
    let metrics = Metrics::count(
        mutants.iter().filter_map(|m| results.get(&m.id.0)).map(|r| r.status),
    );
    if let (Some(brk), Some(score)) = (config.thresholds.r#break, metrics.mutation_score()) {
        if score < brk {
            eprintln!("Mutation score {score:.2}% is below the break threshold {brk}%");
            return Ok(1);
        }
    }
    Ok(0)
}

struct Execution {
    results: BTreeMap<u32, MutantResult>,
    initial_run_ms: u64,
    mutation_ms: u64,
    test_files: Option<BTreeMap<String, stryker_reporters::schema::TestFile>>,
    test_file_hashes: BTreeMap<String, String>,
}

struct ExecuteInput<'a> {
    config: &'a StrykerConfig,
    factory: &'a RunnerFactory,
    mutants: &'a [Mutant],
    dry_run_only: bool,
    project_root: &'a Utf8Path,
    file_sources: &'a BTreeMap<Utf8PathBuf, String>,
    old_report: Option<&'a stryker_reporters::schema::MutationTestResult>,
    command_fingerprint: Option<&'a str>,
}

fn runner_factory(
    config: &StrykerConfig,
    root: &Utf8Path,
    temp_dir: &Utf8Path,
) -> anyhow::Result<RunnerFactory> {
    let root = root.to_owned();
    let temp_dir = temp_dir.to_owned();
    let bun_factory = {
        let root = root.clone();
        let temp_dir = temp_dir.clone();
        let args = config.bun_runner.args.clone();
        let test_files = config.bun_runner.test_files.clone();
        let env: Vec<(String, String)> =
            config.bun_runner.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        move || {
            stryker_runners::bun::BunRunner::new(
                root.clone(),
                temp_dir.clone(),
                args.clone(),
                test_files.clone(),
                env.clone(),
            )
        }
    };
    let vitest_factory = {
        let root = root.clone();
        let temp_dir = temp_dir.clone();
        let config_file = config.vitest_runner.config_file.clone();
        move || {
            stryker_runners::vitest::VitestRunner::new(
                root.clone(),
                temp_dir.clone(),
                config_file.clone(),
            )
        }
    };
    match config.test_runner {
        TestRunnerKind::Command => {
            let command = config
                .command_runner
                .command
                .clone()
                .ok_or_else(|| anyhow::anyhow!("commandRunner.command is required for testRunner: command"))?;
            let shell = config.command_runner.shell;
            // Validate eagerly so config errors surface before the sandbox.
            CommandRunner::with_shell(&command, root.clone(), shell)?;
            Ok(Box::new(move || {
                Ok(Box::new(CommandRunner::with_shell(&command, root.clone(), shell)?)
                    as Box<dyn stryker_runners::TestRunner>)
            }))
        }
        TestRunnerKind::Bun => Ok(Box::new(move || {
            Ok(Box::new(bun_factory()) as Box<dyn stryker_runners::TestRunner>)
        })),
        TestRunnerKind::Vitest => Ok(Box::new(move || {
            Ok(Box::new(vitest_factory()) as Box<dyn stryker_runners::TestRunner>)
        })),
        TestRunnerKind::Composite => {
            let ownership = stryker_runners::composite::shared_ownership();
            Ok(Box::new(move || {
                Ok(Box::new(stryker_runners::composite::CompositeRunner::new(
                    vec![Box::new(bun_factory()), Box::new(vitest_factory())],
                    ownership.clone(),
                )) as Box<dyn stryker_runners::TestRunner>)
            }))
        }
    }
}

async fn execute(input: ExecuteInput<'_>) -> anyhow::Result<Execution> {
    let ExecuteInput {
        config,
        factory,
        mutants,
        dry_run_only,
        project_root,
        file_sources,
        old_report,
        command_fingerprint,
    } = input;
    // ---- dry run ----
    let dry_start = Instant::now();
    let mut dry_runner = factory()?;
    dry_runner.init().await?;
    let dry = dry_runner
        .dry_run(&DryRunOptions {
            timeout: Duration::from_secs(config.dry_run_timeout_minutes * 60),
            collect_coverage: config.effective_coverage()
                != stryker_core::config::CoverageAnalysis::Off,
        })
        .await?;
    let initial_run_ms = dry_start.elapsed().as_millis() as u64;
    let (tests, coverage, gross_ms) = match dry {
        DryRunResult::Complete { tests, coverage, gross_ms } => {
            if let Some(failed) = tests.iter().find(|t| t.failed) {
                anyhow::bail!(
                    "there were failed tests in the initial test run: {} {}",
                    failed.name,
                    failed.failure.as_deref().unwrap_or_default()
                );
            }
            (tests, coverage, gross_ms)
        }
        DryRunResult::Error(message) => anyhow::bail!("dry run failed: {message}"),
        DryRunResult::Timeout => anyhow::bail!("dry run timed out"),
    };
    dry_runner.dispose().await?;
    tracing::info!("dry run complete: {} tests in {gross_ms}ms", tests.len());

    // Test inventory: hashes for incremental, testFiles for the report.
    let mut test_file_hashes: BTreeMap<String, String> = BTreeMap::new();
    let mut test_files_section: BTreeMap<String, stryker_reporters::schema::TestFile> =
        BTreeMap::new();
    for test in &tests {
        let Some(file) = &test.file else { continue };
        if !test_file_hashes.contains_key(file.as_str()) {
            if let Ok(content) = std::fs::read_to_string(project_root.join(file)) {
                test_file_hashes
                    .insert(file.to_string(), stryker_incremental::hash_content(&content));
            }
        }
        test_files_section
            .entry(file.to_string())
            .or_insert_with(|| stryker_reporters::schema::TestFile { source: None, tests: vec![] })
            .tests
            .push(stryker_reporters::schema::TestDefinition {
                id: test.id.clone(),
                name: test.name.clone(),
                location: None,
            });
    }
    let test_files =
        if test_files_section.is_empty() { None } else { Some(test_files_section) };

    if dry_run_only {
        return Ok(Execution {
            results: BTreeMap::new(),
            initial_run_ms,
            mutation_ms: 0,
            test_files,
            test_file_hashes,
        });
    }

    // ---- plan ----
    let net_ms = tests.iter().map(|t| t.time_ms).sum::<f64>() as u64;
    let timing = DryRunTiming { net_ms, overhead_ms: gross_ms.saturating_sub(net_ms) };
    let test_times: Vec<(String, f64)> =
        tests.iter().map(|t| (t.id.clone(), t.time_ms)).collect();
    let plans = planner::plan_mutants(
        mutants,
        config,
        &PlanInput {
            coverage: coverage.as_ref(),
            coverage_mode: config.effective_coverage(),
            timing: &timing,
            test_times: &test_times,
        },
    );

    // ---- incremental reuse ----
    let mut results: BTreeMap<u32, MutantResult> = BTreeMap::new();
    if let Some(old) = old_report {
        let old_test_hashes: BTreeMap<String, String> = old
            .config
            .as_ref()
            .and_then(|c| c.pointer("/strykerRs/testFileHashes"))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let fingerprint_matches = command_fingerprint.map(|new_fp| {
            old.config
                .as_ref()
                .and_then(|c| c.pointer("/strykerRs/testCommandFingerprint"))
                .and_then(|v| v.as_str())
                == Some(new_fp)
        });
        let new_test_ids: std::collections::HashSet<String> =
            tests.iter().map(|t| t.id.clone()).collect();
        let reused = stryker_incremental::reusable_results(&stryker_incremental::IncrementalInput {
            old_report: old,
            mutants,
            sources: file_sources,
            new_test_ids: &new_test_ids,
            old_test_hashes: &old_test_hashes,
            new_test_hashes: &test_file_hashes,
            command_fingerprint_matches: fingerprint_matches,
        });
        tracing::info!("incremental: reusing {} of {} mutant results", reused.len(), mutants.len());
        results.extend(reused);
    }

    let mut run_plans: Vec<MutantTestPlan> = Vec::new();
    for plan in plans {
        if results.contains_key(&plan.mutant_id().0) {
            continue; // reused from the incremental report
        }
        match plan {
            MutantTestPlan::EarlyResult { mutant, status, reason } => {
                results.insert(
                    mutant.0,
                    MutantResult {
                        status,
                        killed_by: vec![],
                        covered_by: vec![],
                        tests_ran: 0,
                        status_reason: Some(reason),
                        duration: None,
                        is_static: None,
                    },
                );
            }
            run @ MutantTestPlan::Run { .. } => run_plans.push(run),
        }
    }
    // Coverage-filtered (cheap) mutants first, full-suite/static last.
    run_plans.sort_by_key(|p| match p {
        MutantTestPlan::Run { test_filter: Some(filter), .. } => (0usize, filter.len()),
        MutantTestPlan::Run { test_filter: None, .. } => (1, usize::MAX),
        MutantTestPlan::EarlyResult { .. } => unreachable!(),
    });

    // ---- worker pool ----
    let mutation_start = Instant::now();
    let total = run_plans.len();
    let concurrency = config.effective_concurrency().min(total.max(1));
    let plan_rx = SharedQueue::new(run_plans);
    let (result_tx, mut result_rx) =
        tokio::sync::mpsc::unbounded_channel::<(u32, MutantResult)>();

    let mut workers = Vec::new();
    for _ in 0..concurrency {
        let rx = plan_rx.clone();
        let tx = result_tx.clone();
        let mut runner = factory()?;
        workers.push(tokio::spawn(async move {
            runner.init().await?;
            while let Ok(plan) = rx.recv().await {
                let MutantTestPlan::Run { mutant, test_filter, timeout, hit_limit, .. } = plan
                else {
                    continue;
                };
                let covered_by = test_filter.clone().unwrap_or_default();
                let run_started = Instant::now();
                let outcome = runner
                    .mutant_run(&MutantRunOptions {
                        active_mutant: mutant,
                        test_filter,
                        timeout,
                        hit_limit,
                    })
                    .await;
                let duration = run_started.elapsed();
                let result = match outcome {
                    Ok(MutantRunOutcome::Killed { killed_by, tests_ran, failure }) => MutantResult {
                        status: MutantStatus::Killed,
                        killed_by,
                        covered_by,
                        tests_ran,
                        status_reason: failure,
                        duration: Some(duration),
                        is_static: None,
                    },
                    Ok(MutantRunOutcome::Survived { tests_ran }) => MutantResult {
                        status: MutantStatus::Survived,
                        killed_by: vec![],
                        covered_by,
                        tests_ran,
                        status_reason: None,
                        duration: Some(duration),
                        is_static: None,
                    },
                    Ok(MutantRunOutcome::Timeout { reason }) => MutantResult {
                        status: MutantStatus::Timeout,
                        killed_by: vec![],
                        covered_by,
                        tests_ran: 0,
                        status_reason: reason,
                        duration: Some(duration),
                        is_static: None,
                    },
                    Ok(MutantRunOutcome::Error(message)) => MutantResult {
                        status: MutantStatus::RuntimeError,
                        killed_by: vec![],
                        covered_by,
                        tests_ran: 0,
                        status_reason: Some(message),
                        duration: Some(duration),
                        is_static: None,
                    },
                    Err(e) => MutantResult {
                        status: MutantStatus::RuntimeError,
                        killed_by: vec![],
                        covered_by,
                        tests_ran: 0,
                        status_reason: Some(e.to_string()),
                        duration: Some(duration),
                        is_static: None,
                    },
                };
                let _ = tx.send((mutant.0, result));
            }
            runner.dispose().await?;
            Ok::<(), anyhow::Error>(())
        }));
    }
    drop(result_tx);

    let progress = if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        let bar = indicatif::ProgressBar::new(total as u64);
        bar.set_style(
            indicatif::ProgressStyle::with_template(
                "{bar:32} {pos}/{len} mutants | {msg} | ETA {eta}",
            )
            .expect("valid template"),
        );
        Some(bar)
    } else {
        None
    };
    let mut done = 0usize;
    let (mut killed, mut survived, mut timed_out) = (0u32, 0u32, 0u32);
    while let Some((id, result)) = result_rx.recv().await {
        done += 1;
        match result.status {
            MutantStatus::Killed => killed += 1,
            MutantStatus::Survived => survived += 1,
            MutantStatus::Timeout => timed_out += 1,
            _ => {}
        }
        match &progress {
            Some(bar) => {
                bar.set_position(done as u64);
                bar.set_message(format!("{killed} killed, {survived} survived, {timed_out} timeout"));
            }
            None if done % 25 == 0 || done == total => {
                eprintln!("  tested {done}/{total} mutants ({killed} killed, {survived} survived)");
            }
            None => {}
        }
        results.insert(id, result);
    }
    if let Some(bar) = progress {
        bar.finish_and_clear();
    }
    for worker in workers {
        worker.await??;
    }

    // Annotate staticness from coverage (schema `static` field).
    if let Some(coverage) = &coverage {
        for mutant in mutants {
            if let Some(result) = results.get_mut(&mutant.id.0) {
                result.is_static = Some(coverage.is_static(mutant.id));
            }
        }
    }

    Ok(Execution {
        results,
        initial_run_ms,
        mutation_ms: mutation_start.elapsed().as_millis() as u64,
        test_files,
        test_file_hashes,
    })
}

/// Minimal MPMC work queue (avoids an extra dependency).
#[derive(Clone)]
struct SharedQueue {
    inner: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<MutantTestPlan>>>,
}

impl SharedQueue {
    fn new(plans: Vec<MutantTestPlan>) -> Self {
        Self { inner: std::sync::Arc::new(std::sync::Mutex::new(plans.into())) }
    }

    async fn recv(&self) -> Result<MutantTestPlan, ()> {
        let plan = self.inner.lock().expect("queue poisoned").pop_front();
        plan.ok_or(())
    }
}
