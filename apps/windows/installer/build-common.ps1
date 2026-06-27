<#
build-common.ps1 — shared prologue for the Windows builders (build.ps1 + build-portable.ps1).

Dot-source it from a builder:
    . "$PSScriptRoot\build-common.ps1"
then call Initialize-BuildEnv / Resolve-BuildArch / Invoke-CargoRelease. Keeps the
toolchain-PATH setup, the per-arch target derivation, and the engine cargo build in
ONE place (mirrors apps/macos/bundle-lib.sh + scripts/lib/common.sh).
#>

# Make the per-user Rust + .NET toolchains visible. PREPEND to the INHERITED PATH (don't
# replace it) so NASM + LLVM added to THIS shell survive — ring's crypto assembles with
# them (CI adds them via GITHUB_PATH; a local build may have them only on the session PATH).
function Initialize-BuildEnv {
    $env:Path = "$env:USERPROFILE\.dotnet;$env:USERPROFILE\.cargo\bin;" + $env:Path + ';' +
        [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
        [Environment]::GetEnvironmentVariable('Path', 'User')
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
        cargo build --release @CargoTargetArg -p ds-core; if ($LASTEXITCODE) { throw 'cargo ds-core failed' }
        cargo build --release @CargoTargetArg -p ds-tts --bin ds-helper; if ($LASTEXITCODE) { throw 'cargo helper failed' }
        cargo build --release @CargoTargetArg -p dontspeak; if ($LASTEXITCODE) { throw 'cargo dontspeak failed' }
    }
    finally { Pop-Location }
}
