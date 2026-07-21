import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

function gitResult(cwd, ...args) {
  return spawnSync("git", args, { cwd, encoding: "utf8" });
}

function git(cwd, ...args) {
  const result = gitResult(cwd, ...args);
  if (result.error) {
    throw new Error(`git ${args.join(" ")} failed to start: ${result.error.message}`);
  }
  if (result.status !== 0) {
    throw new Error((result.stderr ?? "").trim() || `git ${args.join(" ")} failed`);
  }
  return (result.stdout ?? "").trim();
}

function logGit(result, log) {
  for (const output of [result.stdout, result.stderr]) {
    const text = (output ?? "").trim();
    if (text) log(`${text}\n`);
  }
}

export function parseWorktreeList(output) {
  const worktrees = [];
  let current = {};
  for (const field of output.split("\0")) {
    if (field === "") {
      if (current.worktree) worktrees.push(current);
      current = {};
      continue;
    }
    const separator = field.indexOf(" ");
    const key = separator === -1 ? field : field.slice(0, separator);
    const value = separator === -1 ? true : field.slice(separator + 1);
    current[key] = value;
  }
  if (current.worktree) worktrees.push(current);
  return worktrees;
}

export function mainWorktree(cwd) {
  const output = git(cwd, "worktree", "list", "--porcelain", "-z");
  const matches = parseWorktreeList(`${output}\0`).filter(
    (worktree) => worktree.branch === "refs/heads/main",
  );
  if (matches.length !== 1) {
    throw new Error(`expected exactly one worktree on main; found ${matches.length}`);
  }
  return resolve(matches[0].worktree);
}

export function refreshMain(cwd, { log = (message) => process.stderr.write(message) } = {}) {
  const worktree = mainWorktree(cwd);
  const tracked = git(worktree, "status", "--porcelain=v1", "--untracked-files=no");
  if (tracked) {
    throw new Error(`main worktree has tracked changes:\n${tracked}`);
  }

  const pulled = gitResult(worktree, "pull", "--ff-only", "origin", "main");
  logGit(pulled, log);
  if (pulled.error) {
    throw new Error(`git pull failed to start: ${pulled.error.message}`);
  }
  if (pulled.status !== 0) {
    throw new Error((pulled.stderr ?? "").trim() || "git pull --ff-only origin main failed");
  }

  return {
    mainWorktree: worktree,
    baseCommit: git(worktree, "rev-parse", "HEAD"),
  };
}

function validateBranch(cwd, branch) {
  if (!branch || branch === "main" || branch.startsWith("-")) {
    throw new Error("task branch must be a non-main branch name");
  }
  const result = gitResult(cwd, "check-ref-format", "--branch", branch);
  if (result.status !== 0) throw new Error(`invalid task branch: ${branch}`);
}

function defaultWorktreeName(branch) {
  const name = branch
    .replace(/[^A-Za-z0-9._-]+/g, "-")
    .replace(/^[.-]+|[.-]+$/g, "");
  if (!name) throw new Error(`cannot derive a worktree name from branch: ${branch}`);
  return name;
}

function validateWorktreeName(name) {
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(name) || name.endsWith(".")) {
    throw new Error(`invalid worktree name: ${name}`);
  }
  if (/^(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(name)) {
    throw new Error(`worktree name is reserved on Windows: ${name}`);
  }
}

function branchExists(cwd, branch) {
  const result = gitResult(cwd, "show-ref", "--verify", "--quiet", `refs/heads/${branch}`);
  if (result.status === 0) return true;
  if (result.status === 1) return false;
  throw new Error((result.stderr ?? "").trim() || `could not inspect branch ${branch}`);
}

