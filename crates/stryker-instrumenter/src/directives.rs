//! `// Stryker disable` comment directives.
//!
//! Supported forms (same grammar as stryker-js):
//!   // Stryker disable all
//!   // Stryker disable next-line all
//!   // Stryker disable next-line ConditionalExpression,EqualityOperator: reason
//!   // Stryker restore all
//!   // Stryker restore EqualityOperator

use oxc_ast::Comment;

#[derive(Debug, Clone, PartialEq)]
enum Scope {
    All,
    Mutators(Vec<String>),
}

impl Scope {
    fn applies_to(&self, mutator: &str) -> bool {
        match self {
            Scope::All => true,
            Scope::Mutators(list) => list.iter().any(|m| m.eq_ignore_ascii_case(mutator)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Disable,
    DisableNextLine,
    Restore,
}

#[derive(Debug, Clone)]
struct Directive {
    line: u32,
    kind: Kind,
    scope: Scope,
    reason: Option<String>,
}

#[derive(Debug, Default)]
pub struct DirectiveIndex {
    /// Sorted by line.
    directives: Vec<Directive>,
}

const DEFAULT_REASON: &str = "Ignored by a \"Stryker disable\" comment";

impl DirectiveIndex {
    pub fn from_comments(
        comments: &[Comment],
        source: &str,
        line_of: impl Fn(u32) -> u32,
    ) -> Self {
        let mut directives = Vec::new();
        for comment in comments {
            let text = comment.content_span().source_text(source).trim();
            let Some(rest) = text.strip_prefix("Stryker ") else {
                continue;
            };
            let line = line_of(comment.span.start);
            if let Some(parsed) = parse_directive(rest.trim(), line) {
                directives.push(parsed);
            }
        }
        directives.sort_by_key(|d| d.line);
        Self { directives }
    }

    /// Is `mutator` disabled for a mutant on `line` (1-based)? Returns the
    /// reason if so.
    pub fn disabled_reason(&self, line: u32, mutator: &str) -> Option<&str> {
        // next-line directives win and are not cancelled by later restores.
        for d in &self.directives {
            if d.kind == Kind::DisableNextLine && d.line + 1 == line && d.scope.applies_to(mutator)
            {
                return Some(d.reason.as_deref().unwrap_or(DEFAULT_REASON));
            }
        }
        let mut state: Option<&str> = None;
        for d in &self.directives {
            if d.line >= line {
                break;
            }
            if !d.scope.applies_to(mutator) {
                continue;
            }
            match d.kind {
                Kind::Disable => state = Some(d.reason.as_deref().unwrap_or(DEFAULT_REASON)),
                Kind::Restore => state = None,
                Kind::DisableNextLine => {}
            }
        }
        state
    }

    pub fn is_empty(&self) -> bool {
        self.directives.is_empty()
    }
}

fn parse_directive(rest: &str, line: u32) -> Option<Directive> {
    let (kind, rest) = if let Some(r) = rest.strip_prefix("disable next-line") {
        (Kind::DisableNextLine, r)
    } else if let Some(r) = rest.strip_prefix("disable") {
        (Kind::Disable, r)
    } else if let Some(r) = rest.strip_prefix("restore") {
        (Kind::Restore, r)
    } else {
        return None;
    };
    let rest = rest.trim();
    let (scope_text, reason) = match rest.split_once(':') {
        Some((s, r)) => (s.trim(), Some(r.trim().to_string())),
        None => (rest, None),
    };
    let scope = if scope_text.is_empty() || scope_text == "all" {
        Scope::All
    } else {
        Scope::Mutators(
            scope_text
                .split(',')
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect(),
        )
    };
    Some(Directive { line, kind, scope, reason })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(entries: &[(u32, &str)]) -> DirectiveIndex {
        let mut directives = Vec::new();
        for (line, text) in entries {
            let rest = text.trim().strip_prefix("Stryker ").unwrap();
            directives.push(parse_directive(rest.trim(), *line).unwrap());
        }
        directives.sort_by_key(|d| d.line);
        DirectiveIndex { directives }
    }

    #[test]
    fn disable_all_until_restore() {
        let idx = index(&[(5, "Stryker disable all"), (10, "Stryker restore all")]);
        assert!(idx.disabled_reason(6, "EqualityOperator").is_some());
        assert!(idx.disabled_reason(5, "EqualityOperator").is_none()); // not its own line
        assert!(idx.disabled_reason(11, "EqualityOperator").is_none());
    }

    #[test]
    fn next_line_specific_mutator_with_reason() {
        let idx = index(&[(3, "Stryker disable next-line EqualityOperator,LogicalOperator: intended")]);
        assert_eq!(idx.disabled_reason(4, "EqualityOperator"), Some("intended"));
        assert_eq!(idx.disabled_reason(4, "LogicalOperator"), Some("intended"));
        assert!(idx.disabled_reason(4, "BooleanLiteral").is_none());
        assert!(idx.disabled_reason(5, "EqualityOperator").is_none());
    }
}
