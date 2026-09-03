//! Vitest runner: drives js/vitest-shim.mjs (one long-lived Node process per
//! worker) over an NDJSON stdio protocol.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;
use serde_json::json;
use stryker_core::MutantCoverage;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

use crate::{
    Capabilities, DryRunOptions, DryRunResult, MutantRunOptions, MutantRunOutcome, TestResult,
    TestRunner,
};

const SHIM_SOURCE: &str = include_str!("../../../js/vitest-shim.mjs");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShimTest {
    id: String,
    file: String,
    time_ms: f64,
    state: String,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ShimCoverage {
    #[serde(default, rename = "static")]
    static_hits: HashMap<String, u64>,
    #[serde(default, rename = "perTest")]
    per_test: HashMap<String, HashMap<String, u64>>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum ShimReply {
    Ready {},
    DryRunResult {
        tests: Vec<ShimTest>,
        coverage: Option<ShimCoverage>,
    },
    MutantRunResult {
        status: String,
        #[serde(default)]
        killed_by: Vec<String>,
        #[serde(default)]
        tests_ran: u32,
        #[serde(default)]
        failure_message: Option<String>,
        #[serde(default)]
        reason: Option<String>,
    },
    Disposed {},
    Crash {
        error: String,
    },
}

pub struct VitestRunner {
    cwd: Utf8PathBuf,
    /// Project-root-relative prefix of `cwd` ("" when they coincide); test
    /// ids from the shim are cwd-relative and get prefixed on the way out,
    /// filters get de-prefixed on the way in.
    path_prefix: String,
    temp_dir: Utf8PathBuf,
    config_file: Option<String>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<tokio::io::Lines<BufReader<ChildStdout>>>,
    next_id: u64,
}

impl VitestRunner {
    pub fn new(
        cwd: Utf8PathBuf,
        path_prefix: String,
        temp_dir: Utf8PathBuf,
        config_file: Option<String>,
    ) -> Self {
        let path_prefix = if path_prefix.is_empty() || path_prefix.ends_with('/') {
            path_prefix
        } else {
            format!("{path_prefix}/")
        };
        Self {
            cwd,
            path_prefix,
            temp_dir,
            config_file,
            child: None,
            stdin: None,
            stdout: None,
            next_id: 0,
        }
    }

    fn prefix(&self, id: &str) -> String {
        format!("{}{id}", self.path_prefix)
    }

    async fn spawn_shim(&mut self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.temp_dir)?;
        let shim_path = self.temp_dir.join("stryker-vitest-shim.mjs");
        // Always overwrite: a stale shim from an older binary must never win.
        std::fs::write(&shim_path, SHIM_SOURCE)?;
        let mut cmd = tokio::process::Command::new("node");
        cmd.arg(shim_path.as_str())
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn()?;
        self.stdin = child.stdin.take();
        self.stdout = Some(BufReader::new(child.stdout.take().expect("stdout piped")).lines());
        self.child = Some(child);

        let reply = self
            .call(
                json!({
                    "kind": "init",
                    "cwd": self.cwd.as_str(),
                    "configFile": self.config_file,
                    "namespace": "__stryker__",
                }),
                Duration::from_secs(120),
            )
            .await?;
        match reply {
            ShimReply::Ready {} => Ok(()),
            other => anyhow::bail!("unexpected init reply: {other:?}"),
        }
    }

    async fn call(&mut self, mut msg: serde_json::Value, timeout: Duration) -> anyhow::Result<ShimReply> {
        self.next_id += 1;
        msg["id"] = json!(self.next_id);
        let stdin = self.stdin.as_mut().ok_or_else(|| anyhow::anyhow!("shim not running"))?;
        stdin.write_all(serde_json::to_string(&msg)?.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;

        let stdout = self.stdout.as_mut().ok_or_else(|| anyhow::anyhow!("shim not running"))?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let line = tokio::time::timeout_at(deadline, stdout.next_line()).await;
            let line = match line {
                Err(_) => {
                    self.kill().await;
                    anyhow::bail!("vitest shim timed out");
                }
                Ok(line) => line?,
            };
            let Some(line) = line else {
                self.kill().await;
                anyhow::bail!("vitest shim closed its stdout");
            };
            if line.trim().is_empty() {
                continue;
            }
            let reply: ShimReply = match serde_json::from_str(&line) {
                Ok(reply) => reply,
                Err(_) => continue, // stray output despite silencing; skip
            };
            if let ShimReply::Crash { error } = &reply {
                let error = error.clone();
                self.kill().await;
                anyhow::bail!("vitest shim crashed: {error}");
            }
            return Ok(reply);
        }
    }

    async fn kill(&mut self) {
        if let Some(child) = self.child.as_mut() {
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                unsafe {
                    libc::killpg(pid as i32, libc::SIGKILL);
                }
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        self.child = None;
        self.stdin = None;
        self.stdout = None;
    }

    async fn ensure_running(&mut self) -> anyhow::Result<()> {
        if self.child.is_none() {
            self.spawn_shim().await?;
        }
        Ok(())
    }
}

fn convert_coverage(shim: ShimCoverage, path_prefix: &str) -> MutantCoverage {
    let mut coverage = MutantCoverage::default();
    for (id, hits) in shim.static_hits {
        if let Ok(id) = id.parse::<u32>() {
            coverage.static_hits.insert(id, hits);
        }
    }
    for (test, buckets) in shim.per_test {
        let entry = coverage.per_test.entry(format!("{path_prefix}{test}")).or_default();
        for (id, hits) in buckets {
            if let Ok(id) = id.parse::<u32>() {
                entry.insert(id, hits);
            }
        }
    }
    coverage
}

#[async_trait]
impl TestRunner for VitestRunner {
    fn capabilities(&self) -> Capabilities {
        Capabilities { per_test_coverage: true }
    }

    async fn init(&mut self) -> anyhow::Result<()> {
        self.ensure_running().await
    }

    async fn dry_run(&mut self, options: &DryRunOptions) -> anyhow::Result<DryRunResult> {
        self.ensure_running().await?;
        let started = std::time::Instant::now();
        let reply = self
            .call(json!({"kind": "dryRun", "coverage": options.collect_coverage}), options.timeout)
            .await;
        let reply = match reply {
            Ok(reply) => reply,
            Err(e) if e.to_string().contains("timed out") => return Ok(DryRunResult::Timeout),
            Err(e) => return Ok(DryRunResult::Error(e.to_string())),
        };
        match reply {
            ShimReply::DryRunResult { tests, coverage } => Ok(DryRunResult::Complete {
                tests: tests
                    .into_iter()
                    .map(|t| TestResult {
                        id: self.prefix(&t.id),
                        name: self.prefix(&t.id),
                        file: Some(Utf8PathBuf::from(self.prefix(&t.file))),
                        time_ms: t.time_ms,
                        failed: t.state == "fail",
                        failure: t.error,
                    })
                    .collect(),
                coverage: coverage.map(|c| convert_coverage(c, &self.path_prefix)),
                gross_ms: started.elapsed().as_millis() as u64,
            }),
            other => Ok(DryRunResult::Error(format!("unexpected shim reply: {other:?}"))),
        }
    }

    async fn mutant_run(&mut self, options: &MutantRunOptions) -> anyhow::Result<MutantRunOutcome> {
        self.ensure_running().await?;
        let test_filter: Option<Vec<String>> = options.test_filter.as_ref().map(|filter| {
            filter
                .iter()
                .map(|id| id.strip_prefix(&self.path_prefix).unwrap_or(id).to_string())
                .collect()
        });
        let reply = self
            .call(
                json!({
                    "kind": "mutantRun",
                    "activeMutant": options.active_mutant.to_string(),
                    "testFilter": test_filter,
                    "hitLimit": options.hit_limit,
                }),
                options.timeout,
            )
            .await;
        let reply = match reply {
            Ok(reply) => reply,
            Err(e) if e.to_string().contains("timed out") => {
                // Shim was killed; a fresh one spawns on the next call.
                return Ok(MutantRunOutcome::Timeout { reason: Some("wall-clock timeout".into()) });
            }
            Err(e) => return Ok(MutantRunOutcome::Error(e.to_string())),
        };
        match reply {
            ShimReply::MutantRunResult { status, killed_by, tests_ran, failure_message, reason } => {
                Ok(match status.as_str() {
                    "killed" => MutantRunOutcome::Killed {
                        killed_by: killed_by.iter().map(|id| self.prefix(id)).collect(),
                        tests_ran,
                        failure: failure_message,
                    },
                    "survived" => MutantRunOutcome::Survived { tests_ran },
                    "timeout" => MutantRunOutcome::Timeout { reason },
                    other => MutantRunOutcome::Error(format!("unknown shim status {other:?}")),
                })
            }
            other => Ok(MutantRunOutcome::Error(format!("unexpected shim reply: {other:?}"))),
        }
    }

    async fn dispose(&mut self) -> anyhow::Result<()> {
        if self.child.is_some() {
            let _ = self.call(json!({"kind": "dispose"}), Duration::from_secs(5)).await;
            self.kill().await;
        }
        Ok(())
    }
}

impl Drop for VitestRunner {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                unsafe {
                    libc::killpg(pid as i32, libc::SIGKILL);
                }
            }
        }
    }
}

pub fn shim_config_file(_temp: &Utf8Path) -> Option<String> {
    None
}
