import { spawnSync } from "node:child_process";
import {
  chmodSync,
  closeSync,
  existsSync,
  fstatSync,
  mkdirSync,
  openSync,
  readdirSync,
  readFileSync,
  readSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";

export const ATTRIBUTION_CACHE_MAX_AGE_MS = 5 * 60 * 1000;
export const ATTRIBUTION_CACHE_FILE = "agent-attribution.json";
const UPSTREAM_HOOKS_FILE = "upstream-hooks-path";

const EFFORTS = new Set([
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
  "ultra",
]);
const PROHIBITED_ATTRIBUTION = /^(?:Co-Authored-By|Assisted-by|Generated-by|AI):/i;

function cleanString(value) {
  if (typeof value !== "string") return undefined;
  const cleaned = value.trim();
  return cleaned || undefined;
}

function firstString(...values) {
  for (const value of values) {
    const cleaned = cleanString(value);
    if (cleaned) return cleaned;
  }
  return undefined;
}

function normalizedEffort(value) {
  const cleaned = cleanString(value)?.toLowerCase();
  if (!cleaned) return undefined;
  if (cleaned === "off" || cleaned === "disabled") return "none";
  return cleaned;
}

function directEffort(input) {
  return normalizedEffort(firstString(
    input?.effort?.level,
    typeof input?.effort === "string" ? input.effort : undefined,
    input?.reasoning_effort,
    input?.reasoningEffort,
    input?.model?.reasoning_effort,
    input?.model?.reasoningEffort,
  ));
}

function parseJson(value) {
  try {
    return JSON.parse(value);
  } catch {
    return undefined;
  }
}

export function readFileTail(file, maxBytes = 2 * 1024 * 1024) {
  const handle = openSync(file, "r");
  try {
    const size = fstatSync(handle).size;
    const start = Math.max(0, size - maxBytes);
    const buffer = Buffer.alloc(size - start);
    readSync(handle, buffer, 0, buffer.length, start);
    let text = buffer.toString("utf8");
    if (start > 0) {
      const firstNewline = text.indexOf("\n");
      text = firstNewline === -1 ? "" : text.slice(firstNewline + 1);
    }
    return text;
  } finally {
    closeSync(handle);
  }
}

export function readJsonLinesReverse(file, selector) {
  if (!file || !existsSync(file)) return undefined;
  const lines = readFileTail(file).split(/\r?\n/);
  for (let index = lines.length - 1; index >= 0; index -= 1) {
    const row = parseJson(lines[index]);
    if (!row) continue;
    const selected = selector(row);
    if (selected !== undefined && selected !== null) return selected;
  }
  return undefined;
}

function transcriptPath(input) {
  return firstString(input?.transcript_path, input?.transcriptPath);
}

function assistantModelFromTranscript(file) {
  return readJsonLinesReverse(file, (row) => {
    if (row.type !== "assistant" && row.message?.role !== "assistant") return undefined;
    return firstString(row.message?.model, row.model, row.model_id, row.modelId);
  });
}

function codexContextFromTranscript(file, activeModel) {
  return readJsonLinesReverse(file, (row) => {
    if (row.type !== "turn_context" || typeof row.payload !== "object") return undefined;
    const model = cleanString(row.payload.model);
    if (activeModel && model && model !== activeModel) return undefined;
    const effort = normalizedEffort(row.payload.effort);
    if (!model && !effort) return undefined;
    return { model, effort };
  });
}

function loadJson(file) {
  if (!file || !existsSync(file)) return undefined;
  return parseJson(readFileSync(file, "utf8"));
}

function qwenSettings(root, home) {
  const files = [
    join(home, ".qwen", "settings.json"),
    join(root, ".qwen", "settings.json"),
    join(root, ".qwen", "settings.local.json"),
  ];
  let model = {};
  for (const file of files) {
    const loaded = loadJson(file);
    if (loaded?.model && typeof loaded.model === "object") {
      model = { ...model, ...loaded.model };
    }
  }
  return model;
}

function findGrokSession(home, sessionId) {
  if (!sessionId) return undefined;
  const base = join(home, ".grok", "sessions");
  if (!existsSync(base)) return undefined;
  for (const project of readDirNames(base)) {
    const candidate = join(base, project, sessionId);
    if (existsSync(join(candidate, "summary.json"))) return candidate;
  }
  return undefined;
}

function readDirNames(directory) {
  try {
    return readdirSync(directory, { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .map((entry) => entry.name);
  } catch {
    return [];
  }
}

function grokSessionValues(sessionDirectory) {
  if (!sessionDirectory) return {};
  const summary = loadJson(join(sessionDirectory, "summary.json"));
  const event = readJsonLinesReverse(join(sessionDirectory, "events.jsonl"), (row) => {
    const model = firstString(row.model_id, row.modelId);
    const effort = normalizedEffort(firstString(row.reasoning_effort, row.reasoningEffort));
    return model || effort ? { model, effort } : undefined;
  });
  const assistant = readJsonLinesReverse(join(sessionDirectory, "chat_history.jsonl"), (row) => {
    if (row.type !== "assistant") return undefined;
    const model = firstString(row.model_id, row.modelId);
    const effort = normalizedEffort(firstString(row.reasoning_effort, row.reasoningEffort));
    return model || effort ? { model, effort } : undefined;
  });
  return {
    model: firstString(assistant?.model, event?.model, summary?.current_model_id),
    effort: normalizedEffort(firstString(assistant?.effort, event?.effort)),
  };
}

function modelDoesNotReason(home, model) {
  if (!model) return false;
  const candidates = [
    join(home, ".grok", "models_cache.json"),
    join(home, ".grok", "models.json"),
  ];
  for (const file of candidates) {
    const loaded = loadJson(file);
    const entry = loaded?.models?.[model];
    const info = entry?.info ?? entry;
    if (info?.supports_reasoning_effort === false || info?.supportsReasoningEffort === false) {
      return true;
    }
  }
  return false;
}

function resolveCodex(input) {
  const hookModel = firstString(input?.model, input?.model_id, input?.modelId);
  const context = codexContextFromTranscript(transcriptPath(input), hookModel);
  const hookEffort = directEffort(input);
  return {
    model: firstString(hookModel, context?.model),
    effort: normalizedEffort(firstString(hookEffort, context?.effort)),
    sources: { model: hookModel ? "hook" : "transcript", effort: hookEffort ? "hook" : "transcript" },
  };
}

function resolveClaude(input, env) {
  const hookModel = firstString(input?.model, input?.model_id, input?.modelId);
  const hookEffort = directEffort(input);
  return {
    model: firstString(hookModel, assistantModelFromTranscript(transcriptPath(input))),
    effort: firstString(hookEffort, normalizedEffort(env.CLAUDE_EFFORT)),
    sources: { model: hookModel ? "hook" : "transcript", effort: hookEffort ? "hook" : "environment" },
  };
}

function resolveQwen(input, root, home) {
  const settings = qwenSettings(root, home);
  const direct = directEffort(input);
  return {
    model: firstString(
      input?.model,
      input?.model_id,
      input?.modelId,
      assistantModelFromTranscript(transcriptPath(input)),
    ),
    effort: normalizedEffort(firstString(direct, settings.reasoningEffort)),
    sources: { model: input?.model ? "hook" : "transcript", effort: direct ? "hook" : "settings selection" },
  };
}

function resolveGrok(input, env, home) {
  const sessionId = firstString(input?.sessionId, input?.session_id, env.GROK_SESSION_ID);
  const session = grokSessionValues(findGrokSession(home, sessionId));
  const model = firstString(input?.modelId, input?.model_id, input?.model, session.model);
  const hookEffort = directEffort(input);
  let effort = hookEffort ?? session.effort;
  let effortSource = hookEffort ? "hook" : "session";
  if (!effort && modelDoesNotReason(home, model)) {
    effort = "none";
    effortSource = "model catalog";
  }
  return {
    model,
    effort,
    sources: { model: input?.modelId || input?.model_id || input?.model ? "hook" : "session", effort: effortSource },
  };
}

export function resolveAttribution(client, input, options = {}) {
  const env = options.env ?? process.env;
  const home = options.home ?? homedir();
  const root = options.root ?? input?.cwd ?? process.cwd();
  let resolved;
  switch (client) {
    case "codex":
      resolved = resolveCodex(input);
      break;
    case "claude":
      resolved = resolveClaude(input, env);
      break;
    case "qwen":
      resolved = resolveQwen(input, root, home);
      break;
    case "grok":
      resolved = resolveGrok(input, env, home);
      break;
    default:
      return { errors: [`unsupported client ${JSON.stringify(client)}`] };
  }
  const errors = validateAttribution(resolved.model, resolved.effort);
  return { ...resolved, errors };
}

export function validateAttribution(model, effort) {
  const errors = [];
  if (!model) {
    errors.push("active model slug is unavailable");
  } else if (!/^\S+$/.test(model) || /^(?:unknown|default|auto)$/i.test(model)) {
    errors.push(`model is not an exact slug: ${JSON.stringify(model)}`);
  }
  if (!effort) {
    errors.push("active reasoning-effort level is unavailable");
  } else if (!EFFORTS.has(effort)) {
    errors.push(`unsupported or ambiguous effort level: ${JSON.stringify(effort)}`);
  }
  return errors;
}

export function detectClient(requested, input, env = process.env) {
  if (requested && requested !== "auto") return requested;
  if (env.GROK_SESSION_ID) return "grok";
  if (env.CODEX_THREAD_ID) return "codex";
  if (env.QWEN_CODE || env.QWEN_PROJECT_DIR) return "qwen";
  if (env.CLAUDE_CODE_SESSION_ID || env.CLAUDE_PROJECT_DIR) return "claude";
  if (input?.sessionId || input?.workspaceRoot) return "grok";
  if (input?.turn_id && input?.model) return "codex";
  return undefined;
}

function shellTokens(command) {
  const tokens = [];
  const matcher = /"(?:\\.|[^"\\])*"|'[^']*'|[^\s;&|()]+/g;
  for (const match of command.matchAll(matcher)) {
    const raw = match[0];
    const quoted = raw.startsWith("\"") || raw.startsWith("'");
    tokens.push({ value: quoted ? raw.slice(1, -1) : raw, quoted });
  }
  return tokens;
}

export function gitCommitWorkingDirectory(command, baseCwd = process.cwd()) {
  if (typeof command !== "string") return undefined;
  const tokens = shellTokens(command);
  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index];
    if (token.quoted || !/(?:^|[\\/])git(?:\.exe)?$/i.test(token.value)) continue;
    let workingDirectory = resolve(baseCwd);
    let cursor = index + 1;
    while (cursor < tokens.length) {
      const value = tokens[cursor].value;
      if (value === "-C") {
        const target = tokens[cursor + 1]?.value;
        if (!target) break;
        workingDirectory = resolve(workingDirectory, target);
        cursor += 2;
        continue;
      }
      if (["-c", "--git-dir", "--work-tree", "--namespace", "--config-env"].includes(value)) {
        cursor += 2;
        continue;
      }
      if (value.startsWith("-")) {
        cursor += 1;
        continue;
      }
      if (value === "commit") return workingDirectory;
      break;
    }
  }
  return undefined;
}

