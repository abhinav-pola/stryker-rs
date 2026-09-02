//! Assemble the instrumented file text from collected sites.
//!
//! Purely textual: the output is the original source with each placement
//! span rewritten to a mutant switch, recursively. Mutated arms contain RAW
//! original text with exactly one mutation applied (inner mutants stay
//! un-instrumented there — when an outer mutant is active, inner mutants are
//! inactive by definition, so their original behavior is what we want).

use oxc_span::Span;

use crate::collect::{Placement, Site};

/// A site whose candidates have been assigned global mutant ids and filtered
/// down to the ones that should actually be placed.
pub struct PlacedSite {
    pub placement: Placement,
    /// (mutant id, sub_span, replacement)
    pub arms: Vec<(u32, Span, String)>,
}

impl PlacedSite {
    fn span(&self) -> Span {
        self.placement.span()
    }
}

pub fn build_instrumented(source: &str, header: &str, mut sites: Vec<PlacedSite>) -> String {
    // Sort outer-first, then left-to-right: (start asc, end desc).
    sites.sort_by(|a, b| {
        a.span()
            .start
            .cmp(&b.span().start)
            .then(b.span().end.cmp(&a.span().end))
    });
    let all = sites;
    let top_level: Vec<usize> = top_level_in(&all, 0, source.len() as u32, None);
    let mut out = String::with_capacity(source.len() * 2 + header.len());
    // A shebang must stay on line 1; the header goes after it.
    let body_start = if source.starts_with("#!") {
        let end = source.find('\n').map_or(source.len(), |i| i + 1);
        out.push_str(&source[..end]);
        end as u32
    } else {
        0
    };
    out.push_str(header);
    render_range(source, &all, &top_level, body_start, source.len() as u32, &mut out);
    out
}

/// Indices of sites fully inside [start, end) that are not nested inside
/// another site in the same range (excluding `exclude`).
fn top_level_in(all: &[PlacedSite], start: u32, end: u32, exclude: Option<usize>) -> Vec<usize> {
    let mut result: Vec<usize> = Vec::new();
    for (i, site) in all.iter().enumerate() {
        if Some(i) == exclude {
            continue;
        }
        let s = site.span();
        if s.start < start || s.end > end {
            continue;
        }
        // Nested inside a site already selected? (sites are sorted outer-first)
        if let Some(&last) = result.last() {
            let l = all[last].span();
            if s.start >= l.start && s.end <= l.end {
                continue;
            }
        }
        result.push(i);
    }
    result
}

fn render_range(
    source: &str,
    all: &[PlacedSite],
    site_indices: &[usize],
    start: u32,
    end: u32,
    out: &mut String,
) {
    let mut cursor = start;
    for &i in site_indices {
        let site = &all[i];
        let span = site.span();
        out.push_str(&source[cursor as usize..span.start as usize]);
        render_site(source, all, i, out);
        cursor = span.end;
    }
    out.push_str(&source[cursor as usize..end as usize]);
}

fn render_site(source: &str, all: &[PlacedSite], index: usize, out: &mut String) {
    let site = &all[index];
    let span = site.span();
    let ids_list = site
        .arms
        .iter()
        .map(|(id, _, _)| format!("\"{id}\""))
        .collect::<Vec<_>>()
        .join(", ");

    match site.placement {
        Placement::Expression(_) => {
            // (stryMutAct_9fa48("1") ? arm1 : stryMutAct_9fa48("2") ? arm2 :
            //  (stryCov_9fa48("1", "2"), <instrumented original>))
            out.push('(');
            for (id, sub, replacement) in &site.arms {
                out.push_str(&format!("stryMutAct_9fa48(\"{id}\") ? "));
                out.push_str(&source[span.start as usize..sub.start as usize]);
                out.push_str(replacement);
                out.push_str(&source[sub.end as usize..span.end as usize]);
                out.push_str(" : ");
            }
            out.push_str(&format!("(stryCov_9fa48({ids_list}), "));
            let children = top_level_in(all, span.start, span.end, Some(index));
            render_range(source, all, &children, span.start, span.end, out);
            out.push_str("))");
        }
        Placement::BlockBody { block } => {
            // { if (stryMutAct_9fa48("1")) {} else { stryCov_9fa48("1");
            //   <instrumented original statements> } }
            debug_assert!(source[block.start as usize..].starts_with('{'));
            out.push_str("{ ");
            for (id, _, _) in &site.arms {
                out.push_str(&format!("if (stryMutAct_9fa48(\"{id}\")) {{}} else "));
            }
            out.push_str(&format!("{{ stryCov_9fa48({ids_list}); "));
            let inner_start = block.start + 1;
            let inner_end = block.end - 1;
            let children = top_level_in(all, inner_start, inner_end, Some(index));
            render_range(source, all, &children, inner_start, inner_end, out);
            out.push_str(" } }");
        }
        Placement::Statements { span: stmts } => {
            // Brace-less statement list (switch-case consequent):
            // if (stryMutAct_9fa48("1")) {} else { stryCov_9fa48("1"); ... }
            for (id, _, _) in &site.arms {
                out.push_str(&format!("if (stryMutAct_9fa48(\"{id}\")) {{}} else "));
            }
            out.push_str(&format!("{{ stryCov_9fa48({ids_list}); "));
            let children = top_level_in(all, stmts.start, stmts.end, Some(index));
            render_range(source, all, &children, stmts.start, stmts.end, out);
            out.push_str(" }");
        }
    }
}
