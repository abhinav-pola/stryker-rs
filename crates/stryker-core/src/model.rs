use std::collections::HashMap;
use std::time::Duration;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// Ordinal id assigned across all files in deterministic walk order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MutantId(pub u32);

impl std::fmt::Display for MutantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Runner-defined test identity: `<rel test file> > <describe path> > <name>`.
pub type TestId = String;

/// 1-based position, matching the mutation-testing report schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

/// Start inclusive, end exclusive (schema convention).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Clone)]
pub struct Mutant {
    pub id: MutantId,
    /// Relative to project root.
    pub file: Utf8PathBuf,
    /// Byte offsets in the ORIGINAL source.
    pub span: (u32, u32),
    /// 1-based line/col derived from the original parse.
    pub location: Location,
    /// Schema `mutatorName`, e.g. "EqualityOperator".
    pub mutator_name: &'static str,
    /// Code of the mutated subtree.
    pub replacement: String,
    /// Original source slice (for clear-text diffs).
    pub original: String,
    /// `// Stryker disable` reason; ignored mutants are never placed or run.
    pub ignored: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MutantStatus {
    Killed,
    Survived,
    NoCoverage,
    CompileError,
    RuntimeError,
    Timeout,
    Ignored,
    Pending,
}

impl MutantStatus {
    pub fn is_detected(self) -> bool {
        matches!(self, MutantStatus::Killed | MutantStatus::Timeout)
    }

    /// Counts toward the mutation score denominator.
    pub fn is_valid(self) -> bool {
        !matches!(
            self,
            MutantStatus::CompileError
                | MutantStatus::RuntimeError
                | MutantStatus::Ignored
                | MutantStatus::Pending
        )
    }
}

#[derive(Debug, Clone)]
pub struct MutantResult {
    pub status: MutantStatus,
    pub killed_by: Vec<TestId>,
    pub covered_by: Vec<TestId>,
    pub tests_ran: u32,
    pub status_reason: Option<String>,
    pub duration: Option<Duration>,
    /// True when the mutant was covered outside any test (module scope).
    pub is_static: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct TestInfo {
    pub id: TestId,
    pub name: String,
    pub file: Option<Utf8PathBuf>,
    pub time_ms: f64,
}

/// Coverage collected during the dry run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MutantCoverage {
    /// mutantId -> hits recorded while no test was running (module scope).
    #[serde(rename = "static", default)]
    pub static_hits: HashMap<u32, u64>,
    /// testId -> mutantId -> hits.
    #[serde(rename = "perTest", default)]
    pub per_test: HashMap<TestId, HashMap<u32, u64>>,
}

impl MutantCoverage {
    /// Total hits for a mutant across static + all per-test buckets.
    pub fn total_hits(&self, id: MutantId) -> u64 {
        let static_hits = self.static_hits.get(&id.0).copied().unwrap_or(0);
        let per_test: u64 = self
            .per_test
            .values()
            .filter_map(|m| m.get(&id.0))
            .sum();
        static_hits + per_test
    }

    pub fn is_static(&self, id: MutantId) -> bool {
        self.static_hits.contains_key(&id.0)
    }

    pub fn covering_tests(&self, id: MutantId) -> Vec<&TestId> {
        self.per_test
            .iter()
            .filter(|(_, hits)| hits.contains_key(&id.0))
            .map(|(test, _)| test)
            .collect()
    }
}

#[derive(Debug, Clone)]
pub enum MutantTestPlan {
    /// Result known without running tests (NoCoverage, Ignored).
    EarlyResult {
        mutant: MutantId,
        status: MutantStatus,
        reason: String,
    },
    Run {
        mutant: MutantId,
        /// None = run the full suite (static mutants, coverage off).
        test_filter: Option<Vec<TestId>>,
        /// timeoutFactor × netTime + timeoutMS + overhead, floor 1s.
        timeout: Duration,
        /// 100 × max dry-run hits for this mutant.
        hit_limit: Option<u64>,
        /// Always true for one-shot runners (bun, command).
        reload_environment: bool,
    },
}

impl MutantTestPlan {
    pub fn mutant_id(&self) -> MutantId {
        match self {
            MutantTestPlan::EarlyResult { mutant, .. } => *mutant,
            MutantTestPlan::Run { mutant, .. } => *mutant,
        }
    }
}
