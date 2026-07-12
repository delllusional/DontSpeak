import { cp, readdir, readFile, rm } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const source = path.join(root, ".agents", "skills");
const mirrors = [
  path.join(root, ".claude", "skills"),
  path.join(root, ".qwen", "skills"),
];

const args = process.argv.slice(2);
if (args.some((arg) => arg !== "--check")) {
  console.error("usage: node scripts/sync-agent-skills.mjs [--check]");
  process.exit(2);
}
const checkOnly = args.includes("--check");
const requiredSkillReferences = [
  "../../../docs/TASK-BASELINE.md",
  "../../../docs/TASK-EFFORT.md",
];

async function snapshot(dir, prefix = "") {
  const files = new Map();
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const relative = path.join(prefix, entry.name);
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      for (const [name, contents] of await snapshot(full, relative)) {
        files.set(name, contents);
      }
    } else if (entry.isFile()) {
      files.set(relative, await readFile(full));
    }
  }
  return files;
}

async function differences(expected, actualDir) {
  let actual;
  try {
    actual = await snapshot(actualDir);
  } catch (error) {
    if (error.code === "ENOENT") return [`missing directory ${path.relative(root, actualDir)}`];
    throw error;
  }

  const names = new Set([...expected.keys(), ...actual.keys()]);
  const drift = [];
  for (const name of [...names].sort()) {
    if (!expected.has(name)) drift.push(`extra ${name}`);
    else if (!actual.has(name)) drift.push(`missing ${name}`);
    else if (!expected.get(name).equals(actual.get(name))) drift.push(`changed ${name}`);
  }
  return drift;
}

const expected = await snapshot(source);
const missingTaskSetup = [...expected]
  .flatMap(([name, contents]) => {
    if (!name.endsWith("SKILL.md")) return [];
    const text = contents.toString("utf8");
    return requiredSkillReferences
      .filter((reference) => !text.includes(reference))
      .map((reference) => `${name}: ${reference}`);
  })
  .sort();
if (missingTaskSetup.length > 0) {
  console.error("canonical skills missing required task-setup references:");
  for (const item of missingTaskSetup) console.error(`  ${item}`);
  process.exit(1);
}

if (!checkOnly) {
  for (const mirror of mirrors) {
    await rm(mirror, { recursive: true, force: true });
    await cp(source, mirror, { recursive: true });
    console.log(`synced ${path.relative(root, mirror)}`);
  }
}

let failed = false;
for (const mirror of mirrors) {
  const drift = await differences(expected, mirror);
  if (drift.length === 0) {
    console.log(`clean ${path.relative(root, mirror)}`);
    continue;
  }
  failed = true;
  console.error(`${path.relative(root, mirror)} differs from .agents/skills:`);
  for (const item of drift) console.error(`  ${item}`);
}
if (failed) process.exit(1);
