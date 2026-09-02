//! Build the schema report from run results, and compute score metrics.

use std::collections::BTreeMap;

use camino::Utf8PathBuf;
use stryker_core::{Mutant, MutantResult, MutantStatus};

use crate::schema::*;

pub struct ReportInput<'a> {
    /// Every mutate target (even zero-mutant ones) with its ORIGINAL source.
    pub file_sources: &'a BTreeMap<Utf8PathBuf, String>,
    pub mutants: &'a [Mutant],
    /// Indexed by mutant id.
    pub results: &'a BTreeMap<u32, MutantResult>,
    pub thresholds_high: f64,
    pub thresholds_low: f64,
    pub project_root: Option<String>,
    pub config: Option<serde_json::Value>,
    pub test_files: Option<BTreeMap<String, TestFile>>,
    pub performance: Option<Performance>,
}

pub fn build_report(input: &ReportInput<'_>) -> MutationTestResult {
    let mut files: BTreeMap<String, FileResult> = input
        .file_sources
        .iter()
        .map(|(path, source)| {
            (
                path.to_string(),
                FileResult {
                    language: language_for(path.as_str()).to_string(),
                    source: source.clone(),
                    mutants: Vec::new(),
                },
            )
        })
        .collect();

    for mutant in input.mutants {
        let result = input.results.get(&mutant.id.0);
        let status = result.map_or(MutantStatus::Pending, |r| r.status);
        let entry = MutantResultJson {
            id: mutant.id.to_string(),
            mutator_name: mutant.mutator_name.to_string(),
            location: mutant.location,
            status,
            replacement: Some(mutant.replacement.clone()),
            covered_by: result
                .filter(|r| !r.covered_by.is_empty())
                .map(|r| r.covered_by.clone()),
            killed_by: result.filter(|r| !r.killed_by.is_empty()).map(|r| r.killed_by.clone()),
            description: None,
            duration: result.and_then(|r| r.duration).map(|d| d.as_millis() as u64),
            is_static: result.and_then(|r| r.is_static),
            status_reason: result.and_then(|r| r.status_reason.clone()),
            tests_completed: result.map(|r| r.tests_ran).filter(|n| *n > 0),
        };
        files
            .entry(mutant.file.to_string())
            .or_insert_with(|| FileResult {
                language: language_for(mutant.file.as_str()).to_string(),
                source: String::new(),
                mutants: Vec::new(),
            })
            .mutants
            .push(entry);
    }

    MutationTestResult {
        schema_version: SCHEMA_VERSION.to_string(),
        thresholds: SchemaThresholds { high: input.thresholds_high, low: input.thresholds_low },
        project_root: input.project_root.clone(),
        files,
        test_files: input.test_files.clone(),
        framework: Some(FrameworkInformation {
            name: "stryker-rs".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            branding: Some(Branding {
                homepage_url: "https://github.com/abhinavpola/stryker-rs".to_string(),
                image_url: None,
            }),
        }),
        config: input.config.clone(),
        performance: input.performance,
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Metrics {
    pub killed: u32,
    pub survived: u32,
    pub timeout: u32,
    pub no_coverage: u32,
    pub runtime_errors: u32,
    pub compile_errors: u32,
    pub ignored: u32,
    pub pending: u32,
}

impl Metrics {
    pub fn count(statuses: impl Iterator<Item = MutantStatus>) -> Self {
        let mut m = Metrics::default();
        for s in statuses {
            match s {
                MutantStatus::Killed => m.killed += 1,
                MutantStatus::Survived => m.survived += 1,
                MutantStatus::Timeout => m.timeout += 1,
                MutantStatus::NoCoverage => m.no_coverage += 1,
                MutantStatus::RuntimeError => m.runtime_errors += 1,
                MutantStatus::CompileError => m.compile_errors += 1,
                MutantStatus::Ignored => m.ignored += 1,
                MutantStatus::Pending => m.pending += 1,
            }
        }
        m
    }

    pub fn detected(&self) -> u32 {
        self.killed + self.timeout
    }

    pub fn undetected(&self) -> u32 {
        self.survived + self.no_coverage
    }

    pub fn valid(&self) -> u32 {
        self.detected() + self.undetected()
    }

    /// None when there are no valid mutants (score undefined).
    pub fn mutation_score(&self) -> Option<f64> {
        let valid = self.valid();
        (valid > 0).then(|| self.detected() as f64 / valid as f64 * 100.0)
    }
}
