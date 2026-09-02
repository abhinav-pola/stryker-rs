use camino::{Utf8Path, Utf8PathBuf};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;

use crate::config::{StrykerConfig, TEST_FILE_MARKERS};
use crate::mutate_pattern::{MutatePattern, MutationRange};

/// Patterns always excluded from the project walk, mirroring stryker-js's
/// ALWAYS_IGNORE plus our temp dir.
const ALWAYS_IGNORE: &[&str] = &[
    "node_modules",
    ".git",
    "*.tsbuildinfo",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
];

#[derive(Debug, Clone)]
pub struct MutateTarget {
    /// Relative to project root.
    pub path: Utf8PathBuf,
    /// Restrict mutation to these ranges (empty = whole file).
    pub ranges: Vec<MutationRange>,
    /// Named literally in `mutate` — must appear in the report even with
    /// zero mutants (downstream CI parsers treat absence as failure).
    pub requested_literally: bool,
}

#[derive(Debug)]
pub struct Project {
    pub root: Utf8PathBuf,
    /// All non-ignored files, sorted (deterministic mutant ids depend on it).
    pub files: Vec<Utf8PathBuf>,
    /// Files to mutate, sorted.
    pub targets: Vec<MutateTarget>,
}

pub fn read_project(root: &Utf8Path, config: &StrykerConfig) -> anyhow::Result<Project> {
    let patterns = config.parsed_mutate()?;
    let files = walk_files(root, config)?;
    let targets = resolve_targets(&files, &patterns)?;
    Ok(Project { root: root.to_owned(), files, targets })
}

/// Walk the project honoring `ignorePatterns` (gitignore-line semantics:
/// patterns exclude, `!` re-includes) and ALWAYS_IGNORE. Does not respect
/// .gitignore — stryker-js doesn't either.
///
/// Matching is FILE-LEVEL (last matching pattern wins), like stryker-js's
/// globby filtering — deliberately NOT git's directory-pruning semantics,
/// because an inverted allowlist (`'**'` then `!pkg/**`)
/// must re-include files whose parent directory the `**` excluded. Only
/// ALWAYS_IGNORE and the temp dir prune the walk itself.
fn walk_files(root: &Utf8Path, config: &StrykerConfig) -> anyhow::Result<Vec<Utf8PathBuf>> {
    let mut ignore_builder = ignore::gitignore::GitignoreBuilder::new(root.as_std_path());
    for pattern in &config.ignore_patterns {
        ignore_builder.add_line(None, pattern)?;
    }
    let ignore_matcher = ignore_builder.build()?;

    // Prune only the hard-ignored dirs during the walk (perf, not semantics).
    let mut overrides = OverrideBuilder::new(root.as_std_path());
    for pattern in ALWAYS_IGNORE {
        overrides.add(&format!("!{pattern}"))?;
    }
    overrides.add(&format!("!{}", config.temp_dir_name))?;

    let walker = WalkBuilder::new(root.as_std_path())
        .standard_filters(false)
        .hidden(false)
        .overrides(overrides.build()?)
        .follow_links(false)
        .build();

    let mut files = Vec::new();
    for entry in walker {
        let entry = entry?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(path) = Utf8PathBuf::from_path_buf(entry.into_path()) else {
            continue; // non-UTF8 paths are not mutation targets
        };
        let Ok(rel) = path.strip_prefix(root).map(|r| r.to_owned()) else {
            continue;
        };
        if ignore_matcher.matched(rel.as_std_path(), false).is_ignore() {
            continue;
        }
        files.push(rel);
    }
    files.sort();
    Ok(files)
}

