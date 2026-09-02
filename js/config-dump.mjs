// One-shot loader for executable stryker configs (.mjs / .js).
// Usage: bun js/config-dump.mjs <abs-config-path>   (node works too)
// Prints JSON.stringify(default export) to stdout; everything else to stderr.
import { pathToFileURL } from "node:url";

const [, , configPath] = process.argv;
if (!configPath) {
  console.error("usage: config-dump.mjs <config-path>");
  process.exit(2);
}
const mod = await import(pathToFileURL(configPath).href);
const config = mod.default ?? mod;
process.stdout.write(JSON.stringify(config));
