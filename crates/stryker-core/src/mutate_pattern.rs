use std::sync::LazyLock;

use regex::Regex;

/// One entry of the `mutate` config array.
///
/// Supports stryker-js's mutation-range suffixes on top of a glob or literal
/// path: `src/app.ts:1-11` (whole lines) and `src/app.ts:5:4-6:4`
/// (line:column, columns 0-based per stryker-js docs). Negated entries start
/// with `!`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutatePattern {
    pub negated: bool,
    /// Glob (or literal path) with any range suffix stripped.
    pub glob: String,
    pub range: Option<MutationRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutationRange {
    pub start_line: u32,
    pub start_column: Option<u32>,
    pub end_line: u32,
    pub end_column: Option<u32>,
}

impl MutationRange {
    /// Does a mutant starting at `line` (1-based) fall inside this range?
    /// Line-only ranges are inclusive on both ends, matching stryker-js.
    pub fn contains_line(&self, line: u32) -> bool {
        line >= self.start_line && line <= self.end_line
    }
}

// Mirrors stryker-js MUTATION_RANGE_REGEX: `path:5:4-6:4` or `path:5-6`.
static RANGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?):(\d+)(?::(\d+))?-(\d+)(?::(\d+))?$").unwrap());

impl MutatePattern {
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        let (negated, rest) = match raw.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, raw),
        };
        match RANGE_RE.captures(rest) {
            Some(caps) => {
                let glob = caps[1].to_string();
                let start_line: u32 = caps[2].parse()?;
                let start_column = caps.get(3).map(|m| m.as_str().parse()).transpose()?;
                let end_line: u32 = caps[4].parse()?;
                let end_column = caps.get(5).map(|m| m.as_str().parse()).transpose()?;
                if negated {
                    anyhow::bail!("negated mutate pattern {raw:?} cannot carry a mutation range");
                }
                if end_line < start_line {
                    anyhow::bail!("mutate range in {raw:?} ends before it starts");
                }
                Ok(Self {
                    negated,
                    glob,
                    range: Some(MutationRange { start_line, start_column, end_line, end_column }),
                })
            }
            None => Ok(Self { negated, glob: rest.to_string(), range: None }),
        }
    }

    /// True when the pattern names a literal file rather than a glob. Literal
    /// entries must appear in the report even with zero mutants.
    pub fn is_literal_path(&self) -> bool {
        !self.glob.contains(['*', '?', '{', '[']) || self.is_bracket_escaped_literal()
    }

    /// Some config generators escape glob metachars as `[c]` (e.g. `[(]` for
    /// Next.js route groups). A pattern whose only bracket use is single-char
    /// classes is still a literal path.
    fn is_bracket_escaped_literal(&self) -> bool {
        if self.glob.contains(['*', '?', '{']) {
            return false;
        }
        // Every '[' must open a exactly-one-char class "[c]".
        let bytes = self.glob.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'[' {
                if i + 2 >= bytes.len() || bytes[i + 2] != b']' {
                    return false;
                }
                i += 3;
            } else {
                i += 1;
            }
        }
        true
    }

    /// Resolve a bracket-escaped literal back to the plain path (`[(]` → `(`).
    pub fn literal_path(&self) -> Option<String> {
        if !self.is_literal_path() {
            return None;
        }
        let mut out = String::with_capacity(self.glob.len());
        let bytes = self.glob.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'[' && i + 2 < bytes.len() && bytes[i + 2] == b']' {
                out.push(bytes[i + 1] as char);
                i += 3;
            } else {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_glob() {
        let p = MutatePattern::parse("src/**/*.ts").unwrap();
        assert!(!p.negated);
        assert_eq!(p.glob, "src/**/*.ts");
        assert!(p.range.is_none());
        assert!(!p.is_literal_path());
    }

    #[test]
    fn negated() {
        let p = MutatePattern::parse("!src/**/*.spec.ts").unwrap();
        assert!(p.negated);
        assert_eq!(p.glob, "src/**/*.spec.ts");
    }

    #[test]
    fn line_range() {
        let p = MutatePattern::parse("packages/core/index.ts:100-200").unwrap();
        let r = p.range.unwrap();
        assert_eq!((r.start_line, r.end_line), (100, 200));
        assert_eq!(r.start_column, None);
        assert!(r.contains_line(100));
        assert!(r.contains_line(200));
        assert!(!r.contains_line(201));
        assert!(p.is_literal_path());
    }

    #[test]
    fn full_range() {
        let p = MutatePattern::parse("src/app.js:5:4-6:4").unwrap();
        let r = p.range.unwrap();
        assert_eq!((r.start_line, r.start_column, r.end_line, r.end_column), (5, Some(4), 6, Some(4)));
        assert_eq!(p.glob, "src/app.js");
    }

    #[test]
    fn bracket_escaped_route_group_is_literal() {
        let p = MutatePattern::parse("apps/web/app/[(]user[)]/page.tsx").unwrap();
        assert!(p.is_literal_path());
        assert_eq!(p.literal_path().unwrap(), "apps/web/app/(user)/page.tsx");
    }

    #[test]
    fn colon_in_range_only_at_end() {
        // A file literally named with a colon-digit suffix is indistinguishable
        // from a range; stryker-js has the same ambiguity. Document via test.
        let p = MutatePattern::parse("src/a.ts:1-2").unwrap();
        assert!(p.range.is_some());
    }
}
