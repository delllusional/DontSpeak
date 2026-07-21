import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { resolve, win32 } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

function gitExecPath() {
  const result = spawnSync("git", ["--exec-path"], { encoding: "utf8" });
  if (result.error || result.status !== 0) return undefined;
  return result.stdout.trim() || undefined;
}

export function windowsBashCandidates({ env = process.env, execPath = gitExecPath() } = {}) {
  const candidates = [];
  if (env.DONTSPEAK_GIT_BASH) candidates.push(env.DONTSPEAK_GIT_BASH);
  if (execPath) candidates.push(win32.resolve(execPath, "..", "..", "..", "bin", "bash.exe"));
  if (env.ProgramFiles) candidates.push(win32.join(env.ProgramFiles, "Git", "bin", "bash.exe"));
  if (env["ProgramFiles(x86)"]) {
    candidates.push(win32.join(env["ProgramFiles(x86)"], "Git", "bin", "bash.exe"));
  }
  if (env.LOCALAPPDATA) {
    candidates.push(win32.join(env.LOCALAPPDATA, "Programs", "Git", "bin", "bash.exe"));
  }

  const wslLauncher = env.WINDIR && win32.resolve(env.WINDIR, "System32", "bash.exe").toLowerCase();
  return [...new Set(candidates.map((candidate) => win32.resolve(candidate)))].filter(
    (candidate) => candidate.toLowerCase() !== wslLauncher,
  );
}

export function resolveBash(options = {}) {
  const platform = options.platform ?? process.platform;
  const env = options.env ?? process.env;
  if (platform !== "win32") return env.DONTSPEAK_BASH || "bash";

  const present = options.exists ?? existsSync;
  const candidates = windowsBashCandidates({ env, execPath: options.execPath });
  const bash = candidates.find((candidate) => present(candidate));
  if (!bash) {
    throw new Error(
      "Git Bash was not found; install Git for Windows or set DONTSPEAK_GIT_BASH to bash.exe",
    );
  }
  return bash;
}

export function runBash(args, options = {}) {
  if (!Array.isArray(args) || args.length === 0) throw new Error("a Bash script or option is required");
  const bash = resolveBash(options);
  const result = spawnSync(bash, args, {
    cwd: options.cwd ?? process.cwd(),
    env: options.env ?? process.env,
    stdio: options.stdio ?? "inherit",
    encoding: options.stdio === "pipe" ? "utf8" : undefined,
  });
  if (result.error) throw new Error(`could not start ${bash}: ${result.error.message}`);
  return result;
}

async function main() {
  const result = runBash(process.argv.slice(2));
  if (result.signal) throw new Error(`Git Bash terminated by ${result.signal}`);
  process.exitCode = result.status ?? 1;
}

const invoked = process.argv[1]
  && pathToFileURL(resolve(process.argv[1])).href === pathToFileURL(fileURLToPath(import.meta.url)).href;
if (invoked) {
  main().catch((error) => {
    process.stderr.write(`run-bash: ${error.message}\n`);
    process.exitCode = 1;
  });
}

