#!/usr/bin/env node
// Launcher: exec the platform-specific binary from optionalDependencies.
"use strict";
const { spawnSync } = require("node:child_process");

const PLATFORMS = {
  "darwin-arm64": "@stryker-rs/cli-darwin-arm64",
  "darwin-x64": "@stryker-rs/cli-darwin-x64",
  "linux-x64": "@stryker-rs/cli-linux-x64-gnu",
  "linux-arm64": "@stryker-rs/cli-linux-arm64-gnu",
};

const key = `${process.platform}-${process.arch}`;
const pkg = PLATFORMS[key];
if (!pkg) {
  console.error(`stryker-rs: unsupported platform ${key}`);
  process.exit(1);
}

let binary;
try {
  binary = require.resolve(`${pkg}/stryker`);
} catch {
  console.error(
    `stryker-rs: platform package ${pkg} is not installed.\n` +
      `Your package manager may have skipped optionalDependencies; ` +
      `reinstall with them enabled.`
  );
  process.exit(1);
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  console.error(`stryker-rs: failed to launch ${binary}: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
