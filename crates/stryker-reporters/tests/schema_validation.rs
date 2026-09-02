//! Validate our serialized report against the pinned upstream JSON schema.

use std::collections::BTreeMap;
use std::time::Duration;

use camino::Utf8PathBuf;
use stryker_core::{Location, Mutant, MutantId, MutantResult, MutantStatus, Position};
use stryker_reporters::report::{ReportInput, build_report};

#[test]
fn report_matches_upstream_schema() {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../schema/mutation-testing-report-schema.json"
    ))
    .unwrap();

    let mutants = vec![Mutant {
        id: MutantId(0),
        file: Utf8PathBuf::from("src/a.ts"),
        span: (10, 15),
        location: Location {
            start: Position { line: 1, column: 11 },
            end: Position { line: 1, column: 16 },
        },
        mutator_name: "EqualityOperator",
        replacement: "a !== b".into(),
        original: "a === b".into(),
        ignored: None,
    }];
    let mut results = BTreeMap::new();
    results.insert(
        0,
        MutantResult {
            status: MutantStatus::Killed,
            killed_by: vec!["src/a.test.ts > works".into()],
            covered_by: vec!["src/a.test.ts > works".into()],
            tests_ran: 1,
            status_reason: None,
            duration: Some(Duration::from_millis(12)),
            is_static: Some(false),
        },
    );
    let mut file_sources = BTreeMap::new();
    file_sources.insert(Utf8PathBuf::from("src/a.ts"), "const x = a === b;".to_string());
    // A zero-mutant file must be representable too (CI parsers require it).
    file_sources.insert(Utf8PathBuf::from("src/empty.ts"), "export {};".to_string());

    let report = build_report(&ReportInput {
        file_sources: &file_sources,
        mutants: &mutants,
        results: &results,
        thresholds_high: 80.0,
        thresholds_low: 60.0,
        project_root: Some("/repo".into()),
        config: Some(serde_json::json!({"strykerRs": {"testCommandFingerprint": "abc"}})),
        test_files: None,
        performance: Some(stryker_reporters::schema::Performance {
            setup: 1,
            initial_run: 2,
            mutation: 3,
        }),
    });

    let value = serde_json::to_value(&report).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator.iter_errors(&value).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "schema violations: {errors:#?}");

    // Round-trips (incremental mode reads reports back).
    let text = serde_json::to_string(&report).unwrap();
    let _parsed: stryker_reporters::schema::MutationTestResult =
        serde_json::from_str(&text).unwrap();
}
