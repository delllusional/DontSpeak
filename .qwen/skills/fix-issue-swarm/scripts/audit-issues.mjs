#!/usr/bin/env node

import { spawnSync } from "node:child_process";

function fail(message) {
  process.stderr.write(`${message}\n`);
  process.exit(1);
}

function runGh(args) {
  const result = spawnSync("gh", args, { encoding: "utf8" });
  if (result.error) fail(`Failed to run gh: ${result.error.message}`);
  if (result.status !== 0) {
    process.stderr.write(result.stderr);
    process.exit(result.status ?? 1);
  }
  return result.stdout.trim();
}

function argument(name, fallback) {
  const index = process.argv.indexOf(name);
  if (index === -1) return fallback;
  const value = process.argv[index + 1];
  if (!value || value.startsWith("--")) fail(`Missing value for ${name}`);
  return value;
}

const repo = argument("--repo", "delllusional/DontSpeak");
const requiredLogin = "yanchenko";
const login = runGh(["api", "user", "--jq", ".login"]);
if (login !== requiredLogin) {
  fail(
    `GitHub account ${login} is prohibited for ${repo}; switch to ${requiredLogin} before any repository operation.`,
  );
}
const repoState = JSON.parse(
  runGh([
    "repo",
    "view",
    repo,
    "--json",
    "viewerPermission,defaultBranchRef",
  ]),
);

const allowed = new Set(["WRITE", "MAINTAIN", "ADMIN"]);
if (!allowed.has(repoState.viewerPermission)) {
  fail(
    `GitHub account ${login} has ${repoState.viewerPermission} permission on ${repo}; write access is required.`,
  );
}

const issues = JSON.parse(
  runGh([
    "issue",
    "list",
    "--repo",
    repo,
    "--state",
    "open",
    "--limit",
    "100",
    "--json",
    "number,title,body,issueType,assignees,author,createdAt,updatedAt,url,comments",
  ]),
);
for (const issue of issues) {
  const values = JSON.parse(
    runGh([
      "api",
      "-H",
      "X-GitHub-Api-Version: 2026-03-10",
      `repos/${repo}/issues/${issue.number}/issue-field-values`,
    ]),
  );
  issue.issueFields = Object.fromEntries(
    values.map((field) => [
      field.issue_field_name,
      field.single_select_option?.name ?? field.value,
    ]),
  );
}
const pullRequests = JSON.parse(
  runGh([
    "pr",
    "list",
    "--repo",
    repo,
    "--state",
    "open",
    "--limit",
    "100",
    "--json",
    "number,title,headRefName,baseRefName,author,createdAt,updatedAt,url,closingIssuesReferences",
  ]),
);
const defaultBranch = repoState.defaultBranchRef?.name ?? "main";
const mainSha = runGh([
  "api",
  `repos/${repo}/commits/${defaultBranch}`,
  "--jq",
  ".sha",
]);

process.stdout.write(
  `${JSON.stringify(
    {
      snapshotAt: new Date().toISOString(),
      repo,
      login,
      permission: repoState.viewerPermission,
      defaultBranch,
      mainSha,
      pullRequests,
      issues,
    },
    null,
    2,
  )}\n`,
);
