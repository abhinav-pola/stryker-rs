use std::collections::BTreeSet;

use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;

use crate::mutate_pattern::MutatePattern;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TestRunnerKind {
    Command,
    Bun,
    Vitest,
    /// bun + vitest for the same target (mixed-runtime repos); scope each
    /// runner's test files so they don't overlap.
    Composite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CoverageAnalysis {
    Off,
    All,
    PerTest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanTempDir {
    Never,
    OnSuccess,
    Always,
}

impl<'de> Deserialize<'de> for CleanTempDir {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bool(bool),
            Str(String),
        }
        match Raw::deserialize(d)? {
            Raw::Bool(true) => Ok(CleanTempDir::OnSuccess),
            Raw::Bool(false) => Ok(CleanTempDir::Never),
            Raw::Str(s) if s == "always" => Ok(CleanTempDir::Always),
            Raw::Str(s) => Err(serde::de::Error::custom(format!(
                "cleanTempDir must be true, false or \"always\", got {s:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct CommandRunnerConfig {
    pub command: Option<String>,
    /// Force execution through `bash -c` (auto-detected for commands with
    /// shell syntax like `&&`).
    pub shell: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct BunRunnerConfig {
    /// Extra args appended to `bun test` (e.g. more `--preload` entries).
    pub args: Vec<String>,
    /// Test file globs; defaults to bun's own discovery when empty.
    pub test_files: Vec<String>,
    /// Extra environment for every `bun test` invocation
    /// (e.g. RTL_SKIP_AUTO_CLEANUP for dom tests).
    pub env: std::collections::BTreeMap<String, String>,
    /// Directory (relative to the project root) to run `bun test` from.
    /// Monorepo packages need their own bunfig.toml preloads; report paths
    /// and test ids stay project-root-relative.
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct VitestRunnerConfig {
    pub config_file: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct MutatorConfig {
    pub excluded_mutations: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct FileNameOpt {
    pub file_name: Option<Utf8PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Thresholds {
    pub high: f64,
    pub low: f64,
    pub r#break: Option<f64>,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self { high: 80.0, low: 60.0, r#break: None }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StrykerConfig {
    pub mutate: Vec<String>,
    pub test_runner: TestRunnerKind,
    pub command_runner: CommandRunnerConfig,
    pub bun_runner: BunRunnerConfig,
    pub vitest_runner: VitestRunnerConfig,
    /// None = pick a default per runner (perTest for bun/vitest, off for command).
    pub coverage_analysis: Option<CoverageAnalysis>,
    pub concurrency: Option<usize>,
    #[serde(rename = "timeoutMS")]
    pub timeout_ms: u64,
    pub timeout_factor: f64,
    pub dry_run_timeout_minutes: u64,
    pub ignore_patterns: Vec<String>,
    pub in_place: bool,
    pub temp_dir_name: String,
    pub clean_temp_dir: CleanTempDir,
    pub reporters: Vec<String>,
    pub json_reporter: FileNameOpt,
    pub html_reporter: FileNameOpt,
    pub thresholds: Thresholds,
    pub incremental: bool,
    pub incremental_file: Option<Utf8PathBuf>,
    pub force: bool,
    pub mutator: MutatorConfig,
    pub disable_type_checks: bool,
    pub dry_run_only: bool,
    pub allow_empty: bool,
    pub log_level: Option<String>,
}

impl Default for StrykerConfig {
    fn default() -> Self {
        Self {
            mutate: vec![
                "{src,lib}/**/*.{js,jsx,ts,tsx,mjs,mts,cjs,cts}".to_string(),
                "!{src,lib}/**/*.d.{ts,mts,cts}".to_string(),
            ],
            test_runner: TestRunnerKind::Command,
            command_runner: CommandRunnerConfig::default(),
            bun_runner: BunRunnerConfig::default(),
            vitest_runner: VitestRunnerConfig::default(),
            coverage_analysis: None,
            concurrency: None,
            timeout_ms: 5000,
            timeout_factor: 1.5,
            dry_run_timeout_minutes: 5,
            ignore_patterns: Vec::new(),
            in_place: false,
            temp_dir_name: ".stryker-tmp".to_string(),
            clean_temp_dir: CleanTempDir::OnSuccess,
            reporters: vec!["clear-text".into(), "progress".into(), "html".into()],
            json_reporter: FileNameOpt::default(),
            html_reporter: FileNameOpt::default(),
            thresholds: Thresholds::default(),
            incremental: false,
            incremental_file: None,
            force: false,
            mutator: MutatorConfig::default(),
            disable_type_checks: true,
            dry_run_only: false,
            allow_empty: false,
            log_level: None,
        }
    }
}

/// Test-file name suffixes never mutated even when a `mutate` glob matches.
pub const TEST_FILE_MARKERS: &[&str] = &[".test.", ".spec.", ".stories."];

pub const SUPPORTED_CONFIG_FILE_NAMES: &[&str] = &[
    "stryker.config.json",
    "stryker.config.jsonc",
    "stryker.conf.json",
    "stryker.config.mjs",
    "stryker.config.js",
    "stryker.conf.mjs",
    "stryker.conf.js",
];

impl StrykerConfig {
    /// Effective coverage analysis: the command runner cannot observe per-test
    /// coverage, so it is always `off` there.
    pub fn effective_coverage(&self) -> CoverageAnalysis {
        match self.test_runner {
            TestRunnerKind::Command => CoverageAnalysis::Off,
            TestRunnerKind::Bun | TestRunnerKind::Vitest | TestRunnerKind::Composite => {
                self.coverage_analysis.unwrap_or(CoverageAnalysis::PerTest)
            }
        }
    }

    /// Stryker's default: n for small machines, n-1 above 4 cores.
    pub fn effective_concurrency(&self) -> usize {
        self.concurrency.unwrap_or_else(|| {
            let n = std::thread::available_parallelism().map_or(4, |n| n.get());
            if n > 4 { n - 1 } else { n }
        })
    }

    pub fn effective_incremental_file(&self, project_root: &Utf8Path) -> Utf8PathBuf {
        let rel = self
            .incremental_file
            .clone()
            .unwrap_or_else(|| Utf8PathBuf::from("reports/stryker-incremental.json"));
        if rel.is_absolute() { rel } else { project_root.join(rel) }
    }

    pub fn parsed_mutate(&self) -> anyhow::Result<Vec<MutatePattern>> {
        self.mutate.iter().map(|p| MutatePattern::parse(p)).collect()
    }
}

/// Load and parse a config file. JSON/JSONC are parsed directly; executable
/// `.mjs`/`.js` configs are evaluated by `bun` (fallback `node`) via the
/// bundled config-dump script, which prints the default export as JSON.
pub fn load_config(path: &Utf8Path) -> anyhow::Result<StrykerConfig> {
    let json_value: serde_json::Value = match path.extension() {
        Some("json") | Some("jsonc") => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("cannot read config {path}: {e}"))?;
            let parse_opts = jsonc_parser::ParseOptions::default();
            jsonc_parser::parse_to_serde_value::<serde_json::Value>(&text, &parse_opts)?
        }
        Some("mjs") | Some("js") | Some("cjs") => dump_js_config(path)?,
        other => anyhow::bail!("unsupported config extension {other:?} for {path}"),
    };
    parse_config_value(json_value, path)
}

/// Find a config file in `dir` following the discovery order.
pub fn discover_config(dir: &Utf8Path) -> Option<Utf8PathBuf> {
    SUPPORTED_CONFIG_FILE_NAMES
        .iter()
        .map(|name| dir.join(name))
        .find(|p| p.exists())
}

fn parse_config_value(value: serde_json::Value, path: &Utf8Path) -> anyhow::Result<StrykerConfig> {
    let known: BTreeSet<&str> = [
        "mutate", "testRunner", "commandRunner", "bunRunner", "vitestRunner",
        "coverageAnalysis", "concurrency", "timeoutMS", "timeoutFactor",
        "dryRunTimeoutMinutes", "ignorePatterns", "inPlace", "tempDirName",
        "cleanTempDir", "reporters", "jsonReporter", "htmlReporter", "thresholds",
        "incremental", "incrementalFile", "force", "mutator", "disableTypeChecks",
        "dryRunOnly", "allowEmpty", "logLevel",
        // accepted-and-ignored stryker-js keys, so real-world configs load cleanly
        "$schema", "packageManager", "plugins", "appendPlugins", "checkers",
        "warnings", "allowConsoleColors", "symlinkNodeModules", "buildCommand",
        "maxTestRunnerReuse", "disableBail", "testRunnerNodeArgs", "checkerNodeArgs",
        "tsconfigFile", "ignoreStatic", "ignorers", "testFiles", "dashboard",
        "clearTextReporter", "eventReporter", "fileLogLevel", "inspect",
    ]
    .into();
    if let Some(map) = value.as_object() {
        for key in map.keys() {
            if !known.contains(key.as_str()) {
                tracing::warn!("unknown config key {key:?} in {path} (ignored)");
            }
        }
    }
    let config: StrykerConfig = serde_json::from_value(value)
        .map_err(|e| anyhow::anyhow!("invalid config {path}: {e}"))?;
    Ok(config)
}

fn dump_js_config(path: &Utf8Path) -> anyhow::Result<serde_json::Value> {
    let dump_script = include_str!("../../../js/config-dump.mjs");
    let tmp = std::env::temp_dir().join(format!("stryker-config-dump-{}.mjs", std::process::id()));
    std::fs::write(&tmp, dump_script)?;
    let result = (|| {
        for runtime in ["bun", "node"] {
            let output = std::process::Command::new(runtime)
                .arg(&tmp)
                .arg(path.as_str())
                .output();
            match output {
                Ok(out) if out.status.success() => {
                    return Ok(serde_json::from_slice(&out.stdout)?);
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    anyhow::bail!("evaluating {path} with {runtime} failed: {stderr}");
                }
                Err(_) => continue, // runtime not installed; try the next one
            }
        }
        anyhow::bail!("neither `bun` nor `node` is available to evaluate {path}")
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = StrykerConfig::default();
        assert_eq!(c.timeout_ms, 5000);
        assert_eq!(c.effective_coverage(), CoverageAnalysis::Off); // command runner
        assert_eq!(c.clean_temp_dir, CleanTempDir::OnSuccess);
    }

    #[test]
    fn parses_harness_generated_config() {
        let json = serde_json::json!({
            "testRunner": "command",
            "commandRunner": { "command": "bash ../../scripts/run-tests.sh --preload ./setup.ts" },
            "coverageAnalysis": "off",
            "mutate": ["packages/core/index.ts:100-200", "apps/web/app/[(]user[)]/page.tsx"],
            "mutator": { "excludedMutations": ["StringLiteral", "ObjectLiteral"] },
            "ignorePatterns": ["**", "!package.json", "!packages/core/**"],
            "concurrency": 4,
            "timeoutMS": 30000,
            "inPlace": true,
            "tempDirName": ".stryker-tmp-x",
            "cleanTempDir": true,
            "reporters": ["clear-text", "json", "html"],
            "jsonReporter": { "fileName": "reports/mutation/core.json" },
            "htmlReporter": { "fileName": "reports/mutation/core.html" },
            "thresholds": { "break": 50 },
            "incremental": true,
            "incrementalFile": "reports/mutation/core.incremental.json",
            "force": false
        });
        let c = parse_config_value(json, Utf8Path::new("test.json")).unwrap();
        assert_eq!(c.test_runner, TestRunnerKind::Command);
        assert_eq!(c.timeout_ms, 30000);
        assert!(c.in_place);
        assert_eq!(c.thresholds.r#break, Some(50.0));
        assert_eq!(c.thresholds.high, 80.0); // default preserved when only break given
        assert_eq!(c.mutator.excluded_mutations, vec!["StringLiteral", "ObjectLiteral"]);
        let patterns = c.parsed_mutate().unwrap();
        assert_eq!(patterns.len(), 2);
    }

    #[test]
    fn clean_temp_dir_always() {
        let json = serde_json::json!({ "cleanTempDir": "always" });
        let c = parse_config_value(json, Utf8Path::new("t.json")).unwrap();
        assert_eq!(c.clean_temp_dir, CleanTempDir::Always);
    }
}
