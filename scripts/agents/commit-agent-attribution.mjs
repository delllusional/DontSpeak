#!/usr/bin/env node

import { readFileSync, writeFileSync } from "node:fs";
import {
  activeAgentEnvironment,
  readAttributionCache,
  removeAttributionCache,
  repositoryRoot,
  rewriteCommitMessage,
  validateCacheRecord,
  validateCommitMessage,
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
const message = readFileSync(messageFile, "utf8");
const record = readAttributionCache(root);

if (!record) {
  const messageErrors = validateCommitMessage(message);
  if (activeAgentEnvironment() || messageErrors.length > 0) {
    fail(["the CLI did not provide a fresh runtime metadata capture", ...messageErrors]);
  }
  process.exit(0);
}

const errors = validateCacheRecord(record, root);
if (errors.length > 0) fail(errors);

try {
  writeFileSync(messageFile, rewriteCommitMessage(message, record.model, record.effort), "utf8");
  removeAttributionCache(root);
} catch (error) {
  fail([error.message]);
}