export function containsGitCommit(command) {
  return gitCommitWorkingDirectory(command) !== undefined;
}

export function commandFromHookInput(input) {
  return firstString(
    input?.tool_input?.command,
    input?.toolInput?.command,
    input?.tool_input?.cmd,
    input?.toolInput?.cmd,
  );
}

function git(cwd, ...args) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8" });
  if (result.error) {
    throw new Error(`git ${args.join(" ")} failed to start: ${result.error.message}`);
  }
  if (result.status !== 0) {
    throw new Error((result.stderr ?? "").trim() || `git ${args.join(" ")} failed`);
  }
  return (result.stdout ?? "").trim();
}

function optionalGit(cwd, ...args) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8" });
  if (result.error || result.status !== 0) return undefined;
  return (result.stdout ?? "").trim();
}

export function repositoryRoot(cwd) {
  return git(cwd, "rev-parse", "--show-toplevel");
}

export function privateHooksDirectory(root) {
  return resolve(root, git(root, "rev-parse", "--git-path", "dontspeak-hooks"));
}

export function attributionCachePath(root) {
  return join(privateHooksDirectory(root), ATTRIBUTION_CACHE_FILE);
}

export function ensureCommitMessageHook(root) {
  const hooksDirectory = privateHooksDirectory(root);
  mkdirSync(hooksDirectory, { recursive: true });
  const upstreamFile = join(hooksDirectory, UPSTREAM_HOOKS_FILE);
  const configured = optionalGit(root, "config", "--path", "--get", "core.hooksPath");
  let upstream;
  if (configured && resolve(root, configured) === hooksDirectory && existsSync(upstreamFile)) {
    upstream = cleanString(readFileSync(upstreamFile, "utf8"));
  } else {
    upstream = resolve(root, configured ?? git(root, "rev-parse", "--git-path", "hooks"));
    writeFileSync(upstreamFile, `${upstream}\n`, "utf8");
  }
  const hook = join(hooksDirectory, "commit-msg");
  const upstreamHook = upstream
    ? join(upstream, "commit-msg").replaceAll("\\", "/").replaceAll("'", "'\\''")
    : undefined;
  const contents = ["#!/bin/sh"];
  if (upstreamHook && resolve(upstream, "commit-msg") !== hook) {
    contents.push(
      `if [ -x '${upstreamHook}' ]; then`,
      `  '${upstreamHook}' "$1" || exit $?`,
      "fi",
    );
  }
  contents.push(
    "exec node \"$(git rev-parse --show-toplevel)/scripts/commit-agent-attribution.mjs\" \"$1\"",
    "",
  );
  writeFileSync(hook, contents.join("\n"), { encoding: "utf8", mode: 0o755 });
  try {
    chmodSync(hook, 0o755);
  } catch {
    // Git for Windows does not use POSIX execute bits.
  }
  git(root, "config", "--local", "extensions.worktreeConfig", "true");
  git(root, "config", "--worktree", "core.hooksPath", hooksDirectory);
  return hooksDirectory;
}

