import { readFile } from "node:fs/promises";

const root = new URL("../", import.meta.url);
const readText = (path) => readFile(new URL(path, root), "utf8");

const packageJson = JSON.parse(await readText("package.json"));
const packageLock = JSON.parse(await readText("package-lock.json"));
const tauriConfig = JSON.parse(await readText("src-tauri/tauri.conf.json"));
const cargoToml = await readText("Cargo.toml");
const cargoLock = await readText("Cargo.lock");
const readme = await readText("README.md");

const problems = [];
const capture = (text, pattern, label) => {
  const value = text.match(pattern)?.[1]?.trim();
  if (!value) problems.push(`${label}: <missing>`);
  return value;
};

const cargoVersion = capture(
  cargoToml,
  /\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m,
  "Cargo.toml",
);
const readmeEnglishVersion = capture(
  readme,
  /^\| Version \| ([^|]+) \|$/m,
  "README.md (English)",
);
const readmeChineseVersion = capture(
  readme,
  /^\| 版本 \| ([^|]+) \|$/m,
  "README.md (Chinese)",
);

const versions = new Map([
  ["package.json", packageJson.version],
  ["package-lock.json", packageLock.version],
  ["package-lock.json packages root", packageLock.packages?.[""]?.version],
  ["Cargo.toml", cargoVersion],
  ["src-tauri/tauri.conf.json", tauriConfig.version],
  ["README.md (English)", readmeEnglishVersion],
  ["README.md (Chinese)", readmeChineseVersion],
]);

const membersText = cargoToml.match(/\[workspace\][\s\S]*?members\s*=\s*\[([\s\S]*?)\]/)?.[1];
if (!membersText) problems.push("Cargo.toml workspace members: <missing>");
const memberPaths = [...(membersText ?? "").matchAll(/"([^"]+)"/g)].map((match) => match[1]);
const lockPackages = cargoLock
  .split("[[package]]")
  .slice(1)
  .map((block) => ({
    name: block.match(/^name\s*=\s*"([^"]+)"/m)?.[1],
    version: block.match(/^version\s*=\s*"([^"]+)"/m)?.[1],
    external: /^source\s*=/m.test(block),
  }));

for (const memberPath of memberPaths) {
  const manifestPath = `${memberPath.replaceAll("\\", "/")}/Cargo.toml`;
  const manifest = await readText(manifestPath);
  const name = capture(manifest, /^name\s*=\s*"([^"]+)"/m, `${manifestPath} package name`);
  if (!/^version\.workspace\s*=\s*true$/m.test(manifest)) {
    problems.push(`${manifestPath}: expected version.workspace = true`);
  }
  if (!name) continue;
  const lockPackage = lockPackages.find((entry) => entry.name === name && !entry.external);
  if (!lockPackage?.version) {
    problems.push(`Cargo.lock (${name}): <missing>`);
  } else {
    versions.set(`Cargo.lock (${name})`, lockPackage.version);
  }
}

for (const [label, version] of versions) {
  if (typeof version !== "string" || version.length === 0) {
    problems.push(`${label}: <missing>`);
  }
}

const unique = new Set([...versions.values()].filter((value) => typeof value === "string"));
if (unique.size !== 1) {
  for (const [label, version] of versions) {
    problems.push(`${label}: ${version ?? "<missing>"}`);
  }
}

const version = unique.size === 1 ? [...unique][0] : undefined;
const semverPattern = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;
if (version && !semverPattern.test(version)) {
  problems.push(`invalid SemVer: ${version}`);
}
if (version && semverPattern.test(version)) {
  const [major, minor, patch] = version.split(/[+-]/, 1)[0].split(".").map(Number);
  if (major > 255 || minor > 255 || patch > 65_535) {
    problems.push(`version exceeds Windows MSI numeric limits: ${version}`);
  }
}

if (problems.length > 0) {
  for (const problem of [...new Set(problems)]) console.error(problem);
  process.exitCode = 1;
} else {
  console.log(`GXPlayer version ${version} is consistent across manifests, locks, and README.`);
}
