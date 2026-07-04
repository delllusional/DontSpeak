<#
build-common.ps1 — shared prologue for the Windows portable builder (build-portable.ps1).

Dot-source it from a builder:
    . "$PSScriptRoot\build-common.ps1"
then call Initialize-BuildEnv / Resolve-BuildArch / Invoke-CargoRelease. Keeps the
toolchain-PATH setup, the per-arch target derivation, and the engine cargo build in
ONE place (mirrors apps/macos/bundle-lib.sh + scripts/lib/common.sh).
#>

# The single-source workspace version (rust/Cargo.toml [workspace.package]) — the same
# value scripts/version.sh reads; release asset names embed it.
function Get-DsVersion {
    param([Parameter(Mandatory)][string]$Repo)
    $inBlock = $false
    foreach ($line in Get-Content "$Repo\rust\Cargo.toml") {
        if ($line -match '^\s*\[workspace\.package\]') { $inBlock = $true; continue }
        if ($inBlock -and $line -match '^\s*\[') { break }
        if ($inBlock -and $line -match '^\s*version\s*=\s*"([^"]+)"') { return $Matches[1] }
    }
    '0.0.0'
}

# Make the per-user Rust + .NET toolchains visible. PREPEND to the INHERITED PATH (don't
# replace it) so NASM + LLVM added to THIS shell survive — ring's crypto assembles with
# them (CI adds them via GITHUB_PATH; a local build may have them only on the session
# PATH). The inherited PATH already contains the Machine + User values, so nothing else
# needs appending.
function Initialize-BuildEnv {
    $env:Path = "$env:USERPROFILE\.dotnet;$env:USERPROFILE\.cargo\bin;" + $env:Path
    $env:DOTNET_ROOT = "$env:USERPROFILE\.dotnet"
}

# Per-arch build inputs. arm64 CROSS-compiles (--target aarch64-pc-windows-msvc) and stages
# from that target dir; x64 = the host default. Returns the derived paths/args as an object.
function Resolve-BuildArch {
    param(
        [Parameter(Mandatory)][ValidateSet('x64', 'arm64')][string]$Arch,
        [Parameter(Mandatory)][string]$Repo
    )
    $rustTarget = if ($Arch -eq 'arm64') { 'aarch64-pc-windows-msvc' } else { '' }
    [pscustomobject]@{
        RustTarget     = $rustTarget
        Rel            = if ($rustTarget) { "$Repo\rust\target\$rustTarget\release" } else { "$Repo\rust\target\release" }
        CargoTargetArg = if ($rustTarget) { @('--target', $rustTarget) } else { @() }
        DotnetPlatform = if ($Arch -eq 'arm64') { 'ARM64' } else { 'x64' }
    }
}

# Build the three release artifacts the Windows app ships: the in-process engine cdylib,
# the warm-synth helper, and the merged dontspeak bin (MCP server + Claude Code hook executor).
function Invoke-CargoRelease {
    param(
        [Parameter(Mandatory)][string]$Repo,
        [string[]]$CargoTargetArg = @(),
        [string]$RustTarget = ''
    )
    Push-Location "$Repo\rust"
    try {
        if ($RustTarget) { rustup target add $RustTarget; if ($LASTEXITCODE) { throw "rustup target add $RustTarget failed" } }
        # ds-core ships as a cdylib LOADED IN-PROCESS by the WinUI host (P/Invoke), so it must
        # build under the `release-ffi` profile, not plain `release`: `release-ffi` overrides
        # the workspace default `panic = "abort"` back to `"unwind"` so ds-core's extern "C"
        # catch_unwind guard can actually catch a panic and return a safe default instead of
        # aborting this whole host process (mirrors apps/macos/build.sh, which already builds
        # ds-core this way; see rust/Cargo.toml's [profile.release-ffi] and
        # rust/crates/ds-core/src/ffi.rs, which now hard-fails the build under panic="abort").
        # ds-helper/dontspeak are separate short-lived processes, not an in-process FFI
        # library, so they stay on the plain `release` profile.
        cargo build --profile release-ffi --locked @CargoTargetArg -p ds-core; if ($LASTEXITCODE) { throw 'cargo ds-core failed' }
        cargo build --release --locked @CargoTargetArg -p ds-helper; if ($LASTEXITCODE) { throw 'cargo helper failed' }
        cargo build --release --locked @CargoTargetArg -p dontspeak; if ($LASTEXITCODE) { throw 'cargo dontspeak failed' }
    }
    finally { Pop-Location }
}
