#!/usr/bin/env node

import { readFileSync, writeFileSync } from "node:fs";
import {
  activeAgentEnvironment,
  messageMatchesHead,
  normalizeCacheRecord,
  readAttributionCache,
  removeAttributionCache,
  repositoryRoot,
  resolveAttribution,
  resolveCodexLiveCommit,
  repositoryCommonDirectory,
  rewriteCommitMessage,
  validateCacheRecord,
  validateCommitMessage,
  writeAttributionCache,
} from "./agent-attribution.mjs";

function fail(errors) {
  console.error("commit blocked: exact Agent attribution could not be proven:");
  for (const error of errors) console.error(`  ${error}`);
  console.error("Choose an explicit model and effort in the active CLI, then retry the commit.");
  process.exit(1);
}

function stamp(messageFile, message, root, model, effort) {
  const preserveLone = messageMatchesHead(message, root);
  writeFileSync(
    messageFile,
    rewriteCommitMessage(message, model, effort, { preserveLone }),
    "utf8",
  );
}

const messageFile = process.argv[2];
if (!messageFile) fail(["the commit-msg hook did not receive a message file"]);

const root = repositoryRoot(process.cwd());
const message = readFileSync(messageFile, "utf8");
const active = activeAgentEnvironment();
if (active?.conflict) {
  fail([`conflicting active agent markers: ${active.conflict.join(", ")}`]);
}
let record = normalizeCacheRecord(readAttributionCache(root, active?.sessionId));

if (!record && active?.client === "codex") {
  const live = resolveCodexLiveCommit(root, active.sessionId);
  if (live) {
    writeAttributionCache(root, {
      version: 1,
      client: "codex",
      sessionId: active.sessionId,
      root,
      commonDir: repositoryCommonDirectory(root),
      model: live.model,
      effort: live.effort,
      errors: live.errors,
      uses: live.uses,
      capturedAt: new Date().toISOString(),
    });
    record = normalizeCacheRecord(readAttributionCache(root, active.sessionId));
  }
}

if (!record) {
  // Missing capture gets one fail-closed live-session resolution attempt.
  if (active?.client) {
    const resolved = resolveAttribution(
      active.client,
      { cwd: root, sessionId: active.sessionId },
      { root },
    );
    if (resolved.errors?.length) fail(resolved.errors);
    try {
      stamp(messageFile, message, root, resolved.model, resolved.effort);
    } catch (error) {
      fail([error.message]);
    }
    process.exit(0);
  }

  const messageErrors = validateCommitMessage(message);
  if (active || messageErrors.length > 0) {
    fail(["the CLI did not provide a fresh runtime metadata capture", ...messageErrors]);
  }
  process.exit(0);
}

const errors = validateCacheRecord(record, root);
if (errors.length > 0) fail(errors);

// Uses are interchangeable: reordering/skipped commits can't pick wrong semantics
// (preserve keys on HEAD message identity; spoof inherits HEAD's proven pair).
// Which agent ran the commit is honor-system.
try {
  stamp(messageFile, message, root, record.model, record.effort);
  record.uses -= 1;
  if (record.uses > 0) writeAttributionCache(root, record);
  else removeAttributionCache(root, record.sessionId);
} catch (error) {
  fail([error.message]);
}
