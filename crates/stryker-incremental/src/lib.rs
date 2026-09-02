//! Incremental mode: reuse results from a previous run's report.
//!
//! The incremental file IS a full mutation-testing report (same as
//! stryker-js). Identity is content-based: (relative path, mutatorName,
//! replacement, location remapped through a line diff of old vs new source).
//!
//! Reuse rules (per-test tier):
//! - Killed: every killer test still exists and its test FILE content hash
//!   is unchanged.
//! - Survived / NoCoverage / Timeout: reused only when the whole test-file
//!   hash map is unchanged (a new or edited test anywhere could now cover
//!   or kill the mutant — conservative by design).
//!
//! Command tier has no per-test knowledge: the caller compares a command
//! fingerprint and passes `tests_unchanged` accordingly.

use std::collections::{BTreeMap, HashMap, HashSet};

use camino::Utf8PathBuf;
use similar::{ChangeTag, TextDiff};
use stryker_core::{Mutant, MutantResult, MutantStatus};
use stryker_reporters::schema::{MutantResultJson, MutationTestResult};

pub struct IncrementalInput<'a> {
    pub old_report: &'a MutationTestResult,
    pub mutants: &'a [Mutant],
    /// New sources of every mutate target.
    pub sources: &'a BTreeMap<Utf8PathBuf, String>,
    /// Test ids present in the new dry run (empty set = unknown/command tier).
    pub new_test_ids: &'a HashSet<String>,
    /// file -> content hash, old run (from report config) and new run.
    pub old_test_hashes: &'a BTreeMap<String, String>,
    pub new_test_hashes: &'a BTreeMap<String, String>,
    /// Command-tier signal: true when the test command fingerprint matches.
    pub command_fingerprint_matches: Option<bool>,
}

/// Results that can be carried over without re-running tests.
pub fn reusable_results(input: &IncrementalInput<'_>) -> BTreeMap<u32, MutantResult> {
    let tests_unchanged = match input.command_fingerprint_matches {
        Some(matches) => matches,
        None => input.old_test_hashes == input.new_test_hashes && !input.new_test_hashes.is_empty(),
    };

    let mut reused = BTreeMap::new();
    // Group new mutants per file so each file's diff is computed once.
    let mut by_file: BTreeMap<&Utf8PathBuf, Vec<&Mutant>> = BTreeMap::new();
    for mutant in input.mutants {
        by_file.entry(&mutant.file).or_default().push(mutant);
    }

    for (file, mutants) in by_file {
        let Some(old_file) = input.old_report.files.get(file.as_str()) else { continue };
        let Some(new_source) = input.sources.get(file) else { continue };
        let line_map = old_to_new_line_map(&old_file.source, new_source);

        // (mutator, replacement, new line) -> old results. Column is a
        // tiebreak: exact column match wins, otherwise first unclaimed.
        let mut index: HashMap<(String, String, u32), Vec<&MutantResultJson>> = HashMap::new();
        for old in &old_file.mutants {
            // EVERY line of the mutant's span must survive unchanged and
            // contiguously — an interior edit of a multi-line mutant (e.g. a
            // BlockStatement body) makes the old result stale even though the
            // first line is untouched.
            let Some(new_line) = remap_span(&line_map, old.location.start.line, old.location.end.line)
            else {
                continue;
            };
            let Some(replacement) = &old.replacement else { continue };
            index
                .entry((old.mutator_name.clone(), replacement.clone(), new_line))
                .or_default()
                .push(old);
        }

        for mutant in mutants {
            let key = (
                mutant.mutator_name.to_string(),
                mutant.replacement.clone(),
                mutant.location.start.line,
            );
            let Some(candidates) = index.get_mut(&key) else { continue };
            let position = candidates
                .iter()
                .position(|c| c.location.start.column == mutant.location.start.column)
                .or_else(|| (!candidates.is_empty()).then_some(0));
            let Some(position) = position else { continue };
            let old = candidates.remove(position);

            if let Some(result) = reuse_decision(old, input, tests_unchanged) {
                reused.insert(mutant.id.0, result);
            }
        }
    }
    reused
}

