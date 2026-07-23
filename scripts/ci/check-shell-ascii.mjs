#!/usr/bin/env node
// Shell and PowerShell scripts must be ASCII-only. None of them carries a BOM, so
// Windows PowerShell 5.1 decodes a .ps1 with the machine's ANSI codepage and every
// non-ASCII character in installer output, error text, and `irm | iex` banners reaches
// the user as mojibake. POSIX installers have the same exposure under a C-locale shell.
// The characters that showed up here were only decoration, so the rule is a flat ban
// rather than a per-character judgement call.
//
// Usage: node scripts/ci/check-shell-ascii.mjs [file...]
//   no arguments: every tracked *.sh and *.ps1 in the repository

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { relative, resolve, sep } from "node:path";

// Only what this repo actually accumulated; anything else is reported without a hint.
const REPLACEMENTS = new Map([
  ["─", "-"],
  ["—", "--"],
  ["–", "-"],
  ["→", "->"],
  ["←", "<-"],
  ["•", "-"],
  ["…", "..."],
  ["›", ">"],
  ["‘", "'"],
  ["’", "'"],
  ["“", '"'],
  ["”", '"'],
  ["﻿", "delete the byte-order mark"],
]);
const MAX_REPORTED_PER_FILE = 10;

// Enumeration is rooted at the working directory, so the check runs against the
// checkout it was invoked in rather than the one it was copied from.
function repositoryRoot() {
  const result = spawnSync("git", ["rev-parse", "--show-toplevel"], { encoding: "utf8" });
  if (result.status !== 0) throw new Error("not a git repository; pass the files explicitly");
  return result.stdout.trim();
}

function trackedScripts(root) {
  const result = spawnSync("git", ["ls-files", "-z", "--", "*.sh", "*.ps1"], {
    cwd: root,
    encoding: "utf8",
  });
  if (result.status !== 0) {
    throw new Error(`git ls-files failed: ${(result.stderr ?? "").trim()}`);
  }
  return result.stdout.split("\0").filter(Boolean).map((file) => resolve(root, file));
}

function findings(text) {
  const found = [];
  text.split(/\r?\n/).forEach((line, index) => {
    // Code points, not UTF-16 units: an astral character reports as one column.
    [...line].forEach((character, column) => {
      if (character.codePointAt(0) > 0x7f) {
        found.push({ line: index + 1, column: column + 1, character });
      }
    });
  });
  return found;
}

function describe(file, { line, column, character }) {
  const code = character.codePointAt(0).toString(16).toUpperCase().padStart(4, "0");
  const hint = REPLACEMENTS.get(character);
  const suffix = hint ? ` (use ${JSON.stringify(hint)})` : "";
  return `${file}:${line}:${column}: U+${code} ${JSON.stringify(character)}${suffix}`;
}

const args = process.argv.slice(2);
if (args.includes("--help")) {
  console.log("usage: node scripts/ci/check-shell-ascii.mjs [file...]");
  process.exit(0);
}

const root = args.length > 0 ? process.cwd() : repositoryRoot();
const files = args.length > 0 ? args.map((file) => resolve(root, file)) : trackedScripts(root);

let offenders = 0;
let total = 0;
for (const file of files) {
  const found = findings(readFileSync(file, "utf8"));
  if (found.length === 0) continue;
  offenders += 1;
  total += found.length;
  const shown = found.slice(0, MAX_REPORTED_PER_FILE);
  // Forward slashes on every platform: GitHub annotations resolve no other form.
  const name = (relative(root, file) || file).split(sep).join("/");
  for (const finding of shown) {
    const message = describe(name, finding);
    console.error(process.env.GITHUB_ACTIONS
      ? `::error file=${name},line=${finding.line},col=${finding.column}::${message}`
      : message);
  }
  if (found.length > shown.length) {
    console.error(`${name}: ${found.length - shown.length} more non-ASCII character(s)`);
  }
}

if (offenders > 0) {
  console.error(
    `\n${total} non-ASCII character(s) in ${offenders} file(s). Shell and PowerShell`
      + " scripts must be ASCII-only: they ship to user machines and are decoded by the"
      + " console codepage, not by the file's own encoding.",
  );
  process.exit(1);
}
console.log(`ASCII-only: ${files.length} shell/PowerShell script(s)`);
