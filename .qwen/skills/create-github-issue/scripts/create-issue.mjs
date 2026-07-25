#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";

function fail(message, status = 1) {
  process.stderr.write(`${message}\n`);
  process.exit(status);
}

function runGh(args, input) {
  const result = spawnSync("gh", args, {
    encoding: "utf8",
    input,
  });
  if (result.error) fail(`Failed to run gh: ${result.error.message}`);
  if (result.status !== 0) {
    process.stderr.write(result.stderr);
    process.exit(result.status ?? 1);
  }
  return result.stdout.trim();
}

function value(name) {
  const index = process.argv.indexOf(name);
  if (index === -1) fail(`Missing required argument ${name}`, 2);
  const result = process.argv[index + 1];
  if (!result || result.startsWith("--")) fail(`Missing value for ${name}`, 2);
  return result;
}

const knownArguments = new Set([
  "--repo",
  "--title",
  "--body-file",
  "--type",
  "--priority",
  "--effort",
  "--dry-run",
]);
for (const argument of process.argv.slice(2)) {
  if (argument.startsWith("--") && !knownArguments.has(argument)) {
    fail(`Unknown argument ${argument}`, 2);
  }
}

const repo = value("--repo");
const title = value("--title");
const bodyFile = value("--body-file");
const issueType = value("--type");
const priority = value("--priority");
const effort = value("--effort");
const dryRun = process.argv.includes("--dry-run");

const repoParts = repo.split("/");
if (repoParts.length !== 2 || repoParts.some((part) => part.length === 0)) {
  fail("--repo must be OWNER/REPO", 2);
}
const [owner] = repoParts;

const allowedTypes = new Set(["Bug", "Feature", "Task"]);
const allowedPriorities = new Set(["Urgent", "High", "Medium", "Low"]);
const allowedEfforts = new Set(["High", "Medium", "Low"]);
if (!allowedTypes.has(issueType)) fail(`Unsupported issue type: ${issueType}`, 2);
if (!allowedPriorities.has(priority)) fail(`Unsupported priority: ${priority}`, 2);
if (!allowedEfforts.has(effort)) fail(`Unsupported effort: ${effort}`, 2);

let body;
try {
  body = readFileSync(bodyFile, "utf8");
} catch (error) {
  fail(`Cannot read body file ${bodyFile}: ${error.message}`, 2);
}
if (title.trim().length === 0 || body.trim().length === 0) {
  fail("Issue title and body must not be empty", 2);
}

const login = runGh(["api", "user", "--jq", ".login"]);
if (login !== "yanchenko") {
  fail(`GitHub account ${login} is prohibited for ${repo}; use yanchenko`);
}

const permission = runGh([
  "repo",
  "view",
  repo,
  "--json",
  "viewerPermission",
  "--jq",
  ".viewerPermission",
]);
if (!new Set(["WRITE", "MAINTAIN", "ADMIN"]).has(permission)) {
  fail(`GitHub account ${login} has ${permission} permission on ${repo}`);
}

const apiHeaders = ["-H", "X-GitHub-Api-Version: 2026-03-10"];
const issueTypes = JSON.parse(
  runGh(["api", ...apiHeaders, `orgs/${owner}/issue-types`]),
);
if (!issueTypes.some((candidate) => candidate.name === issueType && candidate.is_enabled)) {
  fail(`Issue type ${issueType} is not enabled for ${owner}`);
}

const fields = JSON.parse(
  runGh(["api", ...apiHeaders, `orgs/${owner}/issue-fields`]),
);
function fieldSelection(fieldName, optionName) {
  const field = fields.find((candidate) => candidate.name === fieldName);
  if (!field || field.data_type !== "single_select") {
    fail(`Required single-select issue field ${fieldName} is unavailable`);
  }
  if (!field.options?.some((option) => option.name === optionName)) {
    fail(`Issue field ${fieldName} has no option ${optionName}`);
  }
  return { field_id: field.id, value: optionName };
}

const issueFieldValues = [
  fieldSelection("Priority", priority),
  fieldSelection("Effort", effort),
];

if (dryRun) {
  process.stdout.write(
    `${JSON.stringify(
      {
        dryRun: true,
        repo,
        login,
        permission,
        title,
        type: issueType,
        priority,
        effort,
      },
      null,
      2,
    )}\n`,
  );
  process.exit(0);
}

const url = runGh([
  "issue",
  "create",
  "--repo",
  repo,
  "--title",
  title,
  "--body",
  body,
  "--type",
  issueType,
]);
const number = Number(new URL(url).pathname.split("/").at(-1));
if (!Number.isInteger(number)) {
  fail(`Issue was created but its number could not be parsed: ${url}`);
}

const payload = JSON.stringify({ issue_field_values: issueFieldValues });
runGh(
  [
    "api",
    "--method",
    "POST",
    ...apiHeaders,
    `repos/${repo}/issues/${number}/issue-field-values`,
    "--input",
    "-",
  ],
  payload,
);

process.stdout.write(
  `${JSON.stringify(
    {
      number,
      url,
      type: issueType,
      priority,
      effort,
    },
    null,
    2,
  )}\n`,
);
