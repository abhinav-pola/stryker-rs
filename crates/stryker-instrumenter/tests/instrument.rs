use camino::Utf8Path;
use stryker_instrumenter::{InstrumentOptions, instrument_file};

fn mutant_lines(result: &stryker_instrumenter::InstrumentedFile) -> String {
    result
        .mutants
        .iter()
        .map(|m| {
            format!(
                "{:>3} {:<22} {}:{}:{}  {} -> {}{}",
                m.id.0,
                m.mutator_name,
                m.file,
                m.location.start.line,
                m.location.start.column,
                m.original.replace('\n', "\\n"),
                m.replacement.replace('\n', "\\n"),
                m.ignored.as_deref().map(|r| format!("  [ignored: {r}]")).unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn ts_sample() {
    let source = r#"export function classify(n: number, opts?: { strict?: boolean }): string {
  const strict = opts?.strict ?? false;
  if (n < 0 && strict) {
    return "negative";
  }
  let total = 0;
  for (let i = 0; i < n; i++) {
    total += i;
  }
  const labels = ["a", "b"].filter((l) => l !== "b");
  const isBig = total > 10 ? true : !strict;
  return labels.some((l) => l.startsWith("a")) ? `big:${isBig}` : "small";
}
"#;
    let result = instrument_file(
        Utf8Path::new("src/classify.ts"),
        source,
        0,
        &InstrumentOptions::default(),
    )
    .unwrap();
    insta::assert_snapshot!("ts_sample_mutants", mutant_lines(&result));
    insta::assert_snapshot!("ts_sample_output", result.instrumented.as_deref().unwrap());
}

#[test]
fn tsx_sample() {
    let source = r#"export function Badge({ count, label }: { count: number; label?: string }) {
  const text = label ?? "items";
  return (
    <span className={count > 0 ? "full" : "empty"}>
      {count > 0 && <strong>{count}</strong>} {text.toUpperCase()}
    </span>
  );
}
"#;
    let result = instrument_file(
        Utf8Path::new("src/Badge.tsx"),
        source,
        100,
        &InstrumentOptions::default(),
    )
    .unwrap();
    insta::assert_snapshot!("tsx_sample_mutants", mutant_lines(&result));
    insta::assert_snapshot!("tsx_sample_output", result.instrumented.as_deref().unwrap());
}

#[test]
fn directives_and_exclusions() {
    let source = r#"export function f(a: number, b: number) {
  // Stryker disable next-line EqualityOperator: intentional
  const x = a === b;
  const y = a < b;
  return x || y;
}
"#;
    let result = instrument_file(
        Utf8Path::new("src/f.ts"),
        source,
        0,
        &InstrumentOptions {
            excluded_mutators: vec!["BlockStatement".into()],
            ..InstrumentOptions::default()
        },
    )
    .unwrap();
    insta::assert_snapshot!("directives_mutants", mutant_lines(&result));
    let ignored: Vec<_> = result.mutants.iter().filter(|m| m.ignored.is_some()).collect();
    // One from the disable-next-line directive...
    let directive_ignored: Vec<_> = ignored
        .iter()
        .filter(|m| m.ignored.as_deref() == Some("intentional"))
        .collect();
    assert_eq!(directive_ignored.len(), 1);
    assert_eq!(directive_ignored[0].mutator_name, "EqualityOperator");
    // ...plus the excluded BlockStatement mutants (reported, never placed).
    assert!(
        ignored
            .iter()
            .filter(|m| m.mutator_name == "BlockStatement")
            .all(|m| m.ignored.as_deref().unwrap().contains("excludedMutations"))
    );
    // Ignored mutants are not placed.
    let instrumented = result.instrumented.as_deref().unwrap();
    for m in &ignored {
        assert!(!instrumented.contains(&format!("stryMutAct_9fa48(\"{}\")", m.id.0)));
    }
}

#[test]
fn range_filter() {
    let source = "export const a = 1 + 2;\nexport const b = 3 + 4;\nexport const c = 5 + 6;\n";
    let result = instrument_file(
        Utf8Path::new("src/r.ts"),
        source,
        0,
        &InstrumentOptions {
            ranges: vec![stryker_core::mutate_pattern::MutationRange {
                start_line: 2,
                start_column: None,
                end_line: 2,
                end_column: None,
            }],
            ..InstrumentOptions::default()
        },
    )
    .unwrap();
    assert_eq!(result.mutants.len(), 1);
    assert_eq!(result.mutants[0].location.start.line, 2);
    assert_eq!(result.mutants[0].original, "3 + 4");
}

#[test]
fn no_mutants_means_untouched() {
    let source = "export type A = { x: string };\nexport interface B { y: number }\n";
    let result =
        instrument_file(Utf8Path::new("src/types.ts"), source, 0, &InstrumentOptions::default())
            .unwrap();
    assert!(result.mutants.is_empty());
    assert!(result.instrumented.is_none());
}
