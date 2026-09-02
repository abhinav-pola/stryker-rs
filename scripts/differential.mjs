// Differential harness: run stryker-js and stryker-rs on the SAME targets
// with the SAME config semantics, normalize both JSON reports, and diff.
//
// Neither tool is random, so "same seed" == same inputs; runs are SERIAL
// because both tools mutate the working tree in place.
//
// Buckets reported:
//   MISSING  — mutant stryker-js has, stryker-rs lacks (by file/mutator/location)
//   EXTRA    — mutant stryker-rs has, stryker-js lacks
//   STATUS   — same mutant, different status
//   UNSTABLE — status differs but one side is Timeout (wall-clock sensitive)
//
// Usage:
//   node differential.mjs --cwd <dir> --command "bun test helpers" \
//     --mutate "helpers/**/*.ts" [--mutate ...] \
//     [--js-invoke "node --import <alias> <path>/.bin/stryker"] \
//     [--rs-bin <path>] [--excluded StringLiteral,ObjectLiteral] \
//     [--concurrency 8] [--timeout 30000] [--keep-reports <dir>]

import { execSync, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync, cpSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// ---------- args ----------
const args = process.argv.slice(2);
function flag(name, fallback = undefined) {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
}
function flagAll(name) {
  const out = [];
  for (let i = 0; i < args.length - 1; i++) {
    if (args[i] === `--${name}`) out.push(args[i + 1]);
  }
  return out;
}

const cwd = flag("cwd");
const command = flag("command");
const mutate = flagAll("mutate");
if (!cwd || !command || mutate.length === 0) {
  console.error("required: --cwd --command --mutate (repeatable)");
  process.exit(2);
}
const jsInvoke = flag("js-invoke", "npx stryker");
const rsBin = flag("rs-bin", join(process.env.HOME, "stryker-rs/target/release/stryker"));
const excluded = (flag("excluded", "") || "").split(",").filter(Boolean);
const concurrency = Number(flag("concurrency", "8"));
const timeoutMS = Number(flag("timeout", "30000"));
const keepReports = flag("keep-reports");

const work = mkdtempSync(join(tmpdir(), "stryker-diff-"));

// ---------- configs ----------
const shared = {
  testRunner: "command",
  commandRunner: { command },
  coverageAnalysis: "off",
  mutate,
  mutator: { excludedMutations: excluded },
  concurrency,
  timeoutMS,
  inPlace: true,
  cleanTempDir: true,
  reporters: ["json"],
};
const jsConfig = {
  ...shared,
  tempDirName: ".stryker-tmp-diffjs",
  // stryker-js reads the whole project unless told otherwise; restrict it.
  ignorePatterns: ["**", "!package.json", "!bunfig.toml", "!tsconfig.json", ...mutate.map((m) => `!${m}`)],
  jsonReporter: { fileName: join(work, "js-report.json") },
};
const rsConfig = {
  ...shared,
  tempDirName: ".stryker-tmp-diffrs",
  jsonReporter: { fileName: join(work, "rs-report.json") },
};
writeFileSync(join(work, "js.json"), JSON.stringify(jsConfig, null, 2));
writeFileSync(join(work, "rs.json"), JSON.stringify(rsConfig, null, 2));

// ---------- run (serially: both mutate the tree in place) ----------
function run(label, cmdline) {
  console.error(`[diff] running ${label}: ${cmdline}`);
  const started = Date.now();
  const r = spawnSync("bash", ["-c", cmdline], { cwd, stdio: ["ignore", "inherit", "inherit"] });
  console.error(`[diff] ${label} finished in ${((Date.now() - started) / 1000).toFixed(1)}s (exit ${r.status})`);
  // Exit 1 can mean "score below break threshold"; only treat spawn failures as fatal.
  if (r.error) throw r.error;
}
run("stryker-js", `${jsInvoke} run ${join(work, "js.json")}`);
execSync(`git -C ${JSON.stringify(cwd)} status --porcelain`, { stdio: "ignore" }); // sanity
run("stryker-rs", `${JSON.stringify(rsBin)} run --config ${join(work, "rs.json")}`);

// ---------- normalize ----------
function normalizeReplacement(text) {
  return (text ?? "").replace(/\s+/g, " ").trim();
}
/** Aggressive normalization used only as a matching FALLBACK. */
function fuzzyReplacement(text) {
  return (text ?? "").replace(/[\s()]+/g, "");
}

function load(file) {
  const report = JSON.parse(readFileSync(join(work, file), "utf8"));
  // key: file :: mutator :: startLine:startCol-endLine:endCol
  const groups = new Map();
  for (const [path, fr] of Object.entries(report.files)) {
    for (const m of fr.mutants) {
      const l = m.location;
      const key = `${path} :: ${m.mutatorName} :: ${l.start.line}:${l.start.column}-${l.end.line}:${l.end.column}`;
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push({ replacement: m.replacement ?? "", status: m.status });
    }
  }
  return groups;
}

const js = load("js-report.json");
const rs = load("rs-report.json");

// ---------- diff ----------
const missing = []; // in js, not rs
const extra = []; // in rs, not js
const statusMismatch = [];
const unstable = [];
let matched = 0;

const allKeys = new Set([...js.keys(), ...rs.keys()]);
for (const key of allKeys) {
  const a = js.get(key) ?? [];
  const b = rs.get(key) ?? [];
  // Pair by normalized replacement, then fuzzy, then order.
  const bLeft = [...b];
  const pairs = [];
  const aLeft = [];
  for (const am of a) {
    let idx = bLeft.findIndex((bm) => normalizeReplacement(bm.replacement) === normalizeReplacement(am.replacement));
    if (idx < 0) idx = bLeft.findIndex((bm) => fuzzyReplacement(bm.replacement) === fuzzyReplacement(am.replacement));
    if (idx >= 0) {
      pairs.push([am, bLeft.splice(idx, 1)[0]]);
    } else {
      aLeft.push(am);
    }
  }
  // Same-size leftovers at one location: pair in order (replacement text
  // conventions differ; the location+mutator identity is what matters).
  while (aLeft.length > 0 && bLeft.length > 0) {
    pairs.push([aLeft.shift(), bLeft.shift()]);
  }
  for (const am of aLeft) missing.push(`${key} :: ${JSON.stringify(am.replacement).slice(0, 80)} [${am.status}]`);
  for (const bm of bLeft) extra.push(`${key} :: ${JSON.stringify(bm.replacement).slice(0, 80)} [${bm.status}]`);
  for (const [am, bm] of pairs) {
    matched += 1;
    if (am.status !== bm.status) {
      const line = `${key} :: js=${am.status} rs=${bm.status} :: ${JSON.stringify(am.replacement).slice(0, 60)}`;
      if (am.status === "Timeout" || bm.status === "Timeout") {
        unstable.push(line);
      } else {
        statusMismatch.push(line);
      }
    }
  }
}

// ---------- report ----------
function section(name, items) {
  console.log(`\n== ${name}: ${items.length}`);
  for (const item of items.slice(0, 40)) console.log("  " + item);
  if (items.length > 40) console.log(`  ... and ${items.length - 40} more`);
}
console.log(`\nmatched mutants: ${matched}`);
section("MISSING (js has, rs lacks)", missing.sort());
section("EXTRA (rs has, js lacks)", extra.sort());
section("STATUS MISMATCH", statusMismatch.sort());
section("UNSTABLE (Timeout on one side)", unstable.sort());

if (keepReports) {
  cpSync(join(work, "js-report.json"), join(keepReports, "js-report.json"));
  cpSync(join(work, "rs-report.json"), join(keepReports, "rs-report.json"));
  console.log(`\nreports kept in ${keepReports}`);
}
console.log(`\nwork dir: ${work}`);
process.exit(missing.length + extra.length + statusMismatch.length > 0 ? 1 : 0);
