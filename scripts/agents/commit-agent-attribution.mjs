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

const messageFile = process.argv[2];
if (!messageFile) fail(["the commit-msg hook did not receive a message file"]);

const root = repositoryRoot(process.cwd());
const hooksDirectory = privateHooksDirectory(root);
const message = readFileSync(messageFile, "utf8");
const record = normalizeCacheRecord(readAttributionCache(root, hooksDirectory));

if (!record) {
  const messageErrors = validateCommitMessage(message);
  if (activeAgentEnvironment() || messageErrors.length > 0) {
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
const preserveLone = messageMatchesHead(message, root);

try {
  writeFileSync(messageFile, rewriteCommitMessage(message, record.model, record.effort, { preserveLone }), "utf8");
  record.uses -= 1;
  if (record.uses > 0) writeAttributionCache(root, record, hooksDirectory);
  else removeAttributionCache(root, hooksDirectory);
} catch (error) {
  fail([error.message]);
}
