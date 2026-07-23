import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import {
  chmod,
  lstat,
  mkdtemp,
  mkdir,
  readFile,
  readlink,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const installer = join(repoRoot, "scripts/install/web/install.sh");

test("Windows PATH edits preserve unexpanded registry values and their type", async () => {
  const scripts = [
    "scripts/install/web/install.ps1",
    "scripts/install/bundle/uninstall.ps1",
  ];
  for (const relative of scripts) {
    const source = await readFile(join(repoRoot, relative), "utf8");
    assert.match(source, /RegistryValueOptions\]::DoNotExpandEnvironmentNames/);
    assert.match(source, /GetValueKind\('Path'\)/);
    assert.match(source, /SetValue\('Path', \$(?:userPath|keptPath), \$pathKind\)/);
    assert.match(source, /ExpandEnvironmentVariables\(\$_\)/);
    assert.match(source, /Publish-EnvironmentChange/);
    assert.match(source, /SendMessageTimeout/);
    assert.doesNotMatch(source, /\[Environment\]::(?:Get|Set)EnvironmentVariable\('Path'/);
  }
});

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
printf '#!/bin/sh\nexit 0\n' > "$DONTSPEAK_INSTALL_DIR/dontspeak-uninstall"
chmod +x "$DONTSPEAK_INSTALL_DIR/dontspeak-uninstall"
`,
  );
  // Relative paths + cwd: absolute Windows paths make GNU tar treat `C:` as a host.
  execFileSync(
    "tar",
    ["-czf", assetName, "-C", "package", assetName.replace(".tar.gz", "")],
    { cwd: root },
  );

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

  let shell = "sh";
  let shellArgs = [installer];
  if (process.platform === "win32") {
    const gitExecPath = execFileSync("git", ["--exec-path"], { encoding: "utf8" }).trim();
    shell = resolve(gitExecPath, "../../../bin/sh.exe");
    shellArgs = [
      "-c",
      `
HOME="$(cygpath -u "$HOME")"
TEST_FAKE_BIN="$(cygpath -u "$TEST_FAKE_BIN")"
TEST_ARCHIVE="$(cygpath -u "$TEST_ARCHIVE")"
TEST_CHECKSUMS="$(cygpath -u "$TEST_CHECKSUMS")"
DONTSPEAK_INSTALL_DIR="$(cygpath -u "$DONTSPEAK_INSTALL_DIR")"
export HOME TEST_FAKE_BIN TEST_ARCHIVE TEST_CHECKSUMS DONTSPEAK_INSTALL_DIR
PATH="$TEST_FAKE_BIN:$PATH"
export PATH
exec sh "$(cygpath -u "$1")"
`,
      "sh",
      installer,
    ];
  }

  const result = spawnSync(shell, shellArgs, {
    cwd: repoRoot,
    encoding: "utf8",
    env: {
      ...process.env,
      DISPLAY: "",
      HOME: join(root, "home"),
      PATH: process.platform === "win32" ? process.env.PATH : `${fakeBin}:${process.env.PATH}`,
      TEST_FAKE_BIN: fakeBin,
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

test(
  "macOS web installer replaces a stale CLI with the bundled launcher",
  { skip: process.platform === "win32" },
  async (t) => {
    const root = await mkdtemp(join(tmpdir(), "dontspeak-macos-installer-test-"));
    t.after(() => rm(root, { recursive: true, force: true }));

    const assetName = "dontspeak-0.3.8-dev-macos-aarch64.app.zip";
    const archive = join(root, assetName);
    const checksums = join(root, "checksums.txt");
    const fakeBin = join(root, "bin");
    const home = join(root, "home");
    const installDir = join(root, "install");
    const helper = join(root, "dontspeak");
    const uninstaller = join(root, "uninstall.sh");
    const wireLog = join(root, "wire.log");
    await mkdir(fakeBin);
    await mkdir(join(home, "Applications", "DontSpeak.app"), { recursive: true });
    await mkdir(installDir);
    await writeFile(archive, "fixture archive");
    await executable(
      helper,
      `#!/bin/sh
printf '%s\\n' "$*" >> "$TEST_WIRE_LOG"
`,
    );
    await executable(uninstaller, "#!/bin/sh\nexit 0\n");
    await executable(join(installDir, "dontspeak"), "#!/bin/sh\nexit 99\n");

    const archiveBytes = await readFile(archive);
    const sha256 = createHash("sha256").update(archiveBytes).digest("hex");
    await writeFile(checksums, `${sha256}  ${assetName}\n`);

    await executable(
      join(fakeBin, "uname"),
      `#!/bin/sh
case "$1" in
  -s) printf 'Darwin\\n' ;;
  -m) printf 'arm64\\n' ;;
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
    await executable(
      join(fakeBin, "ditto"),
      `#!/bin/sh
set -eu
[ "$1" = "-x" ] && [ "$2" = "-k" ]
out="$4/DontSpeak.app"
mkdir -p "$out/Contents/Helpers" "$out/Contents/Resources"
cp "$TEST_HELPER" "$out/Contents/Helpers/dontspeak"
cp "$TEST_UNINSTALLER" "$out/Contents/Resources/uninstall.sh"
chmod +x "$out/Contents/Helpers/dontspeak" "$out/Contents/Resources/uninstall.sh"
`,
    );
    for (const command of ["open", "osascript", "pkill"]) {
      await executable(join(fakeBin, command), "#!/bin/sh\nexit 0\n");
    }

    const result = spawnSync("sh", [installer], {
      cwd: repoRoot,
      encoding: "utf8",
      env: {
        ...process.env,
        HOME: home,
        PATH: `${fakeBin}:${process.env.PATH}`,
        TEST_ARCHIVE: archive,
        TEST_CHECKSUMS: checksums,
        TEST_HELPER: helper,
        TEST_UNINSTALLER: uninstaller,
        TEST_WIRE_LOG: wireLog,
        DONTSPEAK_INSTALL_DIR: installDir,
      },
    });

    assert.equal(result.status, 0, `installer failed:\n${result.stdout}\n${result.stderr}`);
    const launcher = join(installDir, "dontspeak");
    assert.equal((await lstat(launcher)).isSymbolicLink(), true);
    assert.equal(
      await readlink(launcher),
      join(home, "Applications", "DontSpeak.app", "Contents", "Helpers", "dontspeak"),
    );
    execFileSync(launcher, ["--version"], {
      env: { ...process.env, TEST_WIRE_LOG: wireLog },
    });
    assert.equal(await readFile(wireLog, "utf8"), "wire --reconcile\n--version\n");
    assert.match(result.stdout, new RegExp(`launcher placed: ${launcher}`));
  },
);