fn resolve_targets(
    files: &[Utf8PathBuf],
    patterns: &[MutatePattern],
) -> anyhow::Result<Vec<MutateTarget>> {
    let positive: Vec<&MutatePattern> = patterns.iter().filter(|p| !p.negated).collect();
    let negative_set = build_globset(patterns.iter().filter(|p| p.negated))?;
    let positive_set = build_globset(positive.iter().copied())?;

    let mut targets: Vec<MutateTarget> = Vec::new();
    for file in files {
        let matched: Vec<usize> = positive_set.matches(file.as_str());
        if matched.is_empty() || negative_set.is_match(file.as_str()) {
            continue;
        }
        if is_test_file(file) || is_declaration_file(file) {
            continue;
        }
        let mut ranges = Vec::new();
        let mut whole_file = false;
        let mut requested_literally = false;
        for idx in matched {
            let pattern = positive[idx];
            requested_literally |= pattern.is_literal_path();
            match pattern.range {
                Some(range) => ranges.push(range),
                None => whole_file = true,
            }
        }
        if whole_file {
            ranges.clear();
        }
        targets.push(MutateTarget { path: file.clone(), ranges, requested_literally });
    }
    Ok(targets)
}

fn build_globset<'a>(
    patterns: impl Iterator<Item = &'a MutatePattern>,
) -> anyhow::Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(&pattern.glob)
            .literal_separator(true)
            .backslash_escape(true)
            .empty_alternates(true)
            .build()?;
        builder.add(glob);
    }
    Ok(builder.build()?)
}

fn is_test_file(path: &Utf8Path) -> bool {
    let name = path.file_name().unwrap_or_default();
    TEST_FILE_MARKERS.iter().any(|marker| name.contains(marker))
        || path.components().any(|c| {
            matches!(c.as_str(), "__tests__" | "__mocks__" | "test" | "tests" | "e2e")
        })
}

fn is_declaration_file(path: &Utf8Path) -> bool {
    let name = path.file_name().unwrap_or_default();
    name.ends_with(".d.ts") || name.ends_with(".d.mts") || name.ends_with(".d.cts")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StrykerConfig;

    fn write(root: &std::path::Path, rel: &str, content: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn inverted_allowlist_ignore_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "package.json", "{}");
        write(root, "secret/data.ts", "x");
        write(root, "pkg/src/a.ts", "x");
        write(root, "pkg/src/a.test.ts", "x");
        write(root, "node_modules/dep/i.ts", "x");

        let config = StrykerConfig {
            ignore_patterns: vec![
                "**".into(),
                "!package.json".into(),
                "!pkg/**".into(),
            ],
            mutate: vec!["pkg/**/*.ts".into()],
            ..StrykerConfig::default()
        };
        let root8 = Utf8Path::from_path(root).unwrap();
        let project = read_project(root8, &config).unwrap();
        let files: Vec<&str> = project.files.iter().map(|f| f.as_str()).collect();
        assert_eq!(files, vec!["package.json", "pkg/src/a.test.ts", "pkg/src/a.ts"]);
        let targets: Vec<&str> = project.targets.iter().map(|t| t.path.as_str()).collect();
        assert_eq!(targets, vec!["pkg/src/a.ts"]); // test file excluded
    }

    #[test]
    fn route_group_literal_and_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "app/(user)/page.tsx", "x");
        write(root, "lib/util.ts", "x");

        let config = StrykerConfig {
            mutate: vec![
                "app/[(]user[)]/page.tsx".into(),
                "lib/util.ts:10-20".into(),
            ],
            ..StrykerConfig::default()
        };
        let root8 = Utf8Path::from_path(root).unwrap();
        let project = read_project(root8, &config).unwrap();
        let by_path: std::collections::HashMap<_, _> =
            project.targets.iter().map(|t| (t.path.as_str(), t)).collect();
        let page = by_path["app/(user)/page.tsx"];
        assert!(page.requested_literally);
        assert!(page.ranges.is_empty());
        let util = by_path["lib/util.ts"];
        assert!(util.requested_literally);
        assert_eq!(util.ranges.len(), 1);
        assert!(util.ranges[0].contains_line(15));
    }
}
