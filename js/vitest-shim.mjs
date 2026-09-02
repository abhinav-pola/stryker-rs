// stryker-rs vitest shim: a long-lived Node process speaking NDJSON over
// stdio. One request in flight at a time (the Rust side serializes calls).
//
// Protocol (one JSON object per line):
//   → {"id":1,"kind":"init","cwd":"...","configFile":null,"namespace":"__stryker__"}
//   ← {"id":1,"kind":"ready"}
//   → {"id":2,"kind":"dryRun","coverage":true,"timeoutMs":300000}
//   ← {"id":2,"kind":"dryRunResult","tests":[...],"coverage":{...}|null}
//   → {"id":3,"kind":"mutantRun","activeMutant":"7","testFilter":[...]|null,"hitLimit":400}
//   ← {"id":3,"kind":"mutantRunResult","status":"killed","killedBy":[...],"testsRan":4,"failureMessage":"..."}
//   → {"id":4,"kind":"dispose"}   ← {"id":4,"kind":"disposed"}
// Unsolicited: ← {"kind":"crash","error":"..."} then exit 1.
//
// All console output from vitest is silenced; stdout carries ONLY protocol
// lines (stderr is free-form diagnostics).

import { createInterface } from "node:readline";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

let ctx = null;
let namespace = "__stryker__";
let relative = (p) => p;

function send(obj) {
  process.stdout.write(JSON.stringify(obj) + "\n");
}

function testIdOf(task, filepath) {
  const names = [];
  let node = task;
  let guard = 0;
  // The File node reports type "suite"; detect it via `filepath`.
  while (node && node.filepath === undefined && guard++ < 32) {
    if (node.name) names.unshift(node.name);
    const next = node.suite ?? node.file;
    if (next === node) break; // self-referential chain: stop
    node = next;
  }
  return `${relative(filepath)} > ${names.join(" > ")}`;
}

function collectTests() {
  const tests = [];
  const files = ctx.state.getFiles();
  for (const file of files) {
    const stack = [...file.tasks];
    while (stack.length > 0) {
      const task = stack.shift();
      if (task.type === "suite") {
        stack.unshift(...task.tasks);
      } else if (task.type === "test") {
        tests.push({
          id: task.meta?.strykerTestId ?? testIdOf(task, file.filepath),
          file: relative(file.filepath),
          timeMs: task.result?.duration ?? 0,
          state: task.result?.state ?? (task.mode === "skip" || task.mode === "todo" ? "skip" : "unknown"),
          error: task.result?.errors?.[0]?.message ?? null,
        });
      }
    }
  }
  return tests;
}

function collectFileErrors() {
  const errors = [];
  for (const file of ctx.state.getFiles()) {
    if (file.result?.state === "fail") {
      const message = String(file.result?.errors?.[0]?.message ?? "file-level failure");
      errors.push(`${file.filepath ?? file.name}: ${message}`);
    }
  }
  return errors;
}

function collectMeta() {
  let coverage = null;
  let hitCount = null;
  for (const file of ctx.state.getFiles()) {
    const meta = file.meta ?? {};
    if (meta.mutantCoverage) {
      coverage = mergeCoverage(coverage, meta.mutantCoverage);
    }
    if (typeof meta.hitCount === "number") {
      hitCount = Math.max(hitCount ?? 0, meta.hitCount);
    }
  }
  return { coverage, hitCount };
}

function mergeCoverage(a, b) {
  if (!a) return b;
  for (const [k, v] of Object.entries(b.static ?? {})) {
    a.static[k] = (a.static[k] ?? 0) + v;
  }
  for (const [test, buckets] of Object.entries(b.perTest ?? {})) {
    a.perTest[test] ??= {};
    for (const [k, v] of Object.entries(buckets)) {
      a.perTest[test][k] = (a.perTest[test][k] ?? 0) + v;
    }
  }
  return a;
}

// Per-run state is BAKED INTO the setup file (rewritten before each run;
// vite re-transforms on mtime change). This works in every worker runtime —
// including Cloudflare workerd (pool-workers), which has no process.env or
// host filesystem — and avoids vitest's provide()/inject() whose re-provide
// semantics vary across versions.
let setupFilePath = null;
let setupTemplate = null;

function writeState(state) {
  writeFileSync(setupFilePath, setupTemplate.replace("__STATE_JSON__", JSON.stringify(state)));
}

async function runTests(files, testNamePattern) {
  ctx.state.filesMap.clear(); // reset between runs (vitest#3017 workaround)
  ctx.config.testNamePattern = testNamePattern ?? undefined;
  for (const project of ctx.projects ?? []) {
    project.config.testNamePattern = testNamePattern ?? undefined;
  }
  await ctx.start(files && files.length > 0 ? files : undefined);
}

