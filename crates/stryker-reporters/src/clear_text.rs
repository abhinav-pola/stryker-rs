//! Clear-text reporter: per-mutant survivors + a per-directory score table.

use std::collections::BTreeMap;
use std::fmt::Write;

use stryker_core::MutantStatus;

use crate::report::Metrics;
use crate::schema::MutationTestResult;

pub fn render(report: &MutationTestResult) -> String {
    let mut out = String::new();

    // Survivor / no-coverage / error detail lines.
    for (path, file) in &report.files {
        for m in &file.mutants {
            let show = matches!(
                m.status,
                MutantStatus::Survived | MutantStatus::NoCoverage | MutantStatus::RuntimeError
            );
            if !show {
                continue;
            }
            let _ = writeln!(
                out,
                "[{:?}] {} {}:{}:{}",
                m.status, m.mutator_name, path, m.location.start.line, m.location.start.column
            );
            let original = original_snippet(&file.source, m);
            if let Some(orig) = original {
                let _ = writeln!(out, "-   {orig}");
            }
            if let Some(replacement) = &m.replacement {
                let _ = writeln!(out, "+   {}", replacement.replace('\n', "\\n"));
            }
        }
    }

    // Score table grouped by top-level directory of each file.
    let mut by_dir: BTreeMap<String, Vec<MutantStatus>> = BTreeMap::new();
    let mut all = Vec::new();
    for (path, file) in &report.files {
        let dir = path.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
        for m in &file.mutants {
            by_dir.entry(dir.clone()).or_default().push(m.status);
            all.push(m.status);
        }
    }

    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{:<50} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "File / directory", "score", "killed", "timeout", "survived", "no-cov", "errors"
    );
    let _ = writeln!(out, "{}", "-".repeat(104));
    for (dir, statuses) in &by_dir {
        let m = Metrics::count(statuses.iter().copied());
        let _ = writeln!(
            out,
            "{:<50} {:>7} {:>8} {:>8} {:>8} {:>8} {:>8}",
            truncate(dir, 50),
            score_text(&m),
            m.killed,
            m.timeout,
            m.survived,
            m.no_coverage,
            m.runtime_errors + m.compile_errors,
        );
    }
    let total = Metrics::count(all.into_iter());
    let _ = writeln!(out, "{}", "-".repeat(104));
    let _ = writeln!(
        out,
        "{:<50} {:>7} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "All files",
        score_text(&total),
        total.killed,
        total.timeout,
        total.survived,
        total.no_coverage,
        total.runtime_errors + total.compile_errors,
    );
    out
}

fn score_text(m: &Metrics) -> String {
    match m.mutation_score() {
        Some(score) => format!("{score:.2}%"),
        None => "n/a".to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("…{}", &s[s.len() - max + 1..]) }
}

fn original_snippet(source: &str, m: &crate::schema::MutantResultJson) -> Option<String> {
    if source.is_empty() {
        return None;
    }
    let line = source.lines().nth(m.location.start.line as usize - 1)?;
    Some(line.trim().to_string())
}
