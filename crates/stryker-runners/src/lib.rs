pub mod bun;
pub mod command;
pub mod composite;
pub mod junit;
pub mod process;
pub mod vitest;

use std::time::Duration;

use async_trait::async_trait;
use camino::Utf8PathBuf;
use stryker_core::{MutantCoverage, MutantId, TestId};

#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    pub per_test_coverage: bool,
}

#[derive(Debug, Clone)]
pub struct DryRunOptions {
    pub timeout: Duration,
    pub collect_coverage: bool,
}

#[derive(Debug, Clone)]
pub struct TestResult {
    pub id: TestId,
    pub name: String,
    pub file: Option<Utf8PathBuf>,
    pub time_ms: f64,
    pub failed: bool,
    pub failure: Option<String>,
}

#[derive(Debug)]
pub enum DryRunResult {
    Complete {
        tests: Vec<TestResult>,
        coverage: Option<MutantCoverage>,
        gross_ms: u64,
    },
    Error(String),
    Timeout,
}

#[derive(Debug, Clone)]
pub struct MutantRunOptions {
    pub active_mutant: MutantId,
    /// None = run everything (static mutants, coverage off).
    pub test_filter: Option<Vec<TestId>>,
    pub timeout: Duration,
    pub hit_limit: Option<u64>,
}

#[derive(Debug)]
pub enum MutantRunOutcome {
    Killed {
        killed_by: Vec<TestId>,
        tests_ran: u32,
        failure: Option<String>,
    },
    Survived {
        tests_ran: u32,
    },
    Timeout {
        reason: Option<String>,
    },
    Error(String),
}

#[async_trait]
pub trait TestRunner: Send {
    fn capabilities(&self) -> Capabilities;
    async fn init(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn dry_run(&mut self, options: &DryRunOptions) -> anyhow::Result<DryRunResult>;
    async fn mutant_run(&mut self, options: &MutantRunOptions) -> anyhow::Result<MutantRunOutcome>;
    async fn dispose(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Factory so each scheduler worker gets its own runner instance.
pub type RunnerFactory = Box<dyn Fn() -> anyhow::Result<Box<dyn TestRunner>> + Send + Sync>;