fn reuse_decision(
    old: &MutantResultJson,
    input: &IncrementalInput<'_>,
    tests_unchanged: bool,
) -> Option<MutantResult> {
    let reusable = match old.status {
        MutantStatus::Killed => match &old.killed_by {
            Some(killers) if !killers.is_empty() && input.command_fingerprint_matches.is_none() => {
                killers.iter().all(|killer| {
                    input.new_test_ids.contains(killer)
                        && killer_file_unchanged(killer, input)
                })
            }
            _ => tests_unchanged,
        },
        MutantStatus::Survived | MutantStatus::NoCoverage | MutantStatus::Timeout => {
            tests_unchanged
        }
        // Ignored/Pending/errors are cheap or config-dependent: recompute.
        _ => false,
    };
    if !reusable {
        return None;
    }
    Some(MutantResult {
        status: old.status,
        killed_by: old.killed_by.clone().unwrap_or_default(),
        covered_by: old.covered_by.clone().unwrap_or_default(),
        tests_ran: old.tests_completed.unwrap_or(0),
        status_reason: old.status_reason.clone(),
        duration: None,
        is_static: old.is_static,
    })
}

fn killer_file_unchanged(killer_test_id: &str, input: &IncrementalInput<'_>) -> bool {
    let Some((file, _)) = killer_test_id.split_once(" > ") else {
        return false;
    };
    match (input.old_test_hashes.get(file), input.new_test_hashes.get(file)) {
        (Some(old), Some(new)) => old == new,
        _ => false,
    }
}

/// New start line iff every line in [start, end] is unchanged and shifted by
/// the same offset.
fn remap_span(line_map: &HashMap<u32, u32>, start: u32, end: u32) -> Option<u32> {
    let new_start = *line_map.get(&start)?;
    let offset = new_start as i64 - start as i64;
    for line in start..=end.max(start) {
        let mapped = *line_map.get(&line)?;
        if mapped as i64 - line as i64 != offset {
            return None;
        }
    }
    Some(new_start)
}

/// Map 1-based old line numbers to new line numbers for UNCHANGED lines.
fn old_to_new_line_map(old: &str, new: &str) -> HashMap<u32, u32> {
    let diff = TextDiff::from_lines(old, new);
    let mut map = HashMap::new();
    for change in diff.iter_all_changes() {
        if change.tag() == ChangeTag::Equal {
            if let (Some(old_index), Some(new_index)) = (change.old_index(), change.new_index()) {
                map.insert(old_index as u32 + 1, new_index as u32 + 1);
            }
        }
    }
    map
}

