#!/usr/bin/env node

import {
  commandFromHookInput,
  detectClient,
  ensureCommitMessageHook,
  gitCommitWorkingDirectory,
  hookWorkingDirectory,
  repositoryRoot,
  resolveAttribution,
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
  const commitWorkingDirectory = gitCommitWorkingDirectory(command, hookWorkingDirectory(input));
  if (!commitWorkingDirectory) return;

  const client = detectClient(process.argv[2] ?? "auto", input);
  if (!client) throw new Error("could not identify the CLI client that is creating this commit");

  const root = repositoryRoot(commitWorkingDirectory);
  ensureCommitMessageHook(root);
  const resolved = resolveAttribution(client, input, { root });
  writeAttributionCache(root, {
    version: 1,
    client,
    sessionId: sessionIdFromInput(input),
    root,
    model: resolved.model,
    effort: resolved.effort,
    errors: resolved.errors,
    capturedAt: new Date().toISOString(),
  });
}

main().catch((error) => {
  console.error(`commit attribution capture failed: ${error.message}`);
  process.exitCode = 2;
});
