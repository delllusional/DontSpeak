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
if (active?.conflict) {
  fail([`conflicting active agent markers: ${active.conflict.join(", ")}`]);
}

if (!record) {
  // Some tool surfaces skip PreToolUse capture. Every client resolver gets one
  // chance to prove exact attribution from its active session store.
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
  if (record.uses > 0) writeAttributionCache(root, record, hooksDirectory);
  else removeAttributionCache(root, hooksDirectory);
} catch (error) {
  fail([error.message]);
}