export function writeAttributionCache(root, record) {
  const file = attributionCachePath(root);
  mkdirSync(dirname(file), { recursive: true });
  const temporary = `${file}.${process.pid}.${Date.now()}.tmp`;
  writeFileSync(temporary, `${JSON.stringify(record, null, 2)}\n`, "utf8");
  renameSync(temporary, file);
  return file;
}

export function readAttributionCache(root) {
  return loadJson(attributionCachePath(root));
}

export function removeAttributionCache(root) {
  try {
    unlinkSync(attributionCachePath(root));
  } catch {
    // A missing cache is already the desired state.
  }
}

export function sessionIdFromInput(input, env = process.env) {
  return firstString(
    input?.session_id,
    input?.sessionId,
    env.GROK_SESSION_ID,
    env.CODEX_THREAD_ID,
    env.CLAUDE_CODE_SESSION_ID,
    env.QWEN_SESSION_ID,
  );
}

export function activeAgentEnvironment(env = process.env) {
  if (env.GROK_SESSION_ID) return { client: "grok", sessionId: env.GROK_SESSION_ID };
  if (env.CODEX_THREAD_ID) return { client: "codex", sessionId: env.CODEX_THREAD_ID };
  if (env.CLAUDE_CODE_SESSION_ID) return { client: "claude", sessionId: env.CLAUDE_CODE_SESSION_ID };
  if (env.QWEN_CODE || env.QWEN_PROJECT_DIR) return { client: "qwen", sessionId: env.QWEN_SESSION_ID };
  return undefined;
}