const SETUP_TEMPLATE = String.raw`
import { beforeEach, afterEach, afterAll } from "vitest";
const NS = "__NAMESPACE__";
const g = globalThis;
const ns = (g[NS] ??= {});
const state = __STATE_JSON__;
const mode = state.mode;
if (mode === "mutant") {
  ns.activeMutant = String(state.activeMutant);
  if (state.hitLimit != null) { ns.hitLimit = state.hitLimit; ns.hitCount = 0; }
}
const ROOT = "__ROOT__";
function relPath(p) {
  return p.startsWith(ROOT) ? p.slice(ROOT.length) : p;
}
function idOf(task) {
  const names = [];
  let t = task;
  let guard = 0;
  // The File node has type "suite" too; detect it via its filepath field.
  while (t && t.filepath === undefined && guard++ < 32) {
    if (t.name) names.unshift(t.name);
    const next = t.suite ?? t.file;
    if (next === t) break;
    t = next;
  }
  const filepath = task.file?.filepath ?? "";
  return relPath(filepath) + " > " + names.join(" > ");
}
beforeEach((testCtx) => {
  const id = idOf(testCtx.task);
  testCtx.task.meta.strykerTestId = id;
  if (mode !== "mutant") ns.currentTestId = id;
});
afterEach(() => { ns.currentTestId = undefined; });
// vitest >= 4.1: first hook arg MUST be an object destructuring pattern.
afterAll(({}, suite) => {
  const file = suite?.file ?? suite;
  if (file && file.meta) {
    if (mode !== "mutant") file.meta.mutantCoverage = ns.mutantCoverage ?? { static: {}, perTest: {} };
    if (ns.hitCount !== undefined) file.meta.hitCount = ns.hitCount;
  }
});
`;

const handlers = {
  async init(msg) {
    namespace = msg.namespace ?? "__stryker__";
    const { createVitest } = await import(
      pathToFileURL(join(msg.cwd, "node_modules", "vitest", "dist", "node.js")).href
    ).catch(() => import("vitest/node"));
    const path = await import("node:path");
    relative = (p) => path.relative(msg.cwd, p).replaceAll(path.sep, "/");

    const setupDir = mkdtempSync(join(tmpdir(), "stryker-vitest-"));
    setupFilePath = join(setupDir, "stryker-setup.mjs");
    const rootWithSep = msg.cwd.endsWith("/") ? msg.cwd : msg.cwd + "/";
    setupTemplate = SETUP_TEMPLATE.replaceAll("__NAMESPACE__", namespace).replaceAll(
      "__ROOT__",
      JSON.stringify(rootWithSep).slice(1, -1),
    );
    writeState({ mode: "dry-run", activeMutant: null, hitLimit: null });
    const setupFile = setupFilePath;

    ctx = await createVitest("test", {
      root: msg.cwd,
      ...(msg.configFile ? { config: msg.configFile } : {}),
      watch: false,
      pool: "threads",
      maxWorkers: 1,
      minWorkers: 1,
      fileParallelism: false,
      coverage: { enabled: false },
      reporters: [{ /* silent custom reporter */ }],
      onConsoleLog: () => false,
    });
    for (const project of ctx.projects ?? []) {
      project.config.setupFiles = [setupFile, ...(project.config.setupFiles ?? [])];
      project.config.maxConcurrency = 1;
    }
    send({ id: msg.id, kind: "ready" });
  },

  async dryRun(msg) {
    writeState({ mode: "dry-run", activeMutant: null, hitLimit: null });
    await runTests(null, null);
    const tests = collectTests();
    const { coverage } = collectMeta();
    const fileErrors = collectFileErrors();
    if (fileErrors.length > 0) {
      send({ id: msg.id, kind: "crash", error: `dry run file-level failure: ${fileErrors[0]}` });
      return;
    }
    send({
      id: msg.id,
      kind: "dryRunResult",
      tests,
      coverage: msg.coverage ? coverage : null,
    });
  },

  async mutantRun(msg) {
    writeState({
      mode: "mutant",
      activeMutant: String(msg.activeMutant),
      hitLimit: msg.hitLimit ?? null,
    });
    let files = null;
    let namePattern = null;
    if (Array.isArray(msg.testFilter) && msg.testFilter.length > 0) {
      const fileSet = [...new Set(msg.testFilter.map((t) => t.split(" > ")[0]))];
      files = fileSet;
      const names = [...new Set(msg.testFilter.map((t) => t.split(" > ").at(-1)))];
      namePattern = names.map((n) => n.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|");
    }
    await runTests(files, namePattern);
    const tests = collectTests();
    const executed = tests.filter((t) => t.state === "pass" || t.state === "fail");
    const { hitCount } = collectMeta();
    if (msg.hitLimit != null && hitCount != null && hitCount > msg.hitLimit) {
      send({ id: msg.id, kind: "mutantRunResult", status: "timeout", reason: `Hit limit reached (${hitCount}/${msg.hitLimit})` });
      return;
    }
    const killedBy = executed.filter((t) => t.state === "fail").map((t) => t.id);
    const fileErrors = collectFileErrors();
    const killed = killedBy.length > 0 || fileErrors.length > 0;
    send({
      id: msg.id,
      kind: "mutantRunResult",
      status: killed ? "killed" : "survived",
      killedBy,
      testsRan: executed.length,
      failureMessage:
        executed.find((t) => t.state === "fail")?.error ?? fileErrors[0] ?? null,
    });
  },

  async dispose(msg) {
    try {
      await ctx?.close();
    } finally {
      send({ id: msg.id, kind: "disposed" });
      process.exit(0);
    }
  },
};

const rl = createInterface({ input: process.stdin });
let queue = Promise.resolve();
rl.on("line", (line) => {
  if (!line.trim()) return;
  queue = queue.then(async () => {
    let msg;
    try {
      msg = JSON.parse(line);
    } catch (e) {
      send({ kind: "crash", error: `bad request: ${e}` });
      return;
    }
    const handler = handlers[msg.kind];
    if (!handler) {
      send({ id: msg.id, kind: "crash", error: `unknown kind ${msg.kind}` });
      return;
    }
    try {
      await handler(msg);
    } catch (e) {
      send({ id: msg.id, kind: "crash", error: String(e?.stack ?? e) });
    }
  });
});
process.on("uncaughtException", (e) => {
  send({ kind: "crash", error: String(e?.stack ?? e) });
  process.exit(1);
});
