import { spawnSync } from "node:child_process";

const args = process.argv.slice(2);
if (args.length > 1 || args.includes("--help")) {
  console.error("usage: node scripts/check-commit-attribution.mjs [base-ref]");
  process.exit(args.includes("--help") ? 0 : 2);
}
const base = args[0] ?? "origin/main";

function git(...gitArgs) {
  const result = spawnSync("git", gitArgs, { encoding: "utf8" });
  if (result.status !== 0) {
    process.stderr.write(result.stderr);
    process.exit(result.status ?? 1);
  }
  return result.stdout;
}

const commits = git("rev-list", "--reverse", `${base}..HEAD`).trim().split(/\s+/).filter(Boolean);
if (commits.length === 0) {
  console.log(`no outgoing commits after ${base}`);
  process.exit(0);
}

const prohibited = /^(?:Co-Authored-By|Assisted-by|Generated-by|AI):/i;
let failed = false;
for (const commit of commits) {
  const message = git("show", "-s", "--format=%B", commit).trimEnd();
  const lines = message.split(/\r?\n/);
  const trailers = [];
  while (lines.length > 0 && lines.at(-1).startsWith("Agent:")) {
    trailers.unshift(lines.pop());
  }

  const errors = [];
  if (trailers.length === 0) errors.push("missing final Agent trailer");
  const seen = new Set();
  for (const trailer of trailers) {
    if (!/^Agent: \S+ \S+$/.test(trailer)) {
      errors.push(`malformed trailer: ${trailer}`);
    } else if (seen.has(trailer)) {
      errors.push(`duplicate trailer: ${trailer}`);
    }
    seen.add(trailer);
  }
  for (const line of lines) {
    if (line.startsWith("Agent:")) errors.push(`Agent trailer is not final: ${line}`);
    if (prohibited.test(line)) errors.push(`prohibited attribution: ${line}`);
  }

  const short = commit.slice(0, 12);
  if (errors.length === 0) {
    console.log(`clean ${short}`);
  } else {
    failed = true;
    console.error(`invalid attribution in ${short}:`);
    for (const error of errors) console.error(`  ${error}`);
  }
}
if (failed) process.exit(1);
