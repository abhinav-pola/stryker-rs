/**
 * stryker-rs bun:test preload.
 *
 * Materialized into <tempDir>/stryker-preload.ts and passed to every
 * `bun test --preload` invocation by the bun runner.
 *
 * ## M0 probe findings (bun 1.3.14) — the constraints this design obeys
 *
 * - `expect.getState().currentTestName/testPath` are UNDEFINED inside preload
 *   hooks, and hook callbacks receive no arguments. There is no direct way to
 *   learn the current test's name at runtime.
 * - Therefore test identity uses ORDINAL CORRELATION: `beforeEach` fires
 *   exactly once per *executed* test, in junit document order, across all
 *   files in the run. Every non-executed test (skip / todo / `-t`-filtered)
 *   appears in the junit XML with a `<skipped>` child. So the n-th
 *   `beforeEach` firing corresponds to the n-th testcase without a
 *   `<skipped>` child in flattened junit document order. The Rust side joins
 *   ordinals to (file, describe-path, name) after the run.
 *   REQUIREMENT: never pass `--concurrent` (the runner controls argv).
 * - Root-level `afterAll` registered in a preload fires once at the very end
 *   of the run, even when tests fail — this is the only reliable flush point.
 *   `process.on("beforeExit"/"exit")` writes did NOT reliably produce output.
 * - junit `<failure>` elements carry a type but NO message text. Hit-limit
 *   detection therefore happens in Rust by comparing `hitCount` from the
 *   handoff file against the limit; failure messages come from stderr.
 * - junit `classname` is double-escaped and lists describes innermost-first;
 *   the Rust parser reconstructs describe paths from `<testsuite>` nesting
 *   instead.
 *
 * ## Env contract (set by the Rust runner)
 *
 * - STRYKER_NS: global namespace name (default "__stryker__").
 * - STRYKER_COVERAGE_FILE: if set, write the handoff JSON here in afterAll.
 * - __STRYKER_ACTIVE_MUTANT__ / __STRYKER_HIT_LIMIT__: read by the
 *   instrumentation header itself, not by this preload.
 *
 * ## Handoff file shape
 *
 * { "ordinals": <count of executed tests>,
 *   "perTest": { "<ordinal>": { "<mutantId>": hits } },
 *   "static": { "<mutantId>": hits },
 *   "hitCount": <number|null> }
 */
import { afterAll, afterEach, beforeEach } from "bun:test";
import { writeFileSync } from "node:fs";

type CoverageData = Record<string, number>;
interface StrykerNamespace {
  activeMutant?: string;
  currentTestId?: string;
  hitCount?: number;
  hitLimit?: number;
  mutantCoverage?: { static: CoverageData; perTest: Record<string, CoverageData> };
}

const NS = process.env.STRYKER_NS ?? "__stryker__";
const g = globalThis as unknown as Record<string, StrykerNamespace>;
const ns: StrykerNamespace = (g[NS] ??= {});

let ordinal = 0;

beforeEach(() => {
  ns.currentTestId = String(ordinal);
  ordinal += 1;
});

afterEach(() => {
  ns.currentTestId = undefined;
});

afterAll(() => {
  const file = process.env.STRYKER_COVERAGE_FILE;
  if (!file) return;
  const coverage = ns.mutantCoverage ?? { static: {}, perTest: {} };
  const payload = {
    ordinals: ordinal,
    perTest: coverage.perTest,
    static: coverage.static,
    hitCount: ns.hitCount ?? null,
  };
  writeFileSync(file, JSON.stringify(payload));
});