pub fn hash_content(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stryker_core::{Location, MutantId, Position};
    use stryker_reporters::schema::*;

    fn old_report(source: &str, mutants: Vec<MutantResultJson>) -> MutationTestResult {
        let mut files = BTreeMap::new();
        files.insert(
            "src/a.ts".to_string(),
            FileResult { language: "typescript".into(), source: source.into(), mutants },
        );
        MutationTestResult {
            schema_version: SCHEMA_VERSION.into(),
            thresholds: SchemaThresholds { high: 80.0, low: 60.0 },
            project_root: None,
            files,
            test_files: None,
            framework: None,
            config: None,
            performance: None,
        }
    }

    fn old_mutant(line: u32, column: u32, status: MutantStatus, killed_by: Option<Vec<String>>) -> MutantResultJson {
        MutantResultJson {
            id: "old".into(),
            mutator_name: "EqualityOperator".into(),
            location: Location {
                start: Position { line, column },
                end: Position { line, column: column + 5 },
            },
            status,
            replacement: Some("a !== b".into()),
            covered_by: None,
            killed_by,
            description: None,
            duration: None,
            is_static: None,
            status_reason: None,
            tests_completed: Some(1),
        }
    }

    fn new_mutant(line: u32, column: u32) -> Mutant {
        Mutant {
            id: MutantId(7),
            file: Utf8PathBuf::from("src/a.ts"),
            span: (0, 0),
            location: Location {
                start: Position { line, column },
                end: Position { line, column: column + 5 },
            },
            mutator_name: "EqualityOperator",
            replacement: "a !== b".into(),
            original: "a === b".into(),
            ignored: None,
        }
    }

    #[test]
    fn reuses_killed_after_lines_inserted_above() {
        let old_source = "const x = a === b;\n";
        let new_source = "// new comment\nconst y = 1;\nconst x = a === b;\n";
        let killer = "src/a.test.ts > works".to_string();
        let report = old_report(
            old_source,
            vec![old_mutant(1, 11, MutantStatus::Killed, Some(vec![killer.clone()]))],
        );
        let mut sources = BTreeMap::new();
        sources.insert(Utf8PathBuf::from("src/a.ts"), new_source.to_string());
        let new_test_ids: HashSet<String> = [killer].into();
        let hashes: BTreeMap<String, String> =
            [("src/a.test.ts".to_string(), "h1".to_string())].into();

        let reused = reusable_results(&IncrementalInput {
            old_report: &report,
            mutants: &[new_mutant(3, 11)], // shifted down two lines
            sources: &sources,
            new_test_ids: &new_test_ids,
            old_test_hashes: &hashes,
            new_test_hashes: &hashes,
            command_fingerprint_matches: None,
        });
        assert_eq!(reused.len(), 1);
        assert_eq!(reused[&7].status, MutantStatus::Killed);
    }

    #[test]
    fn does_not_reuse_killed_when_killer_changed() {
        let source = "const x = a === b;\n";
        let killer = "src/a.test.ts > works".to_string();
        let report =
            old_report(source, vec![old_mutant(1, 11, MutantStatus::Killed, Some(vec![killer.clone()]))]);
        let mut sources = BTreeMap::new();
        sources.insert(Utf8PathBuf::from("src/a.ts"), source.to_string());
        let new_test_ids: HashSet<String> = [killer].into();
        let old_hashes: BTreeMap<String, String> =
            [("src/a.test.ts".to_string(), "h1".to_string())].into();
        let new_hashes: BTreeMap<String, String> =
            [("src/a.test.ts".to_string(), "h2".to_string())].into();

        let reused = reusable_results(&IncrementalInput {
            old_report: &report,
            mutants: &[new_mutant(1, 11)],
            sources: &sources,
            new_test_ids: &new_test_ids,
            old_test_hashes: &old_hashes,
            new_test_hashes: &new_hashes,
            command_fingerprint_matches: None,
        });
        assert!(reused.is_empty());
    }

    #[test]
    fn survived_needs_fully_unchanged_tests() {
        let source = "const x = a === b;\n";
        let report = old_report(source, vec![old_mutant(1, 11, MutantStatus::Survived, None)]);
        let mut sources = BTreeMap::new();
        sources.insert(Utf8PathBuf::from("src/a.ts"), source.to_string());
        let new_test_ids: HashSet<String> = HashSet::new();
        let old_hashes: BTreeMap<String, String> =
            [("src/a.test.ts".to_string(), "h1".to_string())].into();
        let mut new_hashes = old_hashes.clone();

        let reused = reusable_results(&IncrementalInput {
            old_report: &report,
            mutants: &[new_mutant(1, 11)],
            sources: &sources,
            new_test_ids: &new_test_ids,
            old_test_hashes: &old_hashes,
            new_test_hashes: &new_hashes,
            command_fingerprint_matches: None,
        });
        assert_eq!(reused[&7].status, MutantStatus::Survived);

        // A new test file anywhere invalidates survivors.
        new_hashes.insert("src/b.test.ts".to_string(), "h9".to_string());
        let reused = reusable_results(&IncrementalInput {
            old_report: &report,
            mutants: &[new_mutant(1, 11)],
            sources: &sources,
            new_test_ids: &new_test_ids,
            old_test_hashes: &old_hashes,
            new_test_hashes: &new_hashes,
            command_fingerprint_matches: None,
        });
        assert!(reused.is_empty());
    }

    #[test]
    fn edited_mutated_line_is_not_reused() {
        let old_source = "const x = a === b;\n";
        let new_source = "const x = a === c;\n"; // the line itself changed
        let report = old_report(
            old_source,
            vec![old_mutant(1, 11, MutantStatus::Killed, Some(vec!["t > k".into()]))],
        );
        let mut sources = BTreeMap::new();
        sources.insert(Utf8PathBuf::from("src/a.ts"), new_source.to_string());
        let new_test_ids: HashSet<String> = ["t > k".to_string()].into();
        let hashes: BTreeMap<String, String> = [("t".to_string(), "h".to_string())].into();

        let reused = reusable_results(&IncrementalInput {
            old_report: &report,
            mutants: &[new_mutant(1, 11)],
            sources: &sources,
            new_test_ids: &new_test_ids,
            old_test_hashes: &hashes,
            new_test_hashes: &hashes,
            command_fingerprint_matches: None,
        });
        assert!(reused.is_empty());
    }
}
