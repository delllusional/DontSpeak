#!/usr/bin/env node

import {
  commandFromHookInput,
  detectClient,
  ensureCommitMessageHook,
  gitCommitInvocations,
  hookWorkingDirectory,
  resolveAttribution,
  repositoryCommonDirectory,
  resolveRepositoryRoot,
  sessionIdFromInput,
  writeAttributionCache,
} from "./agent-attribution.mjs";

async function readStdin() {
  const chunks = [];
  for await (const chunk of process.stdin) chunks.push(chunk);
  return Buffer.concat(chunks).toString("utf8");
}

async function main() {
  const raw = await readStdin();
  const input = raw.trim() ? JSON.parse(raw) : {};
  const command = commandFromHookInput(input);
  const invocations = gitCommitInvocations(command, hookWorkingDirectory(input));
  if (invocations.length === 0) return;

  const rootByDirectory = new Map();
  const rootOf = (directory) => {
    if (!rootByDirectory.has(directory)) {
      rootByDirectory.set(directory, resolveRepositoryRoot(directory));
    }
    return rootByDirectory.get(directory);
  };
  // Cache root = first invocation whose repo resolves; other repos dropped.
  // No resolve → nothing to capture (commit fails on its own).
  let root;
  let uses = 0;
  for (const invocation of invocations) {
    const resolved = rootOf(invocation.workingDirectory);
    if (!resolved) continue;
    root ??= resolved;
    if (resolved === root) uses += 1;
  }
  if (!root) return;

  const client = detectClient(process.argv[2] ?? "auto", input);
  if (!client) throw new Error("could not identify the CLI client that is creating this commit");

  ensureCommitMessageHook(root);
  const resolved = resolveAttribution(client, input, { root });
  const sessionId = sessionIdFromInput(input);
  writeAttributionCache(root, {
    version: 1,
    client,
    sessionId,
    root,
    commonDir: repositoryCommonDirectory(root),
    model: resolved.model,
    effort: resolved.effort,
    errors: resolved.errors,
    uses,
    capturedAt: new Date().toISOString(),
  });
}

main().catch((error) => {
  console.error(`commit attribution capture failed: ${error.message}`);
  process.exitCode = 2;
});
