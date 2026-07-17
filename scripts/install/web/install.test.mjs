import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import { chmod, mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const installer = join(repoRoot, "scripts/install/web/install.sh");

async function executable(path, contents) {
  await writeFile(path, contents);
  await chmod(path, 0o755);
}

test("web installer verifies an asset against a CRLF checksum manifest", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "dontspeak-installer-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));

  const assetName = "dontspeak-0.3.2-linux-x86_64.tar.gz";
  const packageRoot = join(root, "package", "dontspeak-0.3.2-linux-x86_64");
  const archive = join(root, assetName);
  const checksums = join(root, "checksums.txt");
  const fakeBin = join(root, "bin");
  const installDir = join(root, "install");
  await mkdir(packageRoot, { recursive: true });
  await mkdir(fakeBin);

  await executable(
    join(packageRoot, "install.sh"),
    `#!/bin/sh
set -eu
mkdir -p "$DONTSPEAK_INSTALL_DIR"
: > "$DONTSPEAK_INSTALL_DIR/dontspeak-uninstall"
chmod +x "$DONTSPEAK_INSTALL_DIR/dontspeak-uninstall"
`,
  );
  execFileSync("tar", [
    "-czf",
    archive,
    "-C",
    join(root, "package"),
    assetName.replace(".tar.gz", ""),
  ]);

  const archiveBytes = await readFile(archive);
  const sha256 = createHash("sha256").update(archiveBytes).digest("hex");
  await writeFile(checksums, `${sha256}  ${assetName}\r\n`);

  await executable(
    join(fakeBin, "uname"),
    `#!/bin/sh
case "$1" in
  -s) printf 'Linux\\n' ;;
  -m) printf 'x86_64\\n' ;;
  *) exit 2 ;;
esac
`,
  );
  await executable(
    join(fakeBin, "curl"),
    `#!/bin/sh
set -eu
[ "$1" = "-fsSL" ]
shift
if [ "\${1:-}" = "-o" ]; then
  cp "$TEST_ARCHIVE" "$2"
  exit 0
fi
case "$1" in
  */releases/latest)
    printf '%s\\n' \\
      '{"browser_download_url":"https://example.test/${assetName}"}' \\
      '{"browser_download_url":"https://example.test/checksums.txt"}'
    ;;
  */checksums.txt) exec cat "$TEST_CHECKSUMS" ;;
  *) exit 22 ;;
esac
`,
  );

  const result = spawnSync("sh", [installer], {
    cwd: repoRoot,
    encoding: "utf8",
    env: {
      ...process.env,
      DISPLAY: "",
      HOME: join(root, "home"),
      PATH: `${fakeBin}:${process.env.PATH}`,
      TEST_ARCHIVE: archive,
      TEST_CHECKSUMS: checksums,
      DONTSPEAK_INSTALL_DIR: installDir,
      DONTSPEAK_NO_AUTOSTART: "1",
      WAYLAND_DISPLAY: "",
    },
  });

  assert.equal(result.status, 0, `installer failed:\n${result.stdout}\n${result.stderr}`);
  assert.match(result.stdout, new RegExp(`verified ${assetName} \\(sha256 ok\\)`));
});
