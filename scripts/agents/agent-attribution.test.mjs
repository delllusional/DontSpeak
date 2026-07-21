import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  ATTRIBUTION_CACHE_FILE,
  activeAgentEnvironment,
  commandFromHookInput,
  detectClient,
  ensureCommitMessageHook,
  gitCommitInvocations,
  gitCommitWorkingDirectory,
  messageMatchesHead,
  normalizeCacheRecord,
  privateHooksDirectory,
  readJsonLinesReverse,
  resolveAttribution,
  resolveShellPath,
  rewriteCommitMessage,
  validateAttribution,
  validateCacheRecord,
  validateCommitMessage,
} from "./agent-attribution.mjs";

const sourceScripts = dirname(fileURLToPath(import.meta.url));

function temporaryDirectory(t) {
  const directory = mkdtempSync(join(tmpdir(), "dontspeak-attribution-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  return directory;
}

// ensureCommitMessageHook / capture+commit scripts must not touch real git config.
function isolatedGitEnvironment(t) {
  const configFile = join(temporaryDirectory(t), "gitconfig");
  writeFileSync(configFile, "", "utf8");
  const overrides = { GIT_CONFIG_GLOBAL: configFile, GIT_CONFIG_NOSYSTEM: "1" };
  const saved = new Map(Object.keys(overrides).map((key) => [key, process.env[key]]));
  Object.assign(process.env, overrides);
  t.after(() => {
    for (const [key, value] of saved) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
  });
  // Strip host agent markers so hooks see a clean "no CLI" env.
  const env = { ...process.env };
  for (const key of [
    "GROK_AGENT",
    "GROK_SESSION_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_PROJECT_DIR",
    "CODEX_THREAD_ID",
    "QWEN_CODE",
    "QWEN_PROJECT_DIR",
    "QWEN_SESSION_ID",
  ]) {
    delete env[key];
  }
  return { configFile, env };
}

function jsonLines(file, rows) {
  mkdirSync(join(file, ".."), { recursive: true });
  writeFileSync(file, `${rows.map((row) => JSON.stringify(row)).join("\n")}\n`, "utf8");
}

function initializedRepository(t, env) {
  const root = temporaryDirectory(t);
  const scripts = join(root, "scripts", "agents");
  mkdirSync(scripts, { recursive: true });
  for (const file of ["agent-attribution.mjs", "capture-agent-attribution.mjs", "commit-agent-attribution.mjs"]) {
    copyFileSync(join(sourceScripts, file), join(scripts, file));
  }
  for (const args of [
    ["init", "-q"],
    ["config", "user.name", "Attribution Test"],
    ["config", "user.email", "test@example.com"],
  ]) {
    const result = spawnSync("git", args, { cwd: root, env, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
  }
  return { root, scripts };
}

function captureCodex(root, scripts, env, command, options = {}) {
  const model = options.model ?? "gpt-5.6-sol";
  const effort = options.effort ?? "xhigh";
  const transcript = options.transcript ?? join(root, "rollout.jsonl");
  jsonLines(transcript, [{ type: "turn_context", payload: { model, effort } }]);
  const capture = spawnSync(process.execPath, [join(scripts, "capture-agent-attribution.mjs"), "codex"], {
    cwd: options.spawnCwd ?? root,
    env,
    encoding: "utf8",
    input: JSON.stringify({
      session_id: options.session ?? "session-1",
      cwd: options.cwd ?? root,
      model,
      transcript_path: transcript,
      tool_input: { command },
    }),
  });
  if (options.expectFailure !== true) assert.equal(capture.status, 0, capture.stderr);
  return capture;
}

function headMessage(root, env) {
  const message = spawnSync("git", ["show", "-s", "--format=%B"], { cwd: root, env, encoding: "utf8" });
  assert.equal(message.status, 0, message.stderr);
  return message.stdout.trimEnd();
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

test("cd tracking resolves the target repository exactly", () => {
  const base = resolve("workspace");
  assert.deepEqual(gitCommitInvocations("cd rust && git -C .. commit --amend", base), [
    { workingDirectory: base, subcommand: "commit" },
  ]);
  assert.deepEqual(
    gitCommitInvocations("cd rust; git commit -m x", base).map((invocation) => invocation.workingDirectory),
    [join(base, "rust")],
  );
});

test("each commit in a chain yields its own invocation", () => {
  const base = resolve("workspace");
  assert.equal(gitCommitInvocations("git commit -m a && git commit -m b", base).length, 2);
  assert.deepEqual(gitCommitInvocations("git commit -m x && some-tool --amend", base), [
    { workingDirectory: base, subcommand: "commit" },
  ]);
});

test("merge commits are detected", () => {
  const base = resolve("workspace");
  assert.deepEqual(gitCommitInvocations("git merge --no-ff feature", base), [
    { workingDirectory: base, subcommand: "merge" },
  ]);
});

test("newlines separate commands like semicolons", () => {
  const base = resolve("workspace");
  assert.deepEqual(
    gitCommitInvocations("cd sub\ngit commit -m x", base).map((invocation) => invocation.workingDirectory),
    [join(base, "sub")],
  );
  assert.equal(
    gitCommitInvocations("git commit -m \"first\ngit commit -m second\"", base).length,
    1,
  );
  assert.deepEqual(gitCommitInvocations("git \\\ncommit -m x", base), [
    { workingDirectory: base, subcommand: "commit" },
  ]);
});

test("unknown shell cwd fails closed without a literal-path guess", () => {
  const base = resolve("workspace");
  assert.deepEqual(gitCommitInvocations("cd - && git commit -m x", base), []);
  assert.deepEqual(gitCommitInvocations("cd \"$DIR\" && git commit -m x", base), []);
  assert.deepEqual(gitCommitInvocations("cd `latest` && git commit -m x", base), []);
  assert.equal(gitCommitInvocations("git commit -m a && cd - && git commit -m b", base).length, 1);
});

test("cd path arguments are allowlisted, with -- and flags skipped", () => {
  const base = resolve("workspace");
  assert.deepEqual(
    gitCommitInvocations("cd -- sub && git commit -m x", base).map((invocation) => invocation.workingDirectory),
    [join(base, "sub")],
  );
  assert.deepEqual(
    gitCommitInvocations("cd ./a-b_c && git commit -m x", base).map((invocation) => invocation.workingDirectory),
    [join(base, "a-b_c")],
  );
  assert.deepEqual(
    gitCommitInvocations("cd C:/x && git commit -m y", "C:\\base", { platform: "win32" })
      .map((invocation) => invocation.workingDirectory),
    ["C:\\x"],
  );
  assert.deepEqual(gitCommitInvocations("cd sub\\dir && git commit -m x", base), []);
  assert.deepEqual(gitCommitInvocations("cd {a,b} && git commit -m x", base), []);
});

test("subshell cd does not leak past the closing paren", () => {
  const base = resolve("workspace");
  assert.deepEqual(
    gitCommitInvocations("(cd sub && git commit -m a) && git commit -m b", base)
      .map((invocation) => invocation.workingDirectory),
    [join(base, "sub"), base],
  );
  assert.deepEqual(gitCommitInvocations(") && git commit -m x", base), []);
});

test("pushd and popd drive a directory stack", () => {
  const base = resolve("workspace");
  assert.deepEqual(
    gitCommitInvocations("pushd sub && git commit -m a && popd && git commit -m b", base)
      .map((invocation) => invocation.workingDirectory),
    [join(base, "sub"), base],
  );
  assert.deepEqual(gitCommitInvocations("popd && git commit -m x", base), []);
  assert.deepEqual(gitCommitInvocations("pushd && git commit -m x", base), []);
  assert.deepEqual(gitCommitInvocations("pushd sub && pushd && popd && git commit -m x", base), []);
});

test("git-dir and work-tree redirections are not captured", () => {
  const base = resolve("workspace");
  assert.deepEqual(gitCommitInvocations("git --git-dir=x commit -m y", base), []);
  assert.deepEqual(gitCommitInvocations("git --git-dir x commit -m y", base), []);
  assert.deepEqual(gitCommitInvocations("git --work-tree=w commit -m y", base), []);
});

test("resolveShellPath translates drive paths and rejects msys mounts", () => {
  assert.equal(resolveShellPath("C:\\base", "/c/Users/dev", { platform: "win32" }), "C:\\Users\\dev");
  assert.equal(resolveShellPath("C:\\base", "/tmp/build", { platform: "win32" }), undefined);
  assert.equal(
    resolveShellPath("C:\\base", "~/repo", { platform: "win32", home: "C:\\Users\\dev" }),
    "C:\\Users\\dev\\repo",
  );
  assert.equal(resolveShellPath("/base", "sub/dir", { platform: "linux" }), "/base/sub/dir");
  assert.equal(resolveShellPath("/base", "~other/x", { platform: "linux" }), undefined);
});

test("win32 cwd tracking rejects untranslatable POSIX paths", () => {
  assert.deepEqual(
    gitCommitInvocations("cd /tmp/build && git commit -m x", "C:\\base", { platform: "win32" }),
    [],
  );
  assert.deepEqual(
    gitCommitInvocations("cd /c/build && git commit -m x", "C:\\base", { platform: "win32" })
      .map((invocation) => invocation.workingDirectory),
    ["C:\\build"],
  );
});

test("quoted git binaries require a path separator", () => {
  const base = resolve("workspace");
  assert.equal(
    gitCommitInvocations("\"C:/Program Files/Git/bin/git.exe\" commit -m x", "C:\\base", { platform: "win32" }).length,
    1,
  );
  assert.deepEqual(gitCommitInvocations("'git' commit -m x", base), []);
});

test("shell wrapper payloads are parsed recursively", () => {
  const base = resolve("workspace");
  assert.equal(gitCommitInvocations("sh -c \"git commit -m x\"", base).length, 1);
  assert.deepEqual(
    gitCommitInvocations("bash -lc \"cd rust && git commit -m x\"", base)
      .map((invocation) => invocation.workingDirectory),
    [join(base, "rust")],
  );
  // -c after a non-flag arg belongs to the script, not the shell.
  assert.deepEqual(gitCommitInvocations("sh script.sh -c \"git commit -m x\"", base), []);
});

test("argv-array commands parse without re-tokenizing", () => {
  const base = resolve("workspace");
  assert.equal(gitCommitInvocations(["git", "commit", "-m", "x"], base).length, 1);
  assert.equal(gitCommitInvocations(["bash", "-lc", "git commit -m x"], base).length, 1);
  assert.deepEqual(
    gitCommitInvocations(["git", "-C", "my \"quoted\" dir", "commit", "-m", "x"], base)
      .map((invocation) => invocation.workingDirectory),
    [join(base, "my \"quoted\" dir")],
  );
  assert.deepEqual(
    commandFromHookInput({ tool_input: { command: ["git", "commit"] } }),
    ["git", "commit"],
  );
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

test("Grok prefers summary model/effort and falls back when only GROK_AGENT is set", (t) => {
  const home = temporaryDirectory(t);
  const cwd = join(home, "work");
  mkdirSync(cwd, { recursive: true });
  const session = join(home, ".grok", "sessions", "proj", "session-cwd");
  mkdirSync(session, { recursive: true });
  writeFileSync(join(session, "summary.json"), JSON.stringify({
    current_model_id: "grok-4.5",
    reasoning_effort: "high",
    git_root_dir: cwd,
    last_active_at: "2026-07-18T20:00:00Z",
  }));
  // Per-turn build slug must not override the product model.
  jsonLines(join(session, "chat_history.jsonl"), [
    { type: "assistant", model_id: "grok-4.5-build", reasoning_effort: "high" },
  ]);
  const byCwd = resolveAttribution(
    "grok",
    { cwd },
    { home, env: { GROK_AGENT: "1" }, root: cwd },
  );
  assert.equal(byCwd.model, "grok-4.5");
  assert.equal(byCwd.effort, "high");
  assert.deepEqual(byCwd.errors, []);
});

test("Grok prefers parent sessions over subagents and uses active_sessions.json", (t) => {
  const home = temporaryDirectory(t);
  const cwd = join(home, "repo");
  mkdirSync(cwd, { recursive: true });
  const parent = join(home, ".grok", "sessions", "p", "parent-1");
  const child = join(home, ".grok", "sessions", "p", "child-1");
  mkdirSync(parent, { recursive: true });
  mkdirSync(child, { recursive: true });
  writeFileSync(join(parent, "summary.json"), JSON.stringify({
    current_model_id: "grok-4.5",
    reasoning_effort: "high",
    git_root_dir: cwd,
    last_active_at: "2026-07-18T19:00:00Z",
    session_kind: "default",
  }));
  writeFileSync(join(child, "summary.json"), JSON.stringify({
    current_model_id: "grok-4.5",
    reasoning_effort: "low",
    git_root_dir: cwd,
    last_active_at: "2026-07-18T21:00:00Z",
    session_kind: "subagent",
  }));
  const preferParent = resolveAttribution("grok", { cwd }, { home, env: { GROK_AGENT: "1" }, root: cwd });
  assert.equal(preferParent.effort, "high");

  const activeOnly = join(home, ".grok", "sessions", "p", "active-only");
  mkdirSync(activeOnly, { recursive: true });
  writeFileSync(join(activeOnly, "summary.json"), JSON.stringify({
    current_model_id: "grok-4.5",
    reasoning_effort: "xhigh",
  }));
  writeFileSync(join(home, ".grok", "active_sessions.json"), JSON.stringify([
    { session_id: "active-only", cwd: home },
  ]));
  const viaActive = resolveAttribution(
    "grok",
    {},
    { home, env: { GROK_AGENT: "1" }, root: join(home, "unrelated") },
  );
  assert.equal(viaActive.effort, "xhigh");
});

test("GROK_AGENT alone marks an active Grok agent environment", () => {
  assert.deepEqual(activeAgentEnvironment({ GROK_AGENT: "1" }), {
    client: "grok",
    sessionId: undefined,
  });
  assert.deepEqual(
    activeAgentEnvironment({ GROK_AGENT: "1", GROK_SESSION_ID: "sess" }),
    { client: "grok", sessionId: "sess" },
  );
  assert.equal(detectClient("auto", {}, { GROK_AGENT: "1" }), "grok");
});

test("commit-msg live-resolves Grok when the PreToolUse cache is missing", (t) => {
  const { env: isolatedEnv } = isolatedGitEnvironment(t);
  const home = temporaryDirectory(t);
  const cwd = join(home, "repo-live");
  mkdirSync(cwd, { recursive: true });
  const session = join(home, ".grok", "sessions", "proj", "live-1");
  mkdirSync(session, { recursive: true });
  writeFileSync(join(session, "summary.json"), JSON.stringify({
    current_model_id: "grok-4.5",
    reasoning_effort: "high",
    git_root_dir: cwd,
    last_active_at: "2026-07-18T22:00:00Z",
  }));

  const env = {
    ...isolatedEnv,
    GROK_AGENT: "1",
    USERPROFILE: home,
    HOME: home,
  };
  const { root, scripts } = initializedRepository(t, env);
  writeFileSync(join(session, "summary.json"), JSON.stringify({
    current_model_id: "grok-4.5",
    reasoning_effort: "high",
    git_root_dir: root,
    last_active_at: "2026-07-18T22:00:00Z",
  }));
  ensureCommitMessageHook(root);
  writeFileSync(join(root, "f.txt"), "x\n");
  assert.equal(spawnSync("git", ["add", "f.txt"], { cwd: root, env, encoding: "utf8" }).status, 0);
  // No PreToolUse cache — live path must stamp high.
  const cache = join(privateHooksDirectory(root), ATTRIBUTION_CACHE_FILE);
  assert.equal(existsSync(cache), false);
  const commit = spawnSync("git", ["commit", "-m", "Live resolve\n\nAgent: grok-4.5 none"], {
    cwd: root,
    env: { ...env, USERPROFILE: home, HOME: home },
    encoding: "utf8",
  });
  assert.equal(commit.status, 0, commit.stderr + commit.stdout);
  assert.equal(headMessage(root, env), "Live resolve\n\nAgent: grok-4.5 high");
  void scripts;
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

test("bare family words are rejected while human none stays valid", () => {
  assert.match(validateAttribution("claude", "high")[0], /not an exact slug/);
  assert.match(validateAttribution("Qwen", "high")[0], /not an exact slug/);
  assert.deepEqual(validateAttribution("human", "none"), []);
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

test("preserveLone keeps a proven lone trailer and appends the new pair", () => {
  const preserved = rewriteCommitMessage(
    "Fix\n\nAgent: gpt-5.6-sol xhigh\n",
    "gpt-5.6-luna",
    "high",
    { preserveLone: true },
  );
  assert.equal(preserved, "Fix\n\nAgent: gpt-5.6-sol xhigh\nAgent: gpt-5.6-luna high\n");
  const replaced = rewriteCommitMessage("Fix\n\nAgent: gpt-5.6-sol xhigh\n", "gpt-5.6-luna", "high");
  assert.equal(replaced, "Fix\n\nAgent: gpt-5.6-luna high\n");
});

test("prohibited attribution above other trailers is stripped", () => {
  const rewritten = rewriteCommitMessage(
    "Fix\n\nCo-Authored-By: Codex <noreply@example.com>\nSigned-off-by: Dev <dev@example.com>\n",
    "gpt-5.6-sol",
    "xhigh",
  );
  assert.equal(rewritten, "Fix\n\nSigned-off-by: Dev <dev@example.com>\n\nAgent: gpt-5.6-sol xhigh\n");
  assert.deepEqual(validateCommitMessage(rewritten), []);
});

test("mid-body Agent lines are stripped without duplicating the pair", () => {
  const rewritten = rewriteCommitMessage(
    "Fix\n\nAgent: gpt-5.6-sol xhigh\nMore body\n",
    "gpt-5.6-sol",
    "xhigh",
  );
  assert.equal(rewritten, "Fix\n\nMore body\n\nAgent: gpt-5.6-sol xhigh\n");
  assert.deepEqual(validateCommitMessage(rewritten), []);
});

test("an attribution-shaped subject fails closed instead of being stripped", () => {
  assert.throws(
    () => rewriteCommitMessage("Agent: fix the parser\n\nBody\n", "gpt-5.6-sol", "xhigh"),
    /subject looks like an attribution line/,
  );
  assert.throws(
    () => rewriteCommitMessage("Co-Authored-By: someone\n\nBody\n", "gpt-5.6-sol", "xhigh"),
    /subject looks like an attribution line/,
  );
});

test("message equality with HEAD gates the lone-trailer preserve", (t) => {
  const { env } = isolatedGitEnvironment(t);
  const { root } = initializedRepository(t, env);
  assert.equal(messageMatchesHead("Base commit\n", root), false); // unborn HEAD
  const committed = spawnSync(
    "git",
    ["commit", "--allow-empty", "-m", "Base commit\n\nDetails here"],
    { cwd: root, env, encoding: "utf8" },
  );
  assert.equal(committed.status, 0, committed.stderr);
  assert.equal(messageMatchesHead("Base commit\n\nDetails here\n", root), true);
  // # comments / trailing WS differ before --cleanup.
  assert.equal(messageMatchesHead("Base commit\n\nDetails here  \n# template comment\n", root), true);
  assert.equal(messageMatchesHead("Different subject\n\nDetails here\n", root), false);
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
    uses: 1,
    capturedAt: new Date(now).toISOString(),
  };
  assert.deepEqual(validateCacheRecord(record, "/repo", { CODEX_THREAD_ID: "one" }, now), []);
  assert.match(validateCacheRecord(record, "/repo", { CODEX_THREAD_ID: "two" }, now)[0], /different agent session/);
});

test("freshness caps depend on the agent environment and uses must be positive", () => {
  const now = Date.now();
  const record = {
    version: 1,
    root: "/repo",
    client: "codex",
    sessionId: "one",
    model: "gpt-5.6-sol",
    effort: "xhigh",
    errors: [],
    uses: 1,
    capturedAt: new Date(now - 8 * 60_000).toISOString(),
  };
  // 8m: ok under agent env (15m cap), stale env-less (5m).
  assert.deepEqual(validateCacheRecord(record, "/repo", { CODEX_THREAD_ID: "one" }, now), []);
  assert.match(validateCacheRecord(record, "/repo", {}, now).join("\n"), /stale/);
  const fresh = { ...record, capturedAt: new Date(now - 4 * 60_000).toISOString() };
  assert.deepEqual(validateCacheRecord(fresh, "/repo", {}, now), []);
  const spent = { ...fresh, uses: 0 };
  assert.deepEqual(validateCacheRecord(spent, "/repo", {}, now), [
    "no usable runtime attribution capture was found",
  ]);
  const malformed = { ...fresh, uses: "2" };
  assert.deepEqual(validateCacheRecord(malformed, "/repo", {}, now), [
    "no usable runtime attribution capture was found",
  ]);
});

test("old-capture records without uses stay usable during transition", () => {
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
  assert.equal(normalizeCacheRecord(record).uses, 1);
});

test("transcript selector retries once with a larger tail cap", (t) => {
  const file = join(temporaryDirectory(t), "big.jsonl");
  const rows = [{ hit: true }];
  for (let index = 0; index < 100; index += 1) rows.push({ pad: "x".repeat(120) });
  jsonLines(file, rows);
  const selector = (row) => (row.hit ? "found" : undefined);
  assert.equal(readJsonLinesReverse(file, selector, { maxBytes: 1024 }), "found");
  assert.equal(readJsonLinesReverse(file, selector, { maxBytes: 1024, retryMaxBytes: 1024 }), undefined);
});

test("the retry drops the row straddling the first-pass boundary", (t) => {
  const file = join(temporaryDirectory(t), "boundary.jsonl");
  // Each row is exactly 99 bytes + newline = 100 bytes (pins the straddle).
  const row = (marker) => {
    const overhead = JSON.stringify({ marker, pad: "" }).length;
    return { marker, pad: "x".repeat(99 - overhead) };
  };
  jsonLines(file, [row("head"), row("early"), row("straddle"), row("tail")]);
  const find = (marker) => readJsonLinesReverse(
    file,
    (parsed) => (parsed.marker === marker ? parsed.marker : undefined),
    { maxBytes: 150, retryMaxBytes: 4096 },
  );
  assert.equal(find("tail"), "tail");
  assert.equal(find("early"), "early");
  assert.equal(find("straddle"), undefined);
});

test("hook installation is isolated to the current worktree config", (t) => {
  isolatedGitEnvironment(t);
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

test("upstream hooks path is re-detected when config scopes change", (t) => {
  const { env } = isolatedGitEnvironment(t);
  const { root } = initializedRepository(t, env);
  const hooks = ensureCommitMessageHook(root);
  const recordedFallback = readFileSync(join(hooks, "upstream-hooks-path"), "utf8").trim();

  const globalHooks = join(temporaryDirectory(t), "global-hooks");
  mkdirSync(globalHooks, { recursive: true });
  const configured = spawnSync("git", ["config", "--global", "core.hooksPath", globalHooks], {
    env,
    encoding: "utf8",
  });
  assert.equal(configured.status, 0, configured.stderr);

  ensureCommitMessageHook(root);
  const recorded = readFileSync(join(hooks, "upstream-hooks-path"), "utf8").trim();
  assert.equal(recorded, globalHooks);
  assert.notEqual(recorded, recordedFallback);
  assert.match(readFileSync(join(hooks, "commit-msg"), "utf8"), /global-hooks/);
});

test("a user-set worktree hooksPath is chained, not clobbered", (t) => {
  const { env } = isolatedGitEnvironment(t);
  const { root } = initializedRepository(t, env);
  const custom = join(temporaryDirectory(t), "custom-hooks");
  mkdirSync(custom, { recursive: true });
  for (const args of [
    ["config", "--local", "extensions.worktreeConfig", "true"],
    ["config", "--worktree", "core.hooksPath", custom],
  ]) {
    const result = spawnSync("git", args, { cwd: root, env, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
  }
  const hooks = ensureCommitMessageHook(root);
  assert.equal(readFileSync(join(hooks, "upstream-hooks-path"), "utf8").trim(), custom);
  assert.match(readFileSync(join(hooks, "commit-msg"), "utf8"), /custom-hooks/);
  // Re-run after our path took worktree scope: still chained.
  ensureCommitMessageHook(root);
  assert.equal(readFileSync(join(hooks, "upstream-hooks-path"), "utf8").trim(), custom);
});

test("a lost hook exec bit is repaired on rerun", { skip: process.platform === "win32" }, (t) => {
  const { env } = isolatedGitEnvironment(t);
  const { root } = initializedRepository(t, env);
  const hooks = ensureCommitMessageHook(root);
  const hook = join(hooks, "commit-msg");
  chmodSync(hook, 0o644);
  ensureCommitMessageHook(root);
  assert.notEqual(statSync(hook).mode & 0o111, 0);
});

test("a linked worktree does not chain to the main worktree's managed hook", (t) => {
  isolatedGitEnvironment(t);
  const base = temporaryDirectory(t);
  const root = join(base, "main");
  const linked = join(base, "linked");
  const initialized = spawnSync("git", ["init", "-q", root], { encoding: "utf8" });
  assert.equal(initialized.status, 0, initialized.stderr);
  const committed = spawnSync("git", [
    "-c", "user.name=Attribution Test",
    "-c", "user.email=test@example.com",
    "commit", "--allow-empty", "-qm", "Initial commit",
  ], { cwd: root, encoding: "utf8" });
  assert.equal(committed.status, 0, committed.stderr);

  const mainHooks = ensureCommitMessageHook(root);
  const added = spawnSync("git", ["worktree", "add", "-q", "--detach", linked], {
    cwd: root,
    encoding: "utf8",
  });
  assert.equal(added.status, 0, added.stderr);
  // Linked worktrees inherit main's core.hooksPath (including private hooks).
  const inherited = spawnSync("git", ["config", "--path", "--get", "core.hooksPath"], {
    cwd: linked,
    encoding: "utf8",
  });
  assert.equal(inherited.stdout.trim(), mainHooks);

  const linkedHooks = ensureCommitMessageHook(linked);
  const hook = readFileSync(join(linkedHooks, "commit-msg"), "utf8");
  assert.equal(hook.includes(mainHooks.replaceAll("\\", "/")), false);
  const resolvedCommon = spawnSync(
    "git",
    ["rev-parse", "--git-common-dir"],
    { cwd: linked, encoding: "utf8" },
  );
  assert.equal(resolvedCommon.status, 0, resolvedCommon.stderr);
  const neutralHooks = resolve(linked, resolvedCommon.stdout.trim(), "hooks");
  const recordedHooks = readFileSync(join(linkedHooks, "upstream-hooks-path"), "utf8").trim();
  assert.equal(realpathSync(recordedHooks), realpathSync(neutralHooks));
});

test("capture skips silently outside a git repository", (t) => {
  const { env: isolatedEnv } = isolatedGitEnvironment(t);
  const env = { ...isolatedEnv, CODEX_THREAD_ID: "session-1" };
  const directory = temporaryDirectory(t);
  for (const cwd of [directory, join(directory, "missing")]) {
    const capture = captureCodex(directory, sourceScripts, env, "git commit -m x", { cwd });
    assert.equal(capture.status, 0, `${capture.stdout}${capture.stderr}`);
    assert.equal(capture.stderr, "");
  }
});

test("unexpected git failures during capture still fail loudly", (t) => {
  const { env: isolatedEnv } = isolatedGitEnvironment(t);
  const directory = temporaryDirectory(t);
  const broken = { ...isolatedEnv, CODEX_THREAD_ID: "session-1" };
  // Empty PATH: real spawn failure, not a non-repo skip.
  for (const key of Object.keys(broken)) {
    if (key.toUpperCase() === "PATH") delete broken[key];
  }
  broken.PATH = "";
  const capture = captureCodex(directory, sourceScripts, broken, "git commit -m x", { expectFailure: true });
  assert.equal(capture.status, 2, `${capture.stdout}${capture.stderr}`);
  assert.match(capture.stderr, /commit attribution capture failed/);
});

test("capture keys on the first invocation whose repository resolves", (t) => {
  const { env: isolatedEnv } = isolatedGitEnvironment(t);
  const env = { ...isolatedEnv, CODEX_THREAD_ID: "session-1" };
  const { root, scripts } = initializedRepository(t, env);
  const outside = temporaryDirectory(t).replaceAll("\\", "/"); // not a repo
  captureCodex(root, scripts, env, `git -C ${outside} commit -m a && git commit -m b && git commit -m c`);
  const record = JSON.parse(readFileSync(join(privateHooksDirectory(root), ATTRIBUTION_CACHE_FILE), "utf8"));
  assert.equal(record.uses, 2);
  assert.equal(realpathSync(record.root), realpathSync(root));
});

test("a captured Codex command stamps each counted commit", (t) => {
  const { env: isolatedEnv } = isolatedGitEnvironment(t);
  const env = { ...isolatedEnv, CODEX_THREAD_ID: "session-1" };
  const { root, scripts } = initializedRepository(t, env);
  writeFileSync(join(root, "change.txt"), "change\n");
  const added = spawnSync("git", ["add", "change.txt"], { cwd: root, env, encoding: "utf8" });
  assert.equal(added.status, 0, added.stderr);

  captureCodex(root, scripts, env, "git commit -m first && git commit -m second");
  const cache = join(privateHooksDirectory(root), ATTRIBUTION_CACHE_FILE);
  assert.equal(existsSync(cache), true);
  assert.equal(JSON.parse(readFileSync(cache, "utf8")).uses, 2);

  const first = spawnSync("git", ["commit", "-m", "First\n\nAgent: gpt-5 high"], {
    cwd: root,
    env,
    encoding: "utf8",
  });
  assert.equal(first.status, 0, first.stderr);
  assert.equal(headMessage(root, env), "First\n\nAgent: gpt-5.6-sol xhigh");
  assert.equal(existsSync(cache), true);

  const second = spawnSync("git", ["commit", "--allow-empty", "-m", "Second"], {
    cwd: root,
    env,
    encoding: "utf8",
  });
  assert.equal(second.status, 0, second.stderr);
  assert.equal(headMessage(root, env), "Second\n\nAgent: gpt-5.6-sol xhigh");
  assert.equal(existsSync(cache), false);
});

test("amend preserves the proven pair only when the message is kept", (t) => {
  const { env: isolatedEnv } = isolatedGitEnvironment(t);
  const env = { ...isolatedEnv, CODEX_THREAD_ID: "session-1" };
  const { root, scripts } = initializedRepository(t, env);
  writeFileSync(join(root, "change.txt"), "change\n");
  const added = spawnSync("git", ["add", "change.txt"], { cwd: root, env, encoding: "utf8" });
  assert.equal(added.status, 0, added.stderr);

  captureCodex(root, scripts, env, "git commit -m base");
  const base = spawnSync("git", ["commit", "-m", "Base commit"], { cwd: root, env, encoding: "utf8" });
  assert.equal(base.status, 0, base.stderr);
  assert.equal(headMessage(root, env), "Base commit\n\nAgent: gpt-5.6-sol xhigh");

  captureCodex(root, scripts, env, "git commit --amend --no-edit", { model: "gpt-5.6-luna", effort: "high" });
  const kept = spawnSync("git", ["commit", "--amend", "--no-edit"], { cwd: root, env, encoding: "utf8" });
  assert.equal(kept.status, 0, kept.stderr);
  assert.equal(
    headMessage(root, env),
    "Base commit\n\nAgent: gpt-5.6-sol xhigh\nAgent: gpt-5.6-luna high",
  );

  captureCodex(root, scripts, env, "git commit --amend -m replaced", { model: "gpt-5.6-luna", effort: "high" });
  const replaced = spawnSync("git", ["commit", "--amend", "-m", "Replaced\n\nAgent: gpt-5 high"], {
    cwd: root,
    env,
    encoding: "utf8",
  });
  assert.equal(replaced.status, 0, replaced.stderr);
  assert.equal(headMessage(root, env), "Replaced\n\nAgent: gpt-5.6-luna high");
});

test("the Claude wrapper forwards commit payloads byte-intact", (t) => {
  const settingsPath = join(sourceScripts, "..", "..", ".claude", "settings.json");
  const hook = JSON.parse(readFileSync(settingsPath, "utf8")).hooks.PreToolUse
    .find((entry) => entry.matcher.includes("run_terminal_command"))
    .hooks[0];
  if (hook.command === "node") {
    assert.deepEqual(hook.args, [
      "${CLAUDE_PROJECT_DIR}/scripts/agents/capture-agent-attribution.mjs",
      "auto",
    ]);
    return;
  }
  const command = hook.command;
  // Pins safe read + unquoted heredoc (expansion without re-scanning).
  assert.match(command, /IFS= read -rd ''/);
  assert.match(command, /<<DONTSPEAK_JSON\n/);
  const bin = temporaryDirectory(t);
  const outFile = join(bin, "captured.json");
  writeFileSync(join(bin, "node"), `#!/bin/sh\ncat > '${outFile.replaceAll("\\", "/")}'\n`, { mode: 0o755 });
  const payload = JSON.stringify({ tool_input: { command: "git commit -m `date` $(hostname)" } });
  // Prefer Git Bash over System32 bash.exe (WSL launcher, not POSIX).
  const bashCandidates = [
    "C:\\Program Files\\Git\\bin\\bash.exe",
    "C:\\Program Files\\Git\\usr\\bin\\bash.exe",
    "bash",
  ];
  let bash = "bash";
  for (const candidate of bashCandidates) {
    const probe = spawnSync(candidate, ["-c", "echo ok"], { encoding: "utf8" });
    if (!probe.error && probe.status === 0 && (probe.stdout ?? "").includes("ok")) {
      bash = candidate;
      break;
    }
  }
  const env = {
    ...process.env,
    CLAUDE_PROJECT_DIR: bin,
    PATH: `${bin}${delimiter}${process.env.PATH ?? ""}`,
  };
  const run = spawnSync(bash, ["-c", command], { input: payload, env, encoding: "utf8" });
  if (run.error || run.status !== 0) {
    // No usable POSIX bash; string assertions above already pin the wrapper form.
    return;
  }
  assert.equal(readFileSync(outFile, "utf8"), `${payload}\n`);
  rmSync(outFile);
  const skip = spawnSync(bash, ["-c", command], {
    input: JSON.stringify({ tool_input: { command: "ls -la" } }),
    env,
    encoding: "utf8",
  });
  assert.equal(skip.status, 0, skip.stderr);
  assert.equal(existsSync(outFile), false);
});