function parseAgentTrailer(line) {
  const match = /^Agent: (\S+) (\S+)$/.exec(line);
  if (!match) return undefined;
  return { model: match[1], effort: match[2], line };
}

export function validateCommitMessage(message) {
  const lines = message.trimEnd().split(/\r?\n/);
  const trailers = [];
  while (lines.length > 0 && lines.at(-1).startsWith("Agent:")) {
    trailers.unshift(lines.pop());
  }
  const errors = [];
  if (trailers.length === 0) errors.push("missing final Agent trailer");
  const seen = new Set();
  for (const trailer of trailers) {
    const parsed = parseAgentTrailer(trailer);
    if (!parsed) {
      errors.push(`malformed trailer: ${trailer}`);
      continue;
    }
    for (const error of validateAttribution(parsed.model, parsed.effort)) {
      errors.push(`${trailer}: ${error}`);
    }
    if (seen.has(trailer)) errors.push(`duplicate trailer: ${trailer}`);
    seen.add(trailer);
  }
  for (const line of lines) {
    if (line.startsWith("Agent:")) errors.push(`Agent trailer is not final: ${line}`);
    if (PROHIBITED_ATTRIBUTION.test(line)) errors.push(`prohibited attribution: ${line}`);
  }
  return errors;
}

export function rewriteCommitMessage(message, model, effort) {
  const pairErrors = validateAttribution(model, effort);
  if (pairErrors.length > 0) throw new Error(pairErrors.join("; "));

  const lines = message.trimEnd().split(/\r?\n/);
  const existing = lines.filter((line) => line.startsWith("Agent:"));
  const body = lines.filter((line) => !line.startsWith("Agent:") && !PROHIBITED_ATTRIBUTION.test(line));
  while (body.length > 0 && body.at(-1).trim() === "") body.pop();

  let trailers = [];
  if (existing.length > 1) {
    for (const line of existing) {
      const parsed = parseAgentTrailer(line);
      if (!parsed) throw new Error(`cannot preserve malformed squash attribution: ${line}`);
      const errors = validateAttribution(parsed.model, parsed.effort);
      if (errors.length > 0) throw new Error(`cannot preserve ${line}: ${errors.join("; ")}`);
      if (!trailers.includes(line)) trailers.push(line);
    }
  }
  const current = `Agent: ${model} ${effort}`;
  if (!trailers.includes(current)) trailers.push(current);
  return `${body.join("\n")}\n\n${trailers.join("\n")}\n`;
}

