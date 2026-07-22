import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  createTaskWorktree,
  mainWorktree,
  parseWorktreeList,
  refreshMain,
} from "./task-worktree.mjs";

const script = join(dirname(fileURLToPath(import.meta.url)), "task-worktree.mjs");

function temporaryDirectory(t) {
  const directory = mkdtempSync(join(tmpdir(), "dontspeak-task-worktree-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  return directory;
}

function git(cwd, env, ...args) {
  const result = spawnSync("git", args, { cwd, env, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr || `git ${args.join(" ")} failed`);
  return result.stdout.trim();
}

function fixture(t) {
  const root = temporaryDirectory(t);
  const config = join(root, "gitconfig");
  writeFileSync(config, "", "utf8");
  const env = { ...process.env, GIT_CONFIG_GLOBAL: config, GIT_CONFIG_NOSYSTEM: "1" };
  const savedGlobal = process.env.GIT_CONFIG_GLOBAL;
  const savedNoSystem = process.env.GIT_CONFIG_NOSYSTEM;
  process.env.GIT_CONFIG_GLOBAL = config;
  process.env.GIT_CONFIG_NOSYSTEM = "1";
  t.after(() => {
    if (savedGlobal === undefined) delete process.env.GIT_CONFIG_GLOBAL;
    else process.env.GIT_CONFIG_GLOBAL = savedGlobal;
    if (savedNoSystem === undefined) delete process.env.GIT_CONFIG_NOSYSTEM;
    else process.env.GIT_CONFIG_NOSYSTEM = savedNoSystem;
  });
  const remote = join(root, "remote.git");
  const main = join(root, "main");
  const publisher = join(root, "publisher");
  const feature = join(root, "feature");

  git(root, env, "init", "--bare", "-q", remote);
  git(root, env, "init", "-q", main);
  git(main, env, "config", "user.name", "Task Worktree Test");
  git(main, env, "config", "user.email", "test@example.com");
  writeFileSync(join(main, ".gitignore"), ".worktrees/\n", "utf8");
  writeFileSync(join(main, "tracked.txt"), "initial\n", "utf8");
  git(main, env, "add", ".");
  git(main, env, "commit", "-qm", "initial");
  git(main, env, "branch", "-M", "main");
  git(main, env, "remote", "add", "origin", remote);
  git(main, env, "push", "-qu", "origin", "main");
  git(remote, env, "symbolic-ref", "HEAD", "refs/heads/main");
  git(main, env, "worktree", "add", "-q", "-b", "existing-task", feature, "main");
  git(root, env, "clone", "-q", remote, publisher);
  git(publisher, env, "config", "user.name", "Task Worktree Publisher");
  git(publisher, env, "config", "user.email", "publisher@example.com");

  return { env, feature, main, publisher, remote };
}

function publish(fixture, contents, ref = "main") {
  writeFileSync(join(fixture.publisher, "tracked.txt"), `${contents}\n`, "utf8");
  git(fixture.publisher, fixture.env, "add", "tracked.txt");
  git(fixture.publisher, fixture.env, "commit", "-qm", contents);
  git(fixture.publisher, fixture.env, "push", "-q", "origin", `HEAD:${ref}`);
  return git(fixture.publisher, fixture.env, "rev-parse", "HEAD");
}

test("parses nul-delimited worktree records", () => {
  assert.deepEqual(
    parseWorktreeList("worktree /repo\0HEAD abc\0branch refs/heads/main\0\0worktree /task\0HEAD def\0detached\0\0"),
    [
      { worktree: "/repo", HEAD: "abc", branch: "refs/heads/main" },
      { worktree: "/task", HEAD: "def", detached: true },
    ],
  );
});

test("creates a unique task worktree from freshly pulled main", (t) => {
  const repo = fixture(t);
  const upstream = publish(repo, "upstream");
  const created = createTaskWorktree(repo.feature, "feat/parallel-agent", {
    log: () => {},
  });

  assert.equal(created.baseCommit, upstream);
  assert.equal(realpathSync(created.mainWorktree), realpathSync(repo.main));
  assert.equal(realpathSync(created.worktree), realpathSync(join(repo.main, ".worktrees", "feat-parallel-agent")));
  assert.equal(git(created.worktree, repo.env, "branch", "--show-current"), "feat/parallel-agent");
  assert.equal(git(created.worktree, repo.env, "rev-parse", "HEAD"), upstream);
  assert.equal(git(repo.main, repo.env, "status", "--short"), "");
});

test("refuses to update a main worktree with tracked changes", (t) => {
  const repo = fixture(t);
  writeFileSync(join(repo.main, "tracked.txt"), "dirty\n", "utf8");

  assert.throws(() => refreshMain(repo.feature, { log: () => {} }), /main worktree has tracked changes/);
  assert.equal(realpathSync(mainWorktree(repo.feature)), realpathSync(repo.main));
  assert.equal(readFileSync(join(repo.main, "tracked.txt"), "utf8"), "dirty\n");
});

test("Claude hook refreshes main before creating an ordinary worktree", (t) => {
  const repo = fixture(t);
  const upstream = publish(repo, "hook upstream");
  const result = spawnSync(process.execPath, [script, "claude-hook"], {
    cwd: repo.feature,
    env: repo.env,
    encoding: "utf8",
    input: JSON.stringify({
      hook_event_name: "WorktreeCreate",
      cwd: repo.feature,
      name: "hook-task",
    }),
  });

  assert.equal(result.status, 0, result.stderr);
  const worktree = result.stdout.trim();
  assert.equal(realpathSync(worktree), realpathSync(join(repo.main, ".worktrees", "hook-task")));
  assert.equal(git(worktree, repo.env, "branch", "--show-current"), "worktree-hook-task");
  assert.equal(git(worktree, repo.env, "rev-parse", "HEAD"), upstream);
});

test("Claude hook creates pull-request worktrees without polluting stdout", (t) => {
  const repo = fixture(t);
  git(repo.publisher, repo.env, "switch", "-qc", "pull-request");
  const pullRequestHead = publish(repo, "pull request", "refs/pull/7/head");
  const result = spawnSync(process.execPath, [script, "claude-hook"], {
    cwd: repo.feature,
    env: repo.env,
    encoding: "utf8",
    input: JSON.stringify({
      hook_event_name: "WorktreeCreate",
      cwd: repo.feature,
      name: "pr-7",
    }),
  });

  assert.equal(result.status, 0, result.stderr);
  const worktree = result.stdout.trim();
  assert.equal(realpathSync(worktree), realpathSync(join(repo.main, ".worktrees", "pr-7")));
  assert.equal(git(worktree, repo.env, "branch", "--show-current"), "worktree-pr-7");
  assert.equal(git(worktree, repo.env, "rev-parse", "HEAD"), pullRequestHead);
});
