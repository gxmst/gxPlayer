import { readFile } from "node:fs/promises";

const root = new URL("../", import.meta.url);
const packageJson = JSON.parse(await readFile(new URL("package.json", root), "utf8"));
const tauriConfig = JSON.parse(
  await readFile(new URL("src-tauri/tauri.conf.json", root), "utf8"),
);
const cargoToml = await readFile(new URL("Cargo.toml", root), "utf8");
const cargoVersion = cargoToml.match(
  /\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m,
)?.[1];

const versions = new Map([
  ["package.json", packageJson.version],
  ["Cargo.toml", cargoVersion],
  ["src-tauri/tauri.conf.json", tauriConfig.version],
]);
const unique = new Set(versions.values());

if (unique.has(undefined) || unique.size !== 1) {
  for (const [file, version] of versions) {
    console.error(`${file}: ${version ?? "<missing>"}`);
  }
  process.exitCode = 1;
} else {
  console.log(`GXPlayer version ${[...unique][0]} is consistent.`);
}
