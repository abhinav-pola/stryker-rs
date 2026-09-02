# stryker-rs

Mutation testing for JavaScript and TypeScript, in Rust. A rewrite of
[stryker-js](https://github.com/stryker-mutator/stryker-js) built for large
TS monorepos, with a native **bun:test** runner as the flagship feature.

## Why

- **Native bun:test runner with per-test coverage.** stryker-js has no bun
  runner, so bun projects fall back to `testRunner: 'command'` with coverage
  off — every mutant re-runs the whole test command. stryker-rs runs a dry
  run once, learns which tests cover which mutants, and re-runs only those.
- **In-place by default, crash-safe.** Only files that contain mutants are
  rewritten (mutant schemata); originals are backed up with a manifest that
  is fsync'd before the first write. Restore happens on completion, panic,
  SIGINT, and via `stryker restore` after a hard kill.
- **No `/bin/sh`.** Test commands are split with shell-words and exec'd as
  argv — paths with `(` `)` (Next.js route groups) can't break quoting.
- **Byte-exact instrumentation.** Mutants are placed by textual span
  splicing over the oxc parse — unmutated code keeps its exact bytes,
  comments included. No codegen round-trip risk.
- **Real incremental mode.** The incremental file is a full
  mutation-testing report; reuse is content-keyed with line-diff location
  remapping and per-test-file hash invalidation.
- **Standard report schema.** JSON output validates against
  mutation-testing-report-schema v2; the HTML report embeds
  mutation-testing-elements (pinned 3.9.0) in a single self-contained file.

## Differential validation

`scripts/differential.mjs` runs stryker-js and stryker-rs on identical
targets and diffs the normalized reports (mutant identity = file + mutator
+ location; ids/durations/formatting excluded). On a subtree of a large production
TS monorepo — 77 source files, 3,863 mutants, judged by a 987-test
production suite — the result is **complete semantic identity**:
0 missing mutants, 0 extra mutants, 0 status mismatches (Timeouts
included). Known intentional deltas vs stryker-js: test files matched by
`mutate` globs are never mutated, and the Regex mutator is a faithful
weapon-regex level-1 port (higher levels not implemented).

## Benchmark

One 287-line file from a production monorepo (214 mutants, 13 covering
tests), hyperfine, 3 runs,
Apple Silicon (release build):

| Tool | Mean | Speedup |
|---|---|---|
| stryker-js 9.6 (command runner, coverage off) | 18.4 s | 1× |
| stryker-rs (command runner, same semantics) | 13.3 s | 1.4× |
| stryker-rs (native bun runner, perTest coverage) | 9.9 s | **1.86×** |
| stryker-rs (bun runner, warm incremental rerun) | 0.46 s | **40×** |

The per-test advantage grows with the number of test files in scope; the
table above is the worst case (a single covering test file). On realistic
CI-shaped scopes from the same monorepo (verdicts identical across tools in
every run):

| Scenario | stryker-js | stryker-rs command | stryker-rs bun |
|---|---:|---:|---:|
| 24 files (CI cap), full 987-test suite | 286.8 s | 274.4 s | **57.5 s (5.0×)** |
| 8 files (typical PR), narrowed command | 8.1 s | 7.5 s | **4.1 s (2.0×)** |

The drop-in command runner roughly matches stryker-js (both are bounded by
the same test-command re-run per mutant); the native runner's per-test
coverage is where the speedup lives, and it narrows per-mutant
automatically — no harness narrowing heuristics required.

## Usage

```sh
stryker run [--config stryker.config.json] [--force-dirty] [--dry-run-only]
stryker restore [--temp-dir .stryker-tmp]   # recover after a hard crash
stryker debug files [--config ...]          # list resolved mutate targets
```

The config path may be positional (`stryker run stryker.config.json`,
stryker-js style). Config is stryker-js-compatible JSON/JSONC (executable
`.mjs`/`.js` configs are evaluated via `bun`/`node`). Supported keys include
`mutate` (globs, `!` negation, `file.ts:10-20` line ranges), `testRunner`
(`command` | `bun` | `vitest` | `composite`), `commandRunner.{command,
shell}` (env-var prefixes like `FOO=1 cmd` are parsed; commands with real
shell syntax run via `bash -c`, never `/bin/sh`), `bunRunner.{args,
testFiles, env}`, `vitestRunner.configFile`, `coverageAnalysis`,
`concurrency`, `timeoutMS`, `timeoutFactor`, `ignorePatterns` (gitignore
semantics, inverted allowlists work), `inPlace`, `tempDirName`,
`cleanTempDir`, `reporters` (`clear-text`, `json`, `html`, `progress`),
`jsonReporter.fileName`, `htmlReporter.fileName`, `thresholds`
(`break` → exit 1), `incremental` / `incrementalFile` / `force`,
`mutator.excludedMutations` (reported as `Ignored`, like stryker-js),
`disableTypeChecks`, `dryRunOnly`.

`testRunner: "composite"` runs bun AND vitest suites for the same targets
(mixed-runtime repos, e.g. bun unit tests + `@cloudflare/vitest-pool-workers`
`.cfw.test.ts`); scope each runner's test files so they don't overlap. The
vitest runner is validated against pool-workers/workerd: per-run state is
baked into the setup file, so no Node APIs are needed inside workers.

Inline suppression: `// Stryker disable next-line EqualityOperator: reason`,
`// Stryker disable all` / `// Stryker restore all`.

Incremental-mode fidelity depends on the runner tier: the bun/vitest
runners track per-test-file content hashes, so killed results are reused
only while their killing tests are unchanged. The command runner can only
fingerprint the command string — if your test *content* can change without
the command changing, keep an external fingerprint (or use `force`) until
you're on a native runner.

## Mutators

ArithmeticOperator, ArrayDeclaration, ArrowFunction, AssignmentOperator,
BlockStatement, BooleanLiteral, ConditionalExpression, EqualityOperator,
LogicalOperator, MethodExpression, ObjectLiteral, OptionalChaining, Regex
(anchor/char-class subset), StringLiteral, UnaryOperator, UpdateOperator.

## How it works

1. **Read**: walk the project (`ignore` crate; file-level ignorePatterns
   matching so inverted allowlists re-include through excluded parents).
2. **Instrument**: parse with oxc (pinned lockstep), collect mutants in one
   read-only visitor pass, then splice all mutants into one instrumented
   copy per file — `stryMutAct_9fa48("3") ? mutated : (stryCov_9fa48("3"),
   original)` — activated via `__STRYKER_ACTIVE_MUTANT__`. A hit counter
   inside the active check throws past `100 × dry-run hits` (infinite-loop
   detection → Timeout).
3. **Dry run**: per-test coverage lands in `globalThis.__stryker__`
   buckets. The bun runner correlates tests by ordinal (bun 1.3 has no
   `expect.getState()` in preloads; the n-th `beforeEach` is the n-th
   non-skipped junit testcase — probed and documented in
   `js/bun-preload.ts`). The vitest runner keeps a long-lived
   `createVitest()` shim per worker speaking NDJSON over stdio.
4. **Plan & execute**: covered mutants run only their covering tests
   (cheapest first); static mutants (hit at module scope) run the full
   suite last; every child process gets its own process group and timeouts
   kill the whole tree.
5. **Report**: clear-text score table, schema-validated JSON, single-file
   HTML, incremental report.

## Development

```sh
cargo test                 # unit + snapshot + e2e (e2e needs bun)
cargo build --release
cargo run --release -p stryker-instrumenter --bin roundtrip -- <corpus>  # oxc upgrade gate
```

Layout: `crates/stryker-core` (config, project, planner, sandbox),
`stryker-instrumenter` (oxc collect + textual schemata),
`stryker-runners` (command/bun/vitest + junit + NDJSON proto),
`stryker-reporters` (schema, clear-text, html), `stryker-incremental`,
`stryker-cli`. JS sidecars in `js/` are `include_str!`-ed into the binary.
