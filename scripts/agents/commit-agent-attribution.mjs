#!/usr/bin/env node

import { readFileSync, writeFileSync } from "node:fs";
import {
  activeAgentEnvironment,
  messageMatchesHead,
  normalizeCacheRecord,
  privateHooksDirectory,
  readAttributionCache,
  removeAttributionCache,
  repositoryRoot,
  resolveAttribution,
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
const hooksDirectory = privateHooksDirectory(root);
const message = readFileSync(messageFile, "utf8");
const record = normalizeCacheRecord(readAttributionCache(root, hooksDirectory));
const active = activeAgentEnvironment();

if (!record) {
  // Grok tool shells often skip PreToolUse capture (GROK_AGENT without
  // GROK_SESSION_ID, or project hooks not trusted). Prove attribution live from
  // ~/.grok/sessions + active_sessions.json so the trailer is still correct.
  if (active?.client === "grok") {
    const resolved = resolveAttribution(
      "grok",
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

// Uses are interchangeable, so chain reordering (|| branches, skipped commits)
// cannot select wrong semantics: the preserve decision keys on message
// identity with HEAD, and the worst spoof inherits HEAD's own proven pair.
// Which agent actually ran the commit stays honor-system.
try {
  stamp(messageFile, message, root, record.model, record.effort);
  record.uses -= 1;
  if (record.uses > 0) writeAttributionCache(root, record, hooksDirectory);
  else removeAttributionCache(root, hooksDirectory);
} catch (error) {
  fail([error.message]);
}
