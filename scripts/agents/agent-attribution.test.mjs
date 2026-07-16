import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  ensureCommitMessageHook,
  gitCommitWorkingDirectory,
  privateHooksDirectory,
  resolveAttribution,
  rewriteCommitMessage,
  validateAttribution,
  validateCacheRecord,
  validateCommitMessage,
} from "./agent-attribution.mjs";

function temporaryDirectory(t) {
  const directory = mkdtempSync(join(tmpdir(), "dontspeak-attribution-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  return directory;
}

function jsonLines(file, rows) {
  mkdirSync(join(file, ".."), { recursive: true });
  writeFileSync(file, `${rows.map((row) => JSON.stringify(row)).join("\n")}\n`, "utf8");
}

test("detects real git commit shell commands", () => {
  assert.notEqual(gitCommitWorkingDirectory("git commit -m test"), undefined);
  assert.notEqual(gitCommitWorkingDirectory("cd rust && git -C .. commit --amend"), undefined);
  assert.equal(gitCommitWorkingDirectory("git status"), undefined);
  assert.equal(gitCommitWorkingDirectory("Write-Output 'git commit'"), undefined);
});

test("resolves repeated git working-directory options for the target repository", () => {
  const base = resolve("workspace");
  assert.equal(
    gitCommitWorkingDirectory("git -C nested -C repository commit -m test", base),
    join(base, "nested", "repository"),
  );
  assert.equal(gitCommitWorkingDirectory("git -C nested status", base), undefined);
});

test("Codex uses the hook model and matching turn effort", (t) => {
  const transcript = join(temporaryDirectory(t), "rollout.jsonl");
  jsonLines(transcript, [
    { type: "turn_context", payload: { model: "gpt-5.5", effort: "high" } },
    { type: "turn_context", payload: { model: "gpt-5.6-sol", effort: "xhigh" } },
  ]);
  assert.deepEqual(
    resolveAttribution("codex", { model: "gpt-5.6-sol", transcript_path: transcript }),
    {
      model: "gpt-5.6-sol",
      effort: "xhigh",
      errors: [],
    },
  );
});

test("Claude combines its transcript model with applied hook effort", (t) => {
  const transcript = join(temporaryDirectory(t), "session.jsonl");
  jsonLines(transcript, [
    { type: "assistant", message: { role: "assistant", model: "claude-opus-4-8" } },
  ]);
  const result = resolveAttribution("claude", {
    transcript_path: transcript,
    effort: { level: "max" },
  });
  assert.equal(result.model, "claude-opus-4-8");
  assert.equal(result.effort, "max");
  assert.deepEqual(result.errors, []);
});

test("Qwen combines its transcript model with its persisted effort selection", (t) => {
  const base = temporaryDirectory(t);
  const root = join(base, "repo");
  const home = join(base, "home");
  const transcript = join(base, "qwen.jsonl");
  mkdirSync(join(home, ".qwen"), { recursive: true });
  mkdirSync(join(root, ".qwen"), { recursive: true });
  writeFileSync(join(home, ".qwen", "settings.json"), JSON.stringify({ model: { reasoningEffort: "high" } }));
  writeFileSync(join(root, ".qwen", "settings.json"), JSON.stringify({ model: { reasoningEffort: "xhigh" } }));
  jsonLines(transcript, [{ type: "assistant", model: "qwen3-coder-plus" }]);
  const result = resolveAttribution("qwen", { transcript_path: transcript }, { root, home });
  assert.equal(result.model, "qwen3-coder-plus");
  assert.equal(result.effort, "xhigh");
  assert.deepEqual(result.errors, []);
});

test("Grok reads the current session model and reasoning effort", (t) => {
  const home = temporaryDirectory(t);
  const session = join(home, ".grok", "sessions", "project", "session-1");
  mkdirSync(session, { recursive: true });
  writeFileSync(join(session, "summary.json"), JSON.stringify({ current_model_id: "grok-4.5" }));
  jsonLines(join(session, "events.jsonl"), [
    { type: "turn_started", model_id: "grok-4.5", reasoning_effort: "high" },
  ]);
  const result = resolveAttribution(
    "grok",
    { sessionId: "session-1" },
    { home, env: { CLAUDE_EFFORT: "low" } },
  );
  assert.equal(result.model, "grok-4.5");
  assert.equal(result.effort, "high");
  assert.deepEqual(result.errors, []);
});

test("Grok reports none only when the catalog proves reasoning is unsupported", (t) => {
  const home = temporaryDirectory(t);
  const session = join(home, ".grok", "sessions", "project", "session-2");
  mkdirSync(session, { recursive: true });
  writeFileSync(join(session, "summary.json"), JSON.stringify({ current_model_id: "grok-build" }));
  writeFileSync(join(home, ".grok", "models_cache.json"), JSON.stringify({
    models: { "grok-build": { info: { supports_reasoning_effort: false } } },
  }));
  const result = resolveAttribution("grok", { sessionId: "session-2" }, { home });
  assert.equal(result.effort, "none");
  assert.deepEqual(result.errors, []);
});

test("unavailable effort is an error instead of a guessed default", () => {
  assert.deepEqual(validateAttribution("grok-4.5", undefined), [
    "active reasoning-effort level is unavailable",
  ]);
  assert.match(validateAttribution("gpt-5.6-sol", "default")[0], /ambiguous effort/);
});

test("family-level GPT names are rejected instead of accepted as exact slugs", () => {
  assert.match(validateAttribution("gpt-5", "high")[0], /not an exact slug/);
  assert.match(validateAttribution("gpt-5.6", "high")[0], /not an exact slug/);
  assert.deepEqual(validateAttribution("gpt-5.6-sol", "high"), []);
});

test("rewriting replaces one guessed trailer and removes built-in attribution", () => {
  const rewritten = rewriteCommitMessage(
    "Fix the thing\n\nAgent: gpt-5 high\nCo-Authored-By: Codex <noreply@example.com>\n",
    "gpt-5.6-sol",
    "xhigh",
  );
  assert.equal(rewritten, "Fix the thing\n\nAgent: gpt-5.6-sol xhigh\n");
  assert.deepEqual(validateCommitMessage(rewritten), []);
});

test("rewriting preserves distinct squash attribution pairs", () => {
  const rewritten = rewriteCommitMessage(
    "Combine work\n\nAgent: claude-opus-4-8 max\nAgent: qwen3-coder-plus xhigh\n",
    "gpt-5.6-sol",
    "xhigh",
  );
  assert.match(rewritten, /Agent: claude-opus-4-8 max\nAgent: qwen3-coder-plus xhigh\nAgent: gpt-5\.6-sol xhigh\n$/);
  assert.deepEqual(validateCommitMessage(rewritten), []);
});

test("commit validation rejects unknown effort and non-final trailers", () => {
  assert.deepEqual(validateCommitMessage("Subject\n\nAgent: model unknown\n"), [
    "Agent: model unknown: unsupported or ambiguous effort level: \"unknown\"",
  ]);
  assert.match(validateCommitMessage("Agent: model high\n\nBody\n")[0], /missing final/);
});

test("runtime captures must be fresh and belong to the active session", () => {
  const now = Date.now();
  const record = {
    version: 1,
    root: "/repo",
    client: "codex",
    sessionId: "one",
    model: "gpt-5.6-sol",
    effort: "xhigh",
    errors: [],
    capturedAt: new Date(now).toISOString(),
  };
  assert.deepEqual(validateCacheRecord(record, "/repo", { CODEX_THREAD_ID: "one" }, now), []);
  assert.match(validateCacheRecord(record, "/repo", { CODEX_THREAD_ID: "two" }, now)[0], /different agent session/);
});

test("hook installation is isolated to the current worktree config", (t) => {
  const root = temporaryDirectory(t);
  const initialized = spawnSync("git", ["init", "-q", root], { encoding: "utf8" });
  assert.equal(initialized.status, 0, initialized.stderr);
  const originalHooks = join(root, "original-hooks");
  mkdirSync(originalHooks);
  writeFileSync(join(originalHooks, "commit-msg"), "#!/bin/sh\nexit 0\n", { mode: 0o755 });
  const configuredOriginal = spawnSync("git", ["config", "core.hooksPath", originalHooks], {
    cwd: root,
    encoding: "utf8",
  });
  assert.equal(configuredOriginal.status, 0, configuredOriginal.stderr);
  const hooks = ensureCommitMessageHook(root);
  assert.equal(hooks, privateHooksDirectory(root));
  assert.equal(existsSync(join(hooks, "commit-msg")), true);
  const hook = readFileSync(join(hooks, "commit-msg"), "utf8");
  assert.match(hook, /commit-agent-attribution\.mjs/);
  assert.match(hook, /original-hooks/);
  const configured = spawnSync("git", ["config", "--worktree", "--get", "core.hooksPath"], {
    cwd: root,
    encoding: "utf8",
  });
  assert.equal(configured.stdout.trim(), hooks);

  ensureCommitMessageHook(root);
  assert.match(readFileSync(join(hooks, "commit-msg"), "utf8"), /original-hooks/);
});

test("a captured Codex commit is rewritten end to end", (t) => {
  const root = temporaryDirectory(t);
  const scripts = join(root, "scripts", "agents");
  const sourceScripts = dirname(fileURLToPath(import.meta.url));
  mkdirSync(scripts, { recursive: true });
  for (const file of ["agent-attribution.mjs", "capture-agent-attribution.mjs", "commit-agent-attribution.mjs"]) {
    copyFileSync(join(sourceScripts, file), join(scripts, file));
  }
  for (const args of [
    ["init", "-q"],
    ["config", "user.name", "Attribution Test"],
    ["config", "user.email", "test@example.com"],
  ]) {
    const result = spawnSync("git", args, { cwd: root, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
  }
  writeFileSync(join(root, "change.txt"), "change\n");
  const added = spawnSync("git", ["add", "change.txt"], { cwd: root, encoding: "utf8" });
  assert.equal(added.status, 0, added.stderr);

  const transcript = join(root, "rollout.jsonl");
  jsonLines(transcript, [
    { type: "turn_context", payload: { model: "gpt-5.6-sol", effort: "xhigh" } },
  ]);
  const env = { ...process.env, CODEX_THREAD_ID: "session-1" };
  const capture = spawnSync(process.execPath, [join(scripts, "capture-agent-attribution.mjs"), "codex"], {
    cwd: root,
    env,
    encoding: "utf8",
    input: JSON.stringify({
      session_id: "session-1",
      cwd: root,
      model: "gpt-5.6-sol",
      transcript_path: transcript,
      tool_input: { command: "git commit -m test" },
    }),
  });
  assert.equal(capture.status, 0, capture.stderr);

  const committed = spawnSync("git", ["commit", "-m", "Test commit\n\nAgent: gpt-5 high"], {
    cwd: root,
    env,
    encoding: "utf8",
  });
  assert.equal(committed.status, 0, committed.stderr);
  const message = spawnSync("git", ["show", "-s", "--format=%B"], { cwd: root, encoding: "utf8" });
  assert.equal(message.status, 0, message.stderr);
  assert.equal(message.stdout.trimEnd(), "Test commit\n\nAgent: gpt-5.6-sol xhigh");
});
