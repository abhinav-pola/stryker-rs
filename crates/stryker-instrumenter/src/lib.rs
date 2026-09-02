pub mod build;
pub mod collect;
pub mod directives;
pub mod header;
pub mod line_index;

pub use header::HEADER_MARKER;
pub use line_index::LineIndex;

use camino::Utf8Path;
use oxc_allocator::Allocator;
use oxc_ast_visit::Visit;
use oxc_parser::{ParseOptions, Parser};
use oxc_span::SourceType;
use stryker_core::mutate_pattern::MutationRange;
use stryker_core::{Location, Mutant, MutantId};

use build::PlacedSite;
use collect::Collector;
use directives::DirectiveIndex;

#[derive(Debug, Clone, Default)]
pub struct InstrumentOptions {
    pub excluded_mutators: Vec<String>,
    /// Restrict mutants to these line ranges (empty = whole file).
    pub ranges: Vec<MutationRange>,
    pub disable_type_checks: bool,
    /// Global namespace property, default `__stryker__`.
    pub namespace: Option<String>,
}

#[derive(Debug)]
pub struct InstrumentedFile {
    pub mutants: Vec<Mutant>,
    /// None when the file has no placed mutants (leave it untouched).
    pub instrumented: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum InstrumentError {
    #[error("unsupported file type: {0}")]
    UnsupportedFileType(String),
    #[error("parse error in {path}: {message}")]
    Parse { path: String, message: String },
}

/// Collapse newline+indent runs to a single space (display normalization
/// for multi-line replacement text).
fn collapse_whitespace(text: &str) -> String {
    if !text.contains('\n') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut in_ws_run = false;
    for c in text.chars() {
        if c == '\n' || (in_ws_run && c.is_whitespace()) {
            in_ws_run = true;
            continue;
        }
        if in_ws_run {
            out.push(' ');
            in_ws_run = false;
        }
        out.push(c);
    }
    out
}

/// Instrument one file. `rel_path` is project-root-relative and is what ends
/// up in the report; mutant ids are assigned sequentially from `id_start`.
pub fn instrument_file(
    rel_path: &Utf8Path,
    source: &str,
    id_start: u32,
    options: &InstrumentOptions,
) -> Result<InstrumentedFile, InstrumentError> {
    let source_type = SourceType::from_path(rel_path.as_std_path())
        .map_err(|_| InstrumentError::UnsupportedFileType(rel_path.to_string()))?;

    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type)
        .with_options(ParseOptions {
            parse_regular_expression: true,
            ..ParseOptions::default()
        })
        .parse();
    if parsed.panicked || !parsed.diagnostics.is_empty() {
        let message = parsed
            .diagnostics
            .first()
            .map(|d| d.to_string())
            .unwrap_or_else(|| "parser panicked".to_string());
        return Err(InstrumentError::Parse { path: rel_path.to_string(), message });
    }

    let line_index = LineIndex::new(source);
    let directives = DirectiveIndex::from_comments(&parsed.program.comments, source, |offset| {
        line_index.position(source, offset).line
    });

    let mut collector = Collector::new(source, &directives, &options.excluded_mutators, |offset| {
        line_index.position(source, offset).line
    });
    collector.visit_program(&parsed.program);
    let mut sites = collector.sites;

    // Deterministic order: by placement span, then collection order within.
    sites.sort_by_key(|s| (s.placement.span().start, s.placement.span().end));

    let mut mutants: Vec<Mutant> = Vec::new();
    let mut placed_sites: Vec<PlacedSite> = Vec::new();
    let mut next_id = id_start;

    for site in sites {
        let mut arms: Vec<(u32, oxc_span::Span, String)> = Vec::new();
        for candidate in site.candidates {
            let start = line_index.position(source, candidate.sub_span.start);
            if !options.ranges.is_empty()
                && !options.ranges.iter().any(|r| r.contains_line(start.line))
            {
                continue; // outside the requested mutation range: not generated at all
            }
            let end = line_index.position(source, candidate.sub_span.end);
            let id = next_id;
            next_id += 1;
            let original =
                source[candidate.sub_span.start as usize..candidate.sub_span.end as usize].to_string();
            if candidate.ignored.is_none() {
                arms.push((id, candidate.sub_span, candidate.replacement.clone()));
            }
            mutants.push(Mutant {
                id: MutantId(id),
                file: rel_path.to_owned(),
                span: (candidate.sub_span.start, candidate.sub_span.end),
                location: Location { start, end },
                mutator_name: candidate.mutator,
                // Reported replacement is whitespace-collapsed for display
                // (stryker-js prints normalized codegen); the spliced arm
                // keeps the raw text.
                replacement: collapse_whitespace(&candidate.replacement),
                original,
                ignored: candidate.ignored,
            });
        }
        if !arms.is_empty() {
            placed_sites.push(PlacedSite { placement: site.placement, arms });
        }
    }

    let instrumented = if placed_sites.is_empty() {
        None
    } else {
        let namespace = options.namespace.as_deref().unwrap_or("__stryker__");
        let header_text = header::header(namespace, options.disable_type_checks);
        Some(build::build_instrumented(source, &header_text, placed_sites))
    };

    Ok(InstrumentedFile { mutants, instrumented })
}
