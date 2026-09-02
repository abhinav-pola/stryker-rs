// Assemble npm platform packages from built binaries into dist-npm/.
//
// Usage:  node scripts/package-npm.mjs <version>
// Expects binaries at:
//   target/release/stryker                                (host)
//   target/<triple>/release/stryker                       (cross)
// Only packages whose binary exists are emitted.
import { cpSync, existsSync, mkdirSync, rmSync, writeFileSync, chmodSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { execSync } from "node:child_process";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const version = process.argv[2];
if (!version) {
  console.error("usage: node scripts/package-npm.mjs <version>");
  process.exit(2);
}

const TARGETS = [
  { pkg: "cli-darwin-arm64", os: "darwin", cpu: "arm64", triple: "aarch64-apple-darwin" },
  { pkg: "cli-darwin-x64", os: "darwin", cpu: "x64", triple: "x86_64-apple-darwin" },
  { pkg: "cli-linux-x64-gnu", os: "linux", cpu: "x64", triple: "x86_64-unknown-linux-gnu" },
  { pkg: "cli-linux-arm64-gnu", os: "linux", cpu: "arm64", triple: "aarch64-unknown-linux-gnu" },
];

const host = `${process.platform}-${process.arch}`;
const dist = join(root, "dist-npm");
rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });

let emitted = 0;
for (const t of TARGETS) {
  const isHost = host === `${t.os === "darwin" ? "darwin" : "linux"}-${t.cpu}` && process.platform === t.os;
  const candidates = [
    join(root, "target", t.triple, "release", "stryker"),
    ...(isHost ? [join(root, "target", "release", "stryker")] : []),
  ];
  const binary = candidates.find(existsSync);
  if (!binary) {
    console.error(`skip ${t.pkg}: no binary (looked at ${candidates.join(", ")})`);
    continue;
  }
  const dir = join(dist, t.pkg);
  mkdirSync(dir, { recursive: true });
  cpSync(binary, join(dir, "stryker"));
  chmodSync(join(dir, "stryker"), 0o755);
  writeFileSync(
    join(dir, "package.json"),
    JSON.stringify(
      {
        name: `@stryker-rs/${t.pkg}`,
        version,
        description: `stryker-rs binary for ${t.os}-${t.cpu}`,
        license: "Apache-2.0",
        repository: "github:abhinav-pola/stryker-rs",
        os: [t.os],
        cpu: [t.cpu],
        files: ["stryker"],
      },
      null,
      2,
    ),
  );
  emitted += 1;
  console.log(`packaged ${t.pkg}`);
}

// Main package with the requested version stamped in.
const mainDir = join(dist, "stryker-rs");
cpSync(join(root, "npm", "stryker-rs"), mainDir, { recursive: true });
const mainPkgPath = join(mainDir, "package.json");
const mainPkg = JSON.parse(execSync(`cat ${JSON.stringify(mainPkgPath)}`).toString());
mainPkg.version = version;
for (const key of Object.keys(mainPkg.optionalDependencies)) {
  mainPkg.optionalDependencies[key] = version;
}
writeFileSync(mainPkgPath, JSON.stringify(mainPkg, null, 2));
console.log(`packaged stryker-rs (${emitted} platform packages) into dist-npm/`);
