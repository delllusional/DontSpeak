import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { resolveBash, windowsBashCandidates } from "./run-bash.mjs";

const script = join(dirname(fileURLToPath(import.meta.url)), "run-bash.mjs");

test("Windows candidates prefer Git Bash and exclude the System32 WSL launcher", () => {
  const env = {
    DONTSPEAK_GIT_BASH: "C:\\Tools\\Git\\bin\\bash.exe",
    ProgramFiles: "C:\\Program Files",
    LOCALAPPDATA: "C:\\Users\\test\\AppData\\Local",
    WINDIR: "C:\\Windows",
  };
  const candidates = windowsBashCandidates({
    env,
    execPath: "C:\\Program Files\\Git\\mingw64\\libexec\\git-core",
  });

  assert.equal(candidates[0], "C:\\Tools\\Git\\bin\\bash.exe");
  assert.ok(candidates.includes("C:\\Program Files\\Git\\bin\\bash.exe"));
  assert.ok(!candidates.some((candidate) => candidate.toLowerCase().includes("\\system32\\bash.exe")));
  assert.ok(!candidates.includes("bash"));
});

test("Windows resolution never falls back to a bare bash command", () => {
  assert.throws(
    () => resolveBash({ platform: "win32", env: {}, execPath: undefined, exists: () => false }),
    /Git Bash was not found/,
  );
});

test("the wrapper executes with the native Bash selected for this platform", () => {
  const result = spawnSync(process.execPath, [script, "-c", "printf dontspeak-bash"], {
    encoding: "utf8",
  });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, "dontspeak-bash");
});

