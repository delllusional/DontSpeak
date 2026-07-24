import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import {
  chmod,
  lstat,
  mkdtemp,
  mkdir,
  readdir,
  readFile,
  readlink,
  rm,
  utimes,
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

// The macOS installer fixture: a redirected HOME/install dir plus PATH-injected fakes for
// every external command the Darwin branch runs. `osascript` (the quit step) and `open` (the
// launch step) bracket the destination-locked section, so the concurrency test overrides those
// two to witness when a process enters and leaves it.
async function macosFixture(root, { osascript, open, extraEnv } = {}) {
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

  // `-n` is load-bearing: the destination lock records "<pid> <uname -n>" as its owner. Without
  // it the fake exits 2, the owner line becomes "<pid> " and the host guard compares "" to ""
  // — the host-mismatch branch would never be covered by the only end-to-end macOS run.
  await executable(
    join(fakeBin, "uname"),
    `#!/bin/sh
case "$1" in
  -s) printf 'Darwin\\n' ;;
  -m) printf 'arm64\\n' ;;
  -n) printf 'test-host\\n' ;;
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
  await executable(join(fakeBin, "osascript"), osascript ?? "#!/bin/sh\nexit 0\n");
  await executable(join(fakeBin, "open"), open ?? "#!/bin/sh\nexit 0\n");
  // Faking pkill keeps the run off the developer's / runner's live DontSpeak and ds-helper.
  await executable(join(fakeBin, "pkill"), "#!/bin/sh\nexit 0\n");

  return {
    home,
    installDir,
    wireLog,
    fakeBin,
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
      ...extraEnv,
    },
  };
}

function runAsync(command, args, options) {
  return new Promise((resolvePromise) => {
    const child = spawn(command, args, { ...options, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.on("close", (status) => resolvePromise({ status, stdout, stderr }));
  });
}

// The POSIX destination-lock block ships duplicated in both installers, so the tests that
// exercise it directly run the shipped bytes rather than a paraphrase.
function extractLockBlock(source, begin, end) {
  const from = source.indexOf(begin);
  const to = source.indexOf(end);
  assert.ok(from !== -1 && to > from, "destination-lock markers not found");
  return source.slice(from, to + end.length);
}

async function posixLockDriver(root, name, sourcePath = installer) {
  const source = await readFile(sourcePath, "utf8");
  const block = extractLockBlock(
    source,
    "# -- BEGIN destination lock",
    "# -- END destination lock -----------------------------------------------------",
  );
  const driver = join(root, name);
  await executable(
    driver,
    `#!/bin/sh
set -eu
${block}
ds_lock_acquire "$1"
printf 'entered\\n'
if [ -n "\${2:-}" ]; then
  printf 'enter %s\\n' "$2" >> "$3"
  sleep 1
  printf 'exit %s\\n' "$2" >> "$3"
fi
ds_lock_release
`,
  );
  return driver;
}

test(
  "public and development installers serialize the same destination",
  { skip: process.platform === "win32" },
  async (t) => {
    const root = await mkdtemp(join(tmpdir(), "dontspeak-dev-public-lock-test-"));
    t.after(() => rm(root, { recursive: true, force: true }));
    const destination = join(root, "bin", "dontspeak");
    const sectionLog = join(root, "sections.log");
    const publicDriver = await posixLockDriver(root, "public.sh");
    const devDriver = await posixLockDriver(
      root,
      "dev.sh",
      join(repoRoot, "scripts/install/lib/destination-lock.sh"),
    );
    const env = { ...process.env, DONTSPEAK_INSTALL_LOCK_WAIT: "10" };

    const runs = await Promise.all([
      runAsync("sh", [publicDriver, destination, "public", sectionLog], { cwd: root, env }),
      runAsync("sh", [devDriver, destination, "dev", sectionLog], { cwd: root, env }),
    ]);
    for (const run of runs) {
      assert.equal(run.status, 0, `lock driver failed:\n${run.stdout}\n${run.stderr}`);
    }

    const sections = await readFile(sectionLog, "utf8");
    assert.match(
      sections,
      /^(enter public\nexit public\nenter dev\nexit dev\n|enter dev\nexit dev\nenter public\nexit public\n)$/,
    );
  },
);

// A pid that is genuinely gone: an unreaped zombie still answers `kill -0`, which would send
// the dead-owner row down the age rule instead and hang it for the whole timeout.
async function reapedPid() {
  for (let attempt = 0; attempt < 5; attempt++) {
    const child = spawn("sh", ["-c", "exit 0"], { stdio: "ignore" });
    const pid = child.pid;
    await new Promise((resolvePromise) => child.on("close", resolvePromise));
    try {
      process.kill(pid, 0);
    } catch {
      return pid;
    }
  }
  throw new Error("could not obtain a non-answering pid");
}

test(
  "macOS web installer replaces a stale CLI with the bundled launcher",
  { skip: process.platform === "win32" },
  async (t) => {
    const root = await mkdtemp(join(tmpdir(), "dontspeak-macos-installer-test-"));
    t.after(() => rm(root, { recursive: true, force: true }));
    const { env, home, installDir, wireLog } = await macosFixture(root);

    const result = spawnSync("sh", [installer], { cwd: repoRoot, encoding: "utf8", env });

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

test(
  "installer lock serializes concurrent macOS finalizations",
  { skip: process.platform === "win32" },
  async (t) => {
    const root = await mkdtemp(join(tmpdir(), "dontspeak-macos-lock-test-"));
    t.after(() => rm(root, { recursive: true, force: true }));

    const sectionLog = join(root, "sections.log");
    const ownerLog = join(root, "owners.log");
    const applications = join(root, "home", "Applications");
    const { env } = await macosFixture(root, {
      osascript: `#!/bin/sh
printf 'enter\\n' >> "$TEST_SECTION_LOG"
cat "$TEST_LOCK_OWNER" >> "$TEST_OWNER_LOG" 2>/dev/null || :
sleep 2
`,
      open: `#!/bin/sh
printf 'exit\\n' >> "$TEST_SECTION_LOG"
`,
      extraEnv: {
        TEST_SECTION_LOG: sectionLog,
        TEST_OWNER_LOG: ownerLog,
        TEST_LOCK_OWNER: join(applications, ".DontSpeak.app.ds-install.lock", "owner"),
        DONTSPEAK_INSTALL_LOCK_WAIT: "30",
      },
    });

    const runs = await Promise.all([
      runAsync("sh", [installer], { cwd: repoRoot, env }),
      runAsync("sh", [installer], { cwd: repoRoot, env }),
    ]);
    for (const run of runs) {
      assert.equal(run.status, 0, `installer failed:\n${run.stdout}\n${run.stderr}`);
    }

    // Strict nesting: an unserialized pair interleaves as enter,enter,...
    assert.equal(await readFile(sectionLog, "utf8"), "enter\nexit\nenter\nexit\n");

    const owners = (await readFile(ownerLog, "utf8")).trimEnd().split("\n");
    assert.equal(owners.length, 2);
    for (const owner of owners) assert.match(owner, /^\d+ test-host$/);
    assert.notEqual(owners[0], owners[1]);

    const bundle = join(applications, "DontSpeak.app");
    assert.ok((await lstat(join(bundle, "Contents", "Helpers", "dontspeak"))).isFile());
    const leftovers = (await readdir(applications)).filter((entry) =>
      entry.startsWith(".DontSpeak.app.staged."),
    );
    assert.deepEqual(leftovers, []);
    // POSIX release deletes the dir — a mkdir lock left behind can only be retaken by force.
    assert.equal(
      (await readdir(applications)).includes(".DontSpeak.app.ds-install.lock"),
      false,
    );
  },
);

test(
  "installer lock serializes concurrent Linux bundled installs",
  { skip: process.platform === "win32" },
  async (t) => {
    const root = await mkdtemp(join(tmpdir(), "dontspeak-linux-lock-test-"));
    t.after(() => rm(root, { recursive: true, force: true }));

    // apps/linux/package.sh copies tarball-install.sh into the package root as install.sh, and
    // the script resolves its payload from BASH_SOURCE. Running the in-tree file instead would
    // bind HERE to apps/linux, where the payload guard passes on the repo's own uninstall.sh
    // wrapper and the run then dies on the missing bin/ glob — inside the lock, so the test
    // would witness entry and never exit.
    const pkg = join(root, "pkg");
    const fakeBin = join(root, "fakebin");
    const installDir = join(root, "bin");
    const sectionLog = join(root, "sections.log");
    await mkdir(join(pkg, "bin"), { recursive: true });
    await mkdir(join(pkg, "share", "applications"), { recursive: true });
    await mkdir(join(pkg, "share", "icons", "hicolor", "scalable", "apps"), { recursive: true });
    await mkdir(fakeBin);

    await executable(
      join(pkg, "install.sh"),
      await readFile(join(repoRoot, "apps/linux/tarball-install.sh"), "utf8"),
    );
    await executable(join(pkg, "uninstall.sh"), "#!/bin/sh\nexit 0\n");
    // `wire --reconcile` is the last step inside the lock, and it runs the INSTALLED copy.
    await executable(
      join(pkg, "bin", "dontspeak"),
      `#!/bin/sh
[ "\${1:-}" = "wire" ] && printf 'exit\\n' >> "$TEST_SECTION_LOG"
exit 0
`,
    );
    for (const binary of ["ds-gtk", "ds-helper"]) {
      await executable(join(pkg, "bin", binary), "#!/bin/sh\nexit 0\n");
    }
    await writeFile(
      join(pkg, "share", "applications", "dontspeak.desktop"),
      "[Desktop Entry]\nExec=ds-gtk\n",
    );
    await writeFile(
      join(pkg, "share", "icons", "hicolor", "scalable", "apps", "dontspeak.svg"),
      "<svg/>\n",
    );
    // The stop step is the first thing inside the lock. Faking it also keeps the run off the
    // developer's / runner's live ds-gtk and ds-helper.
    await executable(
      join(fakeBin, "pkill"),
      `#!/bin/sh
if [ "\${1:-}" = "-x" ] && [ "\${2:-}" = "ds-gtk" ]; then
  printf 'enter\\n' >> "$TEST_SECTION_LOG"
  sleep 2
fi
exit 0
`,
    );

    const env = {
      ...process.env,
      HOME: root,
      PATH: `${fakeBin}:${process.env.PATH}`,
      XDG_DATA_HOME: join(root, "data"),
      XDG_CONFIG_HOME: join(root, "config"),
      DONTSPEAK_INSTALL_DIR: installDir,
      DONTSPEAK_NO_AUTOSTART: "1",
      DONTSPEAK_INSTALL_LOCK_WAIT: "30",
      TEST_SECTION_LOG: sectionLog,
    };
    const script = join(pkg, "install.sh");
    const runs = await Promise.all([
      runAsync("bash", [script], { cwd: root, env }),
      runAsync("bash", [script], { cwd: root, env }),
    ]);
    for (const run of runs) {
      assert.equal(run.status, 0, `bundled installer failed:\n${run.stdout}\n${run.stderr}`);
    }

    assert.equal(await readFile(sectionLog, "utf8"), "enter\nexit\nenter\nexit\n");
    for (const name of ["dontspeak", "dontspeak-uninstall"]) {
      assert.ok((await lstat(join(installDir, name))).isFile());
    }
    assert.equal((await readdir(installDir)).includes(".dontspeak.ds-install.lock"), false);
  },
);

test(
  "installer lock recovers from abandoned lock state",
  { skip: process.platform === "win32" },
  async (t) => {
    const host = execFileSync("uname", ["-n"], { encoding: "utf8" }).trim();
    const ninetyMinutesAgo = (Date.now() - 90 * 60 * 1000) / 1000;
    const cases = [
      { name: "owner died on this host", owner: `${await reapedPid()} ${host}`, mtime: null },
      { name: "owner file never written", owner: null, mtime: ninetyMinutesAgo },
      { name: "owner from another host", owner: `1 not-this-host`, mtime: ninetyMinutesAgo },
    ];

    for (const seeded of cases) {
      const root = await mkdtemp(join(tmpdir(), "dontspeak-stale-lock-test-"));
      t.after(() => rm(root, { recursive: true, force: true }));
      const driver = await posixLockDriver(root, "driver.sh");
      const destination = join(root, "dest");
      const lock = join(root, ".dest.ds-install.lock");
      await mkdir(lock);
      // Write the owner file BEFORE ageing the directory — creating it after would refresh the
      // directory mtime the age rule reads.
      if (seeded.owner !== null) await writeFile(join(lock, "owner"), `${seeded.owner}\n`);
      if (seeded.mtime !== null) await utimes(lock, seeded.mtime, seeded.mtime);

      const run = await runAsync("sh", [driver, destination], {
        cwd: root,
        env: { ...process.env, DONTSPEAK_INSTALL_LOCK_WAIT: "5" },
      });
      assert.equal(run.status, 0, `${seeded.name}: ${run.stdout}${run.stderr}`);
      assert.match(run.stdout, /entered/, seeded.name);
    }
  },
);

test(
  "installer lock fails closed while a live installer holds it",
  { skip: process.platform === "win32" },
  async (t) => {
    const host = execFileSync("uname", ["-n"], { encoding: "utf8" }).trim();
    const holder = spawn("sh", ["-c", "sleep 30"], { stdio: "ignore" });
    t.after(() => holder.kill("SIGKILL"));

    for (const owner of [`${holder.pid} ${host}`, `${holder.pid} not-this-host`]) {
      const root = await mkdtemp(join(tmpdir(), "dontspeak-held-lock-test-"));
      t.after(() => rm(root, { recursive: true, force: true }));
      const driver = await posixLockDriver(root, "driver.sh");
      const destination = join(root, "dest");
      const lock = join(root, ".dest.ds-install.lock");
      await mkdir(lock);
      await writeFile(join(lock, "owner"), `${owner}\n`);

      const run = await runAsync("sh", [driver, destination], {
        cwd: root,
        env: { ...process.env, DONTSPEAK_INSTALL_LOCK_WAIT: "2" },
      });
      assert.notEqual(run.status, 0, owner);
      assert.match(run.stderr, /still finalizing/, owner);
      assert.doesNotMatch(run.stdout, /entered/, owner);
      assert.ok((await lstat(lock)).isDirectory(), owner);
    }
  },
);

test("Windows installer locks the destination before replacing it", async () => {
  const source = await readFile(join(repoRoot, "scripts/install/web/install.ps1"), "utf8");
  const block = extractLockBlock(
    source,
    "# --- BEGIN destination lock ---",
    "# --- END destination lock ---",
  );
  assert.match(block, /\[System\.IO\.FileShare\]::None/);
  // Pin the FIXED filter, not just the mask: $_.Exception.HResult is the
  // MethodInvocationException wrapper's, which never matches 32/33, so every wait would abort
  // instantly and Windows would hold a lock file while serializing nothing.
  assert.match(block, /GetBaseException\(\)\.HResult -band 0xFFFF/);
  // The call site, not the definition — `Enter-DestinationLock` alone also matches the
  // function above and would still pass with the call deleted.
  const acquire = source.indexOf("Enter-DestinationLock -Destination $dest");
  assert.ok(acquire !== -1);
  assert.ok(acquire < source.indexOf("$installed = @(Get-Process"));
  assert.ok(acquire < source.indexOf("Remove-Item $dest"));
  assert.match(source, /\} finally \{\n  if \(\$lock\) \{ \$lock\.Dispose\(\) \}/);
});