export function validateCacheRecord(record, root, env = process.env, now = Date.now()) {
  const errors = [];
  if (!record || record.version !== 1) return ["no usable runtime attribution capture was found"];
  if (!record.root || resolve(record.root) !== resolve(root)) errors.push("runtime capture belongs to a different worktree");
  const captured = Date.parse(record.capturedAt);
  if (!Number.isFinite(captured) || now - captured > ATTRIBUTION_CACHE_MAX_AGE_MS || captured > now + 30_000) {
    errors.push("runtime attribution capture is stale");
  }
  const active = activeAgentEnvironment(env);
  if (active?.client && record.client !== active.client) {
    errors.push(`runtime capture is for ${record.client}, but this commit runs under ${active.client}`);
  }
  if (active?.sessionId && record.sessionId && active.sessionId !== record.sessionId) {
    errors.push("runtime capture belongs to a different agent session");
  }
  errors.push(...(Array.isArray(record.errors) ? record.errors : []));
  errors.push(...validateAttribution(record.model, record.effort));
  return [...new Set(errors)];
}

export function hookWorkingDirectory(input) {
  const configured = firstString(
    input?.tool_input?.workdir,
    input?.toolInput?.workdir,
    input?.tool_input?.cwd,
    input?.toolInput?.cwd,
    input?.cwd,
  );
  return configured && isAbsolute(configured) ? configured : resolve(input?.cwd ?? process.cwd(), configured ?? ".");
}
