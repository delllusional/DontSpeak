import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const CHECKER = path.join(path.dirname(fileURLToPath(import.meta.url)), "check-shell-ascii.mjs");

function sandbox(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "dontspeak-ascii-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  return root;
}

function write(file, contents) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, contents, "utf8");
  return file;
}

function run(args, cwd) {
  return spawnSync(process.execPath, [CHECKER, ...args], { cwd, encoding: "utf8" });
}

test("ASCII-only scripts pass", (t) => {
  const root = sandbox(t);
  const file = write(path.join(root, "install.sh"), '#!/bin/sh\necho "done -> ok"\n');

  const result = run([file], root);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /ASCII-only: 1/);
});

test("a non-ASCII character fails with its position and a replacement", (t) => {
  const root = sandbox(t);
  const file = write(path.join(root, "install.ps1"), 'Write-Host "step — done"\n');

  const result = run([file], root);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /install\.ps1:1:18: U\+2014 .* \(use "--"\)/);
});

test("a byte-order mark is caught like any other non-ASCII byte", (t) => {
  const root = sandbox(t);
  const file = write(path.join(root, "uninstall.sh"), "﻿#!/bin/sh\n");

  const result = run([file], root);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /U\+FEFF/);
});

// Without the cap a mass-converted file buries the summary under hundreds of lines.
test("per-file reporting is capped and reports the remainder", (t) => {
  const root = sandbox(t);
  const file = write(path.join(root, "package.sh"), `${"─\n".repeat(12)}`);

  const result = run([file], root);
  assert.equal(result.status, 1);
  assert.equal(result.stderr.match(/U\+2500/g).length, 10);
  assert.match(result.stderr, /package\.sh: 2 more non-ASCII character\(s\)/);
});

// Default enumeration is the gate's real contract: tracked shell scripts anywhere in
// the checkout, and nothing else.
test("without arguments it scans tracked .sh and .ps1 files only", (t) => {
  const root = sandbox(t);
  const git = (...args) => spawnSync("git", args, { cwd: root, encoding: "utf8" });
  git("init", "--quiet");
  write(path.join(root, "apps", "build.ps1"), 'Write-Host "arrow →"\n');
  write(path.join(root, "README.md"), "prose keeps its — dashes\n");
  write(path.join(root, "untracked.sh"), "echo •\n");
  git("add", "apps/build.ps1", "README.md");

  const result = run([], root);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /apps\/build\.ps1:1:19: U\+2192/);
  assert.doesNotMatch(result.stderr, /README\.md/);
  assert.doesNotMatch(result.stderr, /untracked\.sh/);
});