function createAtRevision(main, branch, name, revision) {
  validateBranch(main, branch);
  validateWorktreeName(name);
  if (branchExists(main, branch)) throw new Error(`task branch already exists: ${branch}`);

  const parent = resolve(main, ".worktrees");
  const target = resolve(parent, name);
  if (dirname(target) !== parent || !target.startsWith(`${parent}${sep}`)) {
    throw new Error(`worktree path escapes ${parent}`);
  }
  if (existsSync(target)) throw new Error(`task worktree path already exists: ${target}`);

  git(main, "worktree", "add", "-b", branch, target, revision);
  const actualCommit = git(target, "rev-parse", "HEAD");
  const actualBranch = git(target, "branch", "--show-current");
  if (actualCommit !== revision || actualBranch !== branch) {
    throw new Error(`created worktree verification failed: ${target}`);
  }
  return { worktree: target, branch, baseCommit: revision, mainWorktree: main };
}

export function createTaskWorktree(cwd, branch, options = {}) {
  const refreshed = refreshMain(cwd, options);
  const name = options.name ?? defaultWorktreeName(branch);
  return createAtRevision(refreshed.mainWorktree, branch, name, refreshed.baseCommit);
}

export function createPullRequestWorktree(cwd, number, name, options = {}) {
  if (!Number.isSafeInteger(number) || number <= 0) throw new Error(`invalid pull request: ${number}`);
  const main = mainWorktree(cwd);
  for (const args of [
    ["fetch", "origin", "main"],
    ["fetch", "origin", `pull/${number}/head`],
  ]) {
    const fetched = gitResult(main, ...args);
    logGit(fetched, options.log ?? ((message) => process.stderr.write(message)));
    if (fetched.error) throw new Error(`git fetch failed to start: ${fetched.error.message}`);
    if (fetched.status !== 0) {
      throw new Error((fetched.stderr ?? "").trim() || `git ${args.join(" ")} failed`);
    }
  }
  const revision = git(main, "rev-parse", "FETCH_HEAD");
  return createAtRevision(main, `worktree-pr-${number}`, name, revision);
}

function parseCreateArguments(args) {
  const branch = args[0];
  let name;
  for (let index = 1; index < args.length; index += 1) {
    if (args[index] !== "--name" || !args[index + 1] || name !== undefined) {
      throw new Error("usage: task-worktree.mjs create <branch> [--name <name>]");
    }
    name = args[index + 1];
    index += 1;
  }
  return { branch, name };
}

function readHookInput() {
  let input;
  try {
    input = JSON.parse(readFileSync(0, "utf8"));
  } catch (error) {
    throw new Error(`invalid WorktreeCreate input: ${error.message}`);
  }
  if (input.hook_event_name !== "WorktreeCreate" || typeof input.name !== "string") {
    throw new Error("expected Claude WorktreeCreate input with a name");
  }
  return input;
}

async function main() {
  const [command, ...args] = process.argv.slice(2);
  if (command === "refresh" && args.length === 0) {
    process.stdout.write(`${JSON.stringify(refreshMain(process.cwd()))}\n`);
    return;
  }
  if (command === "create") {
    const { branch, name } = parseCreateArguments(args);
    if (!branch) throw new Error("usage: task-worktree.mjs create <branch> [--name <name>]");
    process.stdout.write(`${JSON.stringify(createTaskWorktree(process.cwd(), branch, { name }))}\n`);
    return;
  }
  if (command === "claude-hook" && args.length === 0) {
    const input = readHookInput();
    const pullRequest = /^pr-(\d+)$/.exec(input.name);
    const created = pullRequest
      ? createPullRequestWorktree(input.cwd ?? process.cwd(), Number(pullRequest[1]), input.name)
      : createTaskWorktree(input.cwd ?? process.cwd(), `worktree-${input.name}`, { name: input.name });
    process.stdout.write(`${created.worktree}\n`);
    return;
  }
  throw new Error(
    "usage: task-worktree.mjs refresh | create <branch> [--name <name>] | claude-hook",
  );
}

const invoked = process.argv[1]
  && pathToFileURL(resolve(process.argv[1])).href === pathToFileURL(fileURLToPath(import.meta.url)).href;
if (invoked) {
  main().catch((error) => {
    process.stderr.write(`task-worktree: ${error.message}\n`);
    process.exitCode = 1;
  });
}

