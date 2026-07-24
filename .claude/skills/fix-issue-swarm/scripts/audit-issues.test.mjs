import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const script = join(dirname(fileURLToPath(import.meta.url)), "audit-issues.mjs");

test("rejects a prohibited login before repository access", () => {
  const fixture = mkdtempSync(join(tmpdir(), "dontspeak-audit-account-"));
  try {
    const log = join(fixture, "calls.log");
    const fakeGh = join(fixture, "gh");
    writeFileSync(
      fakeGh,
      `#!/usr/bin/env node
import { appendFileSync } from "node:fs";
appendFileSync(process.env.FAKE_GH_LOG, process.argv.slice(2).join(" ") + "\\n");
if (process.argv[2] === "api" && process.argv[3] === "user") {
  process.stdout.write("axy-yanchenko\\n");
  process.exit(0);
}
process.exit(99);
`,
    );
    chmodSync(fakeGh, 0o755);

    const result = spawnSync(
      process.execPath,
      [script, "--login", "axy-yanchenko"],
      {
      encoding: "utf8",
      env: {
        ...process.env,
        FAKE_GH_LOG: log,
        PATH: `${fixture}${process.platform === "win32" ? ";" : ":"}${process.env.PATH}`,
      },
      },
    );

    assert.equal(result.status, 1);
    assert.match(result.stderr, /axy-yanchenko is prohibited/);
    assert.match(result.stderr, /switch to yanchenko/);
    assert.equal(readFileSync(log, "utf8"), "api user --jq .login\n");
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
});
