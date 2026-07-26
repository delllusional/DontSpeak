import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
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
  realpathSync,
  renameSync,
  statSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { dirname, isAbsolute, join, posix, resolve, win32 } from "node:path";

export const ATTRIBUTION_CACHE_MAX_AGE_MS = 15 * 60 * 1000;
const ENVLESS_ATTRIBUTION_CACHE_MAX_AGE_MS = 5 * 60 * 1000;
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

function readFileSlice(file, start, end) {
  const handle = openSync(file, "r");
  try {
    const size = fstatSync(handle).size;
    const from = Math.max(0, Math.min(start, size));
    const to = Math.max(from, Math.min(end, size));
    const buffer = Buffer.alloc(to - from);
    readSync(handle, buffer, 0, buffer.length, from);
    return buffer.toString("utf8");
  } finally {
    closeSync(handle);
  }
}

export function readJsonLinesReverse(file, selector, options = {}) {
  if (!file || !existsSync(file)) return undefined;
  const maxBytes = options.maxBytes ?? 2 * 1024 * 1024;
  const retryMaxBytes = options.retryMaxBytes ?? 32 * 1024 * 1024;
  const scanLines = (text) => {
    const lines = text.split(/\r?\n/);
    for (let index = lines.length - 1; index >= 0; index -= 1) {
      const row = parseJson(lines[index]);
      if (!row) continue;
      const selected = selector(row);
      if (selected !== undefined && selected !== null) return selected;
    }
    return undefined;
  };
  const found = scanLines(readFileTail(file, maxBytes));
  if (found !== undefined) return found;
  // Long sessions outgrow the default tail; retry once with a larger cap.
  let size;
  try {
    size = statSync(file).size;
  } catch {
    return undefined;
  }
  if (size <= maxBytes || retryMaxBytes <= maxBytes) return undefined;
  // Retry scans only the not-yet-read head. Drop the whole boundary-straddling
  // line — its tail half was already discarded by readFileTail; parsing it
  // would invent two bogus rows.
  const start = Math.max(0, size - retryMaxBytes);
  let text = readFileSlice(file, start, size - maxBytes);
  if (start > 0) {
    const firstNewline = text.indexOf("\n");
    text = firstNewline === -1 ? "" : text.slice(firstNewline + 1);
  }
  const lastNewline = text.lastIndexOf("\n");
  text = lastNewline === -1 ? "" : text.slice(0, lastNewline);
  return scanLines(text);
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

function findFileByName(directory, predicate) {
  const pending = [directory];
  while (pending.length > 0) {
    const current = pending.pop();
    let entries;
    try {
      entries = readdirSync(current, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      const candidate = join(current, entry.name);
      if (entry.isDirectory()) pending.push(candidate);
      else if (entry.isFile() && predicate(entry.name)) return candidate;
    }
  }
  return undefined;
}

function findSessionTranscript(home, client, sessionId) {
  if (!sessionId) return undefined;
  const directories = {
    codex: [join(home, ".codex", "sessions"), join(home, ".codex", "archived_sessions")],
    claude: [join(home, ".claude", "projects")],
    qwen: [join(home, ".qwen", "projects")],
  }[client] ?? [];
  const matches = (name) => name === `${sessionId}.jsonl`
    || name.endsWith(`-${sessionId}.jsonl`);
  for (const directory of directories) {
    const transcript = findFileByName(directory, matches);
    if (transcript) return transcript;
  }
  return undefined;
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

// Live sessions from ~/.grok/active_sessions.json ({session_id, cwd, …}).
function findActiveGrokSession(home) {
  const rows = loadJson(join(home, ".grok", "active_sessions.json"));
  if (!Array.isArray(rows)) return undefined;
  for (const row of rows) {
    const id = firstString(row?.session_id, row?.sessionId, row?.id);
    const dir = findGrokSession(home, id);
    if (dir) return dir;
  }
  return undefined;
}

function isGrokSubagentSession(summary) {
  const kind = firstString(summary?.session_kind, summary?.sessionKind) ?? "";
  return kind === "subagent" || kind === "subagent_resume";
}

// Newest non-subagent Grok session whose summary cwd/git_root matches `cwd`.
function findLatestGrokSession(home, cwd) {
  if (!cwd) return undefined;
  const base = join(home, ".grok", "sessions");
  if (!existsSync(base)) return undefined;
  const want = normalizePathKey(cwd);
  let best;
  let bestSub = -1;
  let bestAt = "";
  for (const project of readDirNames(base)) {
    for (const id of readDirNames(join(base, project))) {
      const dir = join(base, project, id);
      const summary = loadJson(join(dir, "summary.json"));
      if (!summary) continue;
      const roots = [
        summary.git_root_dir,
        summary.info?.cwd,
        summary.cwd,
      ].filter(Boolean).map(normalizePathKey);
      if (!roots.some((r) => r === want || want.startsWith(`${r}/`) || r.startsWith(`${want}/`))) {
        continue;
      }
      // Prefer parent sessions over plan/implement subagents.
      const subRank = isGrokSubagentSession(summary) ? 0 : 1;
      const at = firstString(summary.last_active_at, summary.updated_at, summary.created_at) ?? "";
      if (!best || subRank > bestSub || (subRank === bestSub && at > bestAt)) {
        best = dir;
        bestSub = subRank;
        bestAt = at;
      }
    }
  }
  return best;
}

function normalizePathKey(pathValue) {
  let path = String(pathValue);
  try {
    path = realpathSync(path);
  } catch {
    // A stale/nonexistent session path can still be compared lexically.
  }
  return path
    .replace(/\\/g, "/")
    .replace(/\/+$/, "")
    .toLowerCase();
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
  // Prefer product slug (current_model_id) over per-turn build variants so the
  // trailer matches the user-selected model. Effort: turn rows, then summary.
  return {
    model: firstString(summary?.current_model_id, assistant?.model, event?.model),
    effort: normalizedEffort(firstString(
      assistant?.effort,
      event?.effort,
      summary?.reasoning_effort,
      summary?.reasoningEffort,
    )),
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

function codexTranscriptSessionId(file) {
  if (!file || !existsSync(file)) return undefined;
  const text = readFileSlice(file, 0, 1024 * 1024);
  for (const line of text.split(/\r?\n/)) {
    const row = parseJson(line);
    if (row?.type !== "session_meta") continue;
    return firstString(row.payload?.id, row.session_id, row.sessionId);
  }
  return undefined;
}

function codexTurnContext(file, sessionId, turnId, hookModel) {
  if (!file || !sessionId || !turnId || !hookModel) return undefined;
  if (codexTranscriptSessionId(file) !== sessionId) return undefined;
  return readJsonLinesReverse(file, (row) => {
    if (row.type !== "turn_context" || row.payload?.turn_id !== turnId) return undefined;
    const model = firstString(
      row.payload?.model,
      row.payload?.collaboration_mode?.settings?.model,
    );
    if (model !== hookModel) return {};
    return {
      model,
      effort: normalizedEffort(firstString(
        row.payload?.effort,
        row.payload?.reasoning_effort,
        row.payload?.collaboration_mode?.settings?.reasoning_effort,
      )),
    };
  });
}

function resolveCodex(input, env, home) {
  const hookModel = firstString(input?.model, input?.model_id, input?.modelId);
  const hookEffort = directEffort(input);
  const sessionId = firstString(input?.session_id, input?.sessionId, env.CODEX_THREAD_ID);
  const turnId = firstString(input?.turn_id, input?.turnId);
  const transcript = transcriptPath(input) ?? findSessionTranscript(home, "codex", sessionId);
  // Codex hooks expose the exact session, turn, and model but not effort. Read
  // effort only from that same turn and require the transcript model to agree.
  const turn = hookEffort ? undefined : codexTurnContext(transcript, sessionId, turnId, hookModel);
  return {
    model: hookModel,
    effort: hookEffort ?? turn?.effort,
  };
}

function resolveClaude(input, env, home) {
  const hookModel = firstString(input?.model, input?.model_id, input?.modelId);
  const hookEffort = directEffort(input);
  const sessionId = firstString(input?.sessionId, input?.session_id, env.CLAUDE_CODE_SESSION_ID);
  const transcript = transcriptPath(input) ?? findSessionTranscript(home, "claude", sessionId);
  return {
    model: firstString(hookModel, assistantModelFromTranscript(transcript)),
    effort: firstString(hookEffort, normalizedEffort(env.CLAUDE_EFFORT)),
  };
}

function resolveQwen(input, root, home, env) {
  const settings = qwenSettings(root, home);
  const direct = directEffort(input);
  const sessionId = firstString(input?.sessionId, input?.session_id, env.QWEN_SESSION_ID);
  const transcript = transcriptPath(input) ?? findSessionTranscript(home, "qwen", sessionId);
  return {
    model: firstString(
      input?.model,
      input?.model_id,
      input?.modelId,
      assistantModelFromTranscript(transcript),
    ),
    effort: normalizedEffort(firstString(direct, settings.reasoningEffort)),
  };
}

function resolveGrok(input, env, home, root) {
  const sessionId = firstString(input?.sessionId, input?.session_id, env.GROK_SESSION_ID);
  // GROK_AGENT often lacks GROK_SESSION_ID. Fall back: newest parent matching
  // worktree/cwd, then ~/.grok/active_sessions.json.
  const cwdHint = firstString(
    input?.cwd,
    input?.workspaceRoot,
    env.GROK_CWD,
    root,
  );
  const needFallback = Boolean(env.GROK_AGENT || env.GROK_SESSION_ID || sessionId || root);
  const sessionDirectory = findGrokSession(home, sessionId)
    ?? (needFallback ? findLatestGrokSession(home, cwdHint) : undefined)
    ?? (needFallback ? findActiveGrokSession(home) : undefined);
  const session = grokSessionValues(sessionDirectory);
  const model = firstString(input?.modelId, input?.model_id, input?.model, session.model);
  const hookEffort = directEffort(input);
  let effort = hookEffort ?? session.effort;
  if (!effort && modelDoesNotReason(home, model)) {
    effort = "none";
  }
  return {
    model,
    effort,
  };
}

export function resolveAttribution(client, input, options = {}) {
  const env = options.env ?? process.env;
  const home = options.home ?? env.HOME ?? env.USERPROFILE ?? homedir();
  const root = options.root ?? input?.cwd ?? process.cwd();
  let resolved;
  switch (client) {
    case "codex":
      resolved = resolveCodex(input, env, home);
      break;
    case "claude":
      resolved = resolveClaude(input, env, home);
      break;
    case "qwen":
      resolved = resolveQwen(input, root, home, env);
      break;
    case "grok":
      resolved = resolveGrok(input, env, home, root);
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
  } else if (
    !/^\S+$/.test(model)
    || /^(?:unknown|default|auto)$/i.test(model)
    || /^gpt-\d+(?:\.\d+)?$/i.test(model)
    // Bare family words only; "human" stays valid ("Agent: human none").
    || /^(?:claude|sonnet|opus|haiku|fable|gpt|codex|grok|gemini|qwen)$/i.test(model)
  ) {
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
  // GROK_AGENT marks tool shells even without GROK_SESSION_ID.
  if (env.GROK_SESSION_ID || env.GROK_AGENT) return "grok";
  if (env.CODEX_THREAD_ID) return "codex";
  if (env.QWEN_CODE || env.QWEN_PROJECT_DIR) return "qwen";
  if (env.CLAUDE_CODE_SESSION_ID || env.CLAUDE_PROJECT_DIR) return "claude";
  if (input?.sessionId || input?.workspaceRoot) return "grok";
  if (input?.turn_id && input?.model) return "codex";
  return undefined;
}

const SEPARATORS = new Set([";", "&&", "||", "|", "&", "(", ")"]);
const GIT_VALUE_OPTIONS = ["-c", "--namespace", "--config-env"];
const GIT_COMMAND_NAME = /(^|[\\/])git(\.exe)?$/i;
const SHELL_COMMAND_NAME = /(^|[\\/])(?:ba|z|da)?sh(\.exe)?$/i;
const SHELL_COMMAND_FLAG = /^-[a-zA-Z]*c[a-zA-Z]*$/;

function shellTokens(command) {
  const tokens = [];
  // Collapse backslash-newline first (may touch quoted content; ok for path/flag detection).
  const source = command.replace(/\\\r?\n/g, " ");
  const matcher = /"(?:\\.|[^"\\])*"|'[^']*'|\|\||&&|[;&|()]|\r?\n|[^\s;&|()]+/g;
  for (const match of source.matchAll(matcher)) {
    const raw = match[0];
    if (raw === "\n" || raw === "\r\n") {
      tokens.push({ value: "\n", separator: true });
      continue;
    }
    if (SEPARATORS.has(raw)) {
      tokens.push({ value: raw, separator: true });
      continue;
    }
    const quoted = raw.startsWith("\"") || raw.startsWith("'");
    tokens.push({
      value: quoted ? raw.slice(1, -1) : raw,
      quoted,
      doubleQuoted: raw.startsWith("\""),
    });
  }
  return tokens;
}

// Fail-closed: only a plain literal path may steer cwd tracking. Unquoted tokens
// must match the allowlist (no backslash — unquoted \ is a bash escape). Tildes
// are resolveShellPath's job (~/… expands, ~user fails closed).
function unsafePathToken(token) {
  if (token.argv) return false; // post-shell argv is exact
  const value = token.value;
  if (token.quoted) {
    if (value.startsWith("~")) return true; // quoted ~ is literal; resolver would expand it
    return Boolean(token.doubleQuoted) && /[$`]/.test(value);
  }
  return !/^[A-Za-z0-9._:@+,\/~=-]+$/.test(value);
}

// Pure so tests can exercise win32 branches on any host.
export function resolveShellPath(baseCwd, arg, options = {}) {
  const platform = options.platform ?? process.platform;
  const paths = platform === "win32" ? win32 : posix;
  if (typeof baseCwd !== "string" || typeof arg !== "string" || arg === "") return undefined;
  let candidate = arg;
  if (candidate === "~") candidate = options.home ?? homedir();
  else if (candidate.startsWith("~/")) candidate = paths.join(options.home ?? homedir(), candidate.slice(2));
  else if (candidate.startsWith("~")) return undefined;
  if (platform === "win32" && candidate.startsWith("/")) {
    const drive = /^\/([A-Za-z])(\/.*)?$/.exec(candidate);
    if (!drive) return undefined; // msys mount (/tmp, /usr): not translatable
    candidate = `${drive[1].toUpperCase()}:${drive[2] ?? "/"}`;
  }
  return paths.resolve(baseCwd, candidate);
}

function isCommandToken(token, nameRe) {
  if (token.separator) return false;
  const match = nameRe.exec(token.value);
  // Quoted bare names are data; quoted paths still count.
  return match !== null && (!token.quoted || match[1] !== "");
}

function parseGitInvocation(segment, start, cwd, settings) {
  let workingDirectory = cwd;
  let cursor = start + 1;
  while (cursor < segment.length) {
    const value = segment[cursor].value;
    if (value === "-C") {
      const target = segment[cursor + 1];
      if (!target) return undefined;
      if (workingDirectory !== undefined) {
        workingDirectory = unsafePathToken(target)
          ? undefined
          : resolveShellPath(workingDirectory, target.value, settings);
      }
      cursor += 2;
      continue;
    }
    // --git-dir / --work-tree redirect away from cwd: fail closed.
    if (value === "--git-dir" || value === "--work-tree") {
      workingDirectory = undefined;
      cursor += 2;
      continue;
    }
    if (value.startsWith("--git-dir=") || value.startsWith("--work-tree=")) {
      workingDirectory = undefined;
      cursor += 1;
      continue;
    }
    if (GIT_VALUE_OPTIONS.includes(value)) {
      cursor += 2;
      continue;
    }
    if (value.startsWith("-")) {
      cursor += 1;
      continue;
    }
    if (value === "commit" || value === "merge") {
      // Keep in sync with wrapper pre-filters in .claude/settings.json and
      // .codex/hooks.json (command / commandWindows).
      if (workingDirectory === undefined) return { end: cursor }; // fail closed, consume only
      return { end: cursor, invocation: { workingDirectory, subcommand: value } };
    }
    return undefined;
  }
  return undefined;
}

function recurseShellPayload(segment, start, cwd, settings, out, depth) {
  for (let index = start + 1; index + 1 < segment.length; index += 1) {
    const flag = segment[index];
    // Only leading flag run counts: `sh script.sh -c "…"` passes -c to the script.
    if (flag.quoted || !flag.value.startsWith("-")) return undefined;
    if (SHELL_COMMAND_FLAG.test(flag.value)) {
      if (cwd !== undefined && depth < 3) {
        out.push(...invocationsFromTokens(shellTokens(segment[index + 1].value), cwd, settings, depth + 1));
      }
      return index + 2;
    }
  }
  return undefined;
}

function processSegment(segment, state, settings, out, depth) {
  const command = segment[0].value;
  if (command === "cd" || command === "pushd") {
    const target = segment.slice(1).find(
      (token) => token.quoted || (token.value !== "--" && !/^-[LPe@]+$/.test(token.value)),
    );
    if (command === "pushd") {
      if (!target) {
        // Bare pushd rotates the stack → cwd/stack both unknown.
        state.cwd = undefined;
        state.dirStack = [];
        return;
      }
      state.dirStack.push(state.cwd);
    } else if (!target) {
      state.cwd = undefined;
      return;
    }
    state.cwd = target.value === "-" || unsafePathToken(target)
      ? undefined
      : resolveShellPath(state.cwd, target.value, settings);
    return;
  }
  if (command === "popd") {
    state.cwd = state.dirStack.pop(); // empty → unknown
    return;
  }
  let index = 0;
  while (index < segment.length) {
    const token = segment[index];
    if (isCommandToken(token, GIT_COMMAND_NAME)) {
      const parsed = parseGitInvocation(segment, index, state.cwd, settings);
      if (parsed) {
        if (parsed.invocation) out.push(parsed.invocation);
        index = parsed.end + 1;
        continue;
      }
    } else if (isCommandToken(token, SHELL_COMMAND_NAME)) {
      const consumed = recurseShellPayload(segment, index, state.cwd, settings, out, depth);
      if (consumed !== undefined) {
        index = consumed;
        continue;
      }
    }
    index += 1;
  }
}

function invocationsFromTokens(tokens, baseCwd, settings, depth) {
  const out = [];
  const paths = settings.platform === "win32" ? win32 : posix;
  const state = {
    cwd: typeof baseCwd === "string" ? paths.resolve(baseCwd) : undefined,
    dirStack: [],
  };
  const subshells = [];
  let index = 0;
  while (index < tokens.length) {
    const token = tokens[index];
    if (token.separator) {
      // Subshell: cd/pushd inside (...) must not leak past ).
      if (token.value === "(") {
        subshells.push({ cwd: state.cwd, dirStack: [...state.dirStack] });
      } else if (token.value === ")") {
        const saved = subshells.pop();
        state.cwd = saved?.cwd;
        state.dirStack = saved?.dirStack ?? [];
      }
      index += 1;
      continue;
    }
    const segment = [];
    while (index < tokens.length && !tokens[index].separator) {
      segment.push(tokens[index]);
      index += 1;
    }
    processSegment(segment, state, settings, out, depth);
  }
  return out;
}

export function gitCommitInvocations(command, baseCwd = process.cwd(), options = {}) {
  const settings = { platform: options.platform ?? process.platform, home: options.home };
  let tokens;
  if (Array.isArray(command)) {
    if (command.length === 0 || !command.every((element) => typeof element === "string")) return [];
    tokens = command.map((value) => ({ value, quoted: false, argv: true }));
  } else if (typeof command === "string") {
    tokens = shellTokens(command);
  } else {
    return [];
  }
  return invocationsFromTokens(tokens, baseCwd, settings, 0);
}

export function gitCommitWorkingDirectory(command, baseCwd = process.cwd()) {
  return gitCommitInvocations(command, baseCwd)[0]?.workingDirectory;
}

export function commandFromHookInput(input) {
  const candidates = [
    input?.tool_input?.command,
    input?.toolInput?.command,
    input?.tool_input?.cmd,
    input?.toolInput?.cmd,
  ];
  for (const candidate of candidates) {
    if (Array.isArray(candidate)) {
      // argv arrays pass through pre-split.
      if (candidate.length > 0 && candidate.every((element) => typeof element === "string")) {
        return candidate;
      }
      continue;
    }
    const cleaned = cleanString(candidate);
    if (cleaned) return cleaned;
  }
  return undefined;
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

// Missing dir / "not a git repository" → undefined (silent skip); else throws (exit 2).
export function resolveRepositoryRoot(directory) {
  if (!existsSync(directory)) return undefined;
  const result = spawnSync("git", ["rev-parse", "--show-toplevel"], { cwd: directory, encoding: "utf8" });
  if (result.error) throw new Error(`git rev-parse failed to start: ${result.error.message}`);
  if (result.status === 0) return (result.stdout ?? "").trim() || undefined;
  const stderr = (result.stderr ?? "").trim();
  if (/not a git repository/i.test(stderr)) return undefined;
  throw new Error(stderr || "git rev-parse --show-toplevel failed");
}

export function privateHooksDirectory(root) {
  return resolve(root, git(root, "rev-parse", "--git-path", "dontspeak-hooks"));
}

export function repositoryCommonDirectory(root) {
  return resolve(root, git(root, "rev-parse", "--git-common-dir"));
}

function attributionCacheFile(sessionId) {
  const cleaned = cleanString(sessionId);
  if (!cleaned) return ATTRIBUTION_CACHE_FILE;
  const digest = createHash("sha256").update(cleaned).digest("hex");
  return `agent-attribution-${digest}.json`;
}

export function attributionCachePath(root, sessionId) {
  return join(repositoryCommonDirectory(root), "dontspeak-hooks", attributionCacheFile(sessionId));
}

function unwrapManagedHooksDirectory(root, directory, fallback) {
  const seen = new Set();
  let candidate = resolve(root, directory);
  while (!seen.has(candidate)) {
    seen.add(candidate);
    const upstreamFile = join(candidate, UPSTREAM_HOOKS_FILE);
    if (!existsSync(upstreamFile)) return candidate;
    const recorded = cleanString(readFileSync(upstreamFile, "utf8"));
    if (!recorded) return fallback;
    candidate = resolve(root, recorded);
  }
  return fallback;
}

function writeIfChanged(file, contents, mode) {
  try {
    if (readFileSync(file, "utf8") === contents) return false;
  } catch {
    // missing / unreadable → write
  }
  writeFileSync(file, contents, mode === undefined ? "utf8" : { encoding: "utf8", mode });
  return true;
}

export function ensureCommitMessageHook(root) {
  // One spawn: rev-parse prints one result per arg line.
  const [hooksPath, commonDir] = git(
    root,
    "rev-parse",
    "--git-path",
    "dontspeak-hooks",
    "--git-common-dir",
  ).split(/\r?\n/);
  const hooksDirectory = resolve(root, hooksPath);
  mkdirSync(hooksDirectory, { recursive: true });
  const upstreamFile = join(hooksDirectory, UPSTREAM_HOOKS_FILE);
  const fallback = resolve(root, commonDir, "hooks");
  // Walk scopes highest-first (last line first). First foreign value from any
  // scope (incl. worktree — chain, don't clobber user hooksPath) is upstream,
  // unwrapped if managed. Skip our own dir; keep its marker as last resort when
  // our worktree entry shadows the prior upstream scope.
  let upstream;
  let managedMarker;
  const scoped = optionalGit(root, "config", "--show-scope", "--path", "--get-all", "core.hooksPath");
  if (scoped) {
    const values = scoped
      .split(/\r?\n/)
      .map((line) => line.split("\t").slice(1).join("\t"))
      .filter(Boolean);
    for (const value of values.reverse()) {
      if (resolve(root, value) === hooksDirectory) {
        managedMarker ??= unwrapManagedHooksDirectory(root, value, fallback);
        continue;
      }
      const candidate = unwrapManagedHooksDirectory(root, value, fallback);
      if (candidate !== hooksDirectory) {
        upstream = candidate;
        break;
      }
    }
  }
  upstream ??= managedMarker ?? fallback;
  writeIfChanged(upstreamFile, `${upstream}\n`);
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
    "exec node \"$(git rev-parse --show-toplevel)/scripts/agents/commit-agent-attribution.mjs\" \"$1\"",
    "",
  );
  writeIfChanged(hook, contents.join("\n"), 0o755);
  // Always chmod: repairs a lost exec bit when contents are identical.
  try {
    chmodSync(hook, 0o755);
  } catch {
    // Git for Windows ignores POSIX execute bits.
  }
  if (optionalGit(root, "config", "--worktree", "--get", "core.hooksPath") !== hooksDirectory) {
    git(root, "config", "--local", "extensions.worktreeConfig", "true");
    git(root, "config", "--worktree", "core.hooksPath", hooksDirectory);
  }
  return hooksDirectory;
}

export function writeAttributionCache(root, record) {
  const file = attributionCachePath(root, record?.sessionId);
  mkdirSync(dirname(file), { recursive: true });
  const temporary = `${file}.${process.pid}.${Date.now()}.tmp`;
  writeFileSync(temporary, `${JSON.stringify(record, null, 2)}\n`, "utf8");
  // Windows AV/indexers can lock the target; retry briefly.
  for (let attempt = 0; ; attempt += 1) {
    try {
      renameSync(temporary, file);
      return file;
    } catch (error) {
      if (attempt >= 3 || !["EPERM", "EACCES", "EBUSY"].includes(error.code)) throw error;
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 10);
    }
  }
}

export function readAttributionCache(root, sessionId) {
  return loadJson(attributionCachePath(root, sessionId));
}

export function removeAttributionCache(root, sessionId) {
  try {
    unlinkSync(attributionCachePath(root, sessionId));
  } catch {
    // missing is fine
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
  const grok = Boolean(env.GROK_SESSION_ID || env.GROK_AGENT);
  const codex = Boolean(env.CODEX_THREAD_ID);
  if (grok && codex) {
    return { conflict: ["grok", "codex"] };
  }
  // GROK_AGENT alone marks an agent shell (session id may be absent).
  if (grok) {
    return { client: "grok", sessionId: env.GROK_SESSION_ID || undefined };
  }
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

// Trailing trailer block for validate + rewrite: pop blanks, Agent:, prohibited
// lines. Mutates `lines`; never consumes the subject.
function popTrailerBlock(lines) {
  const trailers = [];
  const prohibited = [];
  while (lines.length > 1) {
    const line = lines.at(-1);
    if (line.trim() === "") {
      lines.pop();
      continue;
    }
    if (PROHIBITED_ATTRIBUTION.test(line)) {
      prohibited.unshift(lines.pop());
      continue;
    }
    if (line.startsWith("Agent:")) {
      trailers.unshift(lines.pop());
      continue;
    }
    break;
  }
  return { trailers, prohibited };
}

export function validateCommitMessage(message) {
  const lines = message.trimEnd().split(/\r?\n/);
  const { trailers, prohibited } = popTrailerBlock(lines);
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
  for (const line of prohibited) errors.push(`prohibited attribution: ${line}`);
  for (const line of lines) {
    if (line.startsWith("Agent:")) errors.push(`Agent trailer is not final: ${line}`);
    if (PROHIBITED_ATTRIBUTION.test(line)) errors.push(`prohibited attribution: ${line}`);
  }
  return errors;
}

export function rewriteCommitMessage(message, model, effort, { preserveLone = false } = {}) {
  const pairErrors = validateAttribution(model, effort);
  if (pairErrors.length > 0) throw new Error(pairErrors.join("; "));

  const lines = message.trimEnd().split(/\r?\n/);
  // Attribution-shaped subject cannot be stripped without destroying the message.
  if (lines.length > 0 && (lines[0].startsWith("Agent:") || PROHIBITED_ATTRIBUTION.test(lines[0]))) {
    throw new Error(`commit subject looks like an attribution line: ${lines[0]}`);
  }
  const { trailers: existing } = popTrailerBlock(lines);
  // Preserve only trailing-block candidates; strip mid-body Agent:/prohibited lines.
  const body = lines.filter((line) => !line.startsWith("Agent:") && !PROHIBITED_ATTRIBUTION.test(line));
  while (body.length > 0 && body.at(-1).trim() === "") body.pop();

  const trailers = [];
  // >=2 = squash, keep all. preserveLone = message re-presents a proven lone trailer.
  if (existing.length > 1 || (preserveLone && existing.length === 1)) {
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
  const rewritten = `${body.join("\n")}\n\n${trailers.join("\n")}\n`;
  // Never emit a message the CI checker would reject.
  const messageErrors = validateCommitMessage(rewritten);
  if (messageErrors.length > 0) throw new Error(messageErrors.join("; "));
  return rewritten;
}

// Comparison only (commit-msg runs before --cleanup). Never applied to output.
export function normalizedMessageForComparison(message) {
  const lines = message
    .replace(/\r\n/g, "\n")
    .split("\n")
    .filter((line) => !line.startsWith("#"))
    .map((line) => line.replace(/[ \t]+$/, ""));
  while (lines.length > 0 && lines.at(-1) === "") lines.pop();
  return lines.join("\n");
}

// preserveLone gate: message re-presents HEAD (amend --no-edit / -C HEAD).
// Unborn HEAD / spawn error → no preserve.
export function messageMatchesHead(message, root) {
  const head = optionalGit(root, "show", "-s", "--format=%B", "HEAD");
  if (head === undefined) return false;
  return normalizedMessageForComparison(message) === normalizedMessageForComparison(head);
}

// Transitional: pre-`uses` version-1 records get a single use.
export function normalizeCacheRecord(record) {
  if (record && record.version === 1 && !("uses" in record)) {
    return { ...record, uses: 1 };
  }
  return record;
}

export function validateCacheRecord(record, root, env = process.env, now = Date.now()) {
  const errors = [];
  if (!record || record.version !== 1) return ["no usable runtime attribution capture was found"];
  record = normalizeCacheRecord(record);
  if (!Number.isInteger(record.uses) || record.uses <= 0) {
    errors.push("no usable runtime attribution capture was found");
  }
  if (!record.root) {
    errors.push("runtime capture has no repository identity");
  } else if (resolve(record.root) !== resolve(root)) {
    let commonDir;
    try {
      commonDir = repositoryCommonDirectory(root);
    } catch {
      commonDir = undefined;
    }
    if (!record.commonDir || !commonDir || resolve(record.commonDir) !== resolve(commonDir)) {
      errors.push("runtime capture belongs to a different repository");
    }
  }
  const active = activeAgentEnvironment(env);
  if (active?.conflict) {
    errors.push(`conflicting active agent markers: ${active.conflict.join(", ")}`);
  }
  // Agent env: 15m (long command chains). Env-less human terminal: 5m.
  const maxAge = active ? ATTRIBUTION_CACHE_MAX_AGE_MS : ENVLESS_ATTRIBUTION_CACHE_MAX_AGE_MS;
  const captured = Date.parse(record.capturedAt);
  if (!Number.isFinite(captured) || now - captured > maxAge || captured > now + 30_000) {
    errors.push("runtime attribution capture is stale");
  }
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
