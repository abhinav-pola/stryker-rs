//! M0 spike (a): parse → codegen → reparse a corpus of real-world TS/TSX and
//! report the failure rate. Read-only over the corpus.
//!
//! Usage: cargo run --release -p stryker-instrumenter --bin roundtrip -- <corpus-root>

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use ignore::WalkBuilder;
use oxc_allocator::Allocator;
use oxc_codegen::Codegen;
use oxc_parser::{ParseOptions, Parser};
use oxc_span::SourceType;
use rayon::prelude::*;

const EXTS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

fn main() {
    let root = std::env::args().nth(1).expect("usage: roundtrip <corpus-root>");

    let mut files: Vec<PathBuf> = Vec::new();
    for entry in WalkBuilder::new(&root).hidden(false).build() {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let name = path.file_name().unwrap().to_string_lossy();
        if EXTS.contains(&ext) && !name.ends_with(".d.ts") && !name.ends_with(".d.mts") {
            files.push(path.to_path_buf());
        }
    }
    println!("corpus: {} files", files.len());

    let parse_fail = AtomicUsize::new(0);
    let reparse_fail = AtomicUsize::new(0);
    let ok = AtomicUsize::new(0);
    let skipped = AtomicUsize::new(0);

    files.par_iter().for_each(|path| {
        let Ok(source) = std::fs::read_to_string(path) else {
            skipped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let Ok(source_type) = SourceType::from_path(path) else {
            skipped.fetch_add(1, Ordering::Relaxed);
            return;
        };

        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, &source, source_type)
            .with_options(ParseOptions {
                parse_regular_expression: true,
                ..ParseOptions::default()
            })
            .parse();
        if parsed.panicked || !parsed.diagnostics.is_empty() {
            parse_fail.fetch_add(1, Ordering::Relaxed);
            eprintln!("PARSE-FAIL {}", path.display());
            return;
        }

        let printed = Codegen::new().build(&parsed.program).code;

        let allocator2 = Allocator::default();
        let reparsed = Parser::new(&allocator2, &printed, source_type)
            .with_options(ParseOptions {
                parse_regular_expression: true,
                ..ParseOptions::default()
            })
            .parse();
        if reparsed.panicked || !reparsed.diagnostics.is_empty() {
            reparse_fail.fetch_add(1, Ordering::Relaxed);
            eprintln!("REPARSE-FAIL {}", path.display());
            for e in reparsed.diagnostics.iter().take(2) {
                eprintln!("    {e}");
            }
            return;
        }
        ok.fetch_add(1, Ordering::Relaxed);
    });

    let ok = ok.load(Ordering::Relaxed);
    let parse_fail = parse_fail.load(Ordering::Relaxed);
    let reparse_fail = reparse_fail.load(Ordering::Relaxed);
    let skipped = skipped.load(Ordering::Relaxed);
    let attempted = ok + reparse_fail;
    println!("ok:            {ok}");
    println!("parse-fail:    {parse_fail} (excluded from rate: corpus files we can't parse won't be mutated)");
    println!("reparse-fail:  {reparse_fail}");
    println!("skipped (io):  {skipped}");
    if attempted > 0 {
        println!(
            "round-trip success: {:.4}%",
            ok as f64 / attempted as f64 * 100.0
        );
    }
}
