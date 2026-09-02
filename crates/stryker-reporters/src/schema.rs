//! Serde types for the mutation-testing report schema v2
//! (https://github.com/stryker-mutator/mutation-testing-elements,
//! packages/report-schema). This JSON shape is a load-bearing contract:
//! downstream CI scripts parse `files[path].mutants[]` directly.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use stryker_core::{Location, MutantStatus};

pub const SCHEMA_VERSION: &str = "2";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationTestResult {
    pub schema_version: String,
    pub thresholds: SchemaThresholds,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    /// Keyed by project-root-relative path (forward slashes).
    pub files: BTreeMap<String, FileResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_files: Option<BTreeMap<String, TestFile>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub framework: Option<FrameworkInformation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub performance: Option<Performance>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SchemaThresholds {
    pub high: f64,
    pub low: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileResult {
    pub language: String,
    pub source: String,
    pub mutants: Vec<MutantResultJson>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutantResultJson {
    pub id: String,
    pub mutator_name: String,
    pub location: Location,
    pub status: MutantStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replacement: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub covered_by: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub killed_by: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    #[serde(rename = "static", skip_serializing_if = "Option::is_none")]
    pub is_static: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests_completed: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub tests: Vec<TestDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestDefinition {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<OpenEndLocation>,
}

/// Schema's test location: `end` is optional.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenEndLocation {
    pub start: stryker_core::Position,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<stryker_core::Position>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameworkInformation {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branding: Option<Branding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Branding {
    pub homepage_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Performance {
    pub setup: u64,
    pub initial_run: u64,
    pub mutation: u64,
}

pub fn language_for(path: &str) -> &'static str {
    if path.ends_with(".tsx") || path.ends_with(".jsx") {
        "typescript" // Prism uses `typescript` for tsx as well
    } else if path.ends_with(".ts") || path.ends_with(".mts") || path.ends_with(".cts") {
        "typescript"
    } else {
        "javascript"
    }
}
