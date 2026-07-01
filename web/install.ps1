<#
  DontSpeak one-command installer — Windows.

      irm https://dontspeak.org/install.ps1 | iex

  Downloads the prebuilt setup for this arch from the latest GitHub Release, verifies
  its SHA-256, runs the installer silently (which wires the MCP server + voice hooks),
  re-asserts `dontspeak wire --all` for good measure, and launches the app so the voice
  models download themselves on first boot. No compiler required.

  Programmers who want a from-source build should clone the repo and use the
  build-windows path instead (this script never builds).

  Env overrides:
    DONTSPEAK_REPO            owner/repo (default delllusional/DontSpeak)
    DONTSPEAK_DOWNLOAD_BASE   override the asset base URL (e.g. a dontspeak.org mirror)
    DONTSPEAK_DRY_RUN=1       resolve + print the plan, download nothing
#>
$ErrorActionPreference = 'Stop'
$repo = if ($env:DONTSPEAK_REPO) { $env:DONTSPEAK_REPO } else { 'delllusional/DontSpeak' }
$api  = "https://api.github.com/repos/$repo/releases/latest"
$dry  = $env:DONTSPEAK_DRY_RUN -eq '1'

function Say  ($m) { Write-Host "==> $m" }
function Warn ($m) { Write-Warning $m }

# arch: ARM64 → arm64, everything else (AMD64) → x64
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
$setupName = "dontspeak-setup-$arch.exe"

# Resolve an asset URL by literal filename off the latest release (or the override base).
function Resolve-Asset ($name) {
  if ($env:DONTSPEAK_DOWNLOAD_BASE) { return ($env:DONTSPEAK_DOWNLOAD_BASE.TrimEnd('/') + "/$name") }
  $rel = Invoke-RestMethod -Headers @{ 'User-Agent' = 'dontspeak-install' } -Uri $api
  $a = $rel.assets | Where-Object { $_.name -eq $name } | Select-Object -First 1
  if ($a) { return $a.browser_download_url } else { return $null }
}

$setupUrl = Resolve-Asset $setupName
if (-not $setupUrl) { throw "no Windows asset ($setupName) on the latest release of $repo" }
Say "Windows $arch -> $setupUrl"

if ($dry) { Write-Host "(dry run) would silently install $setupName then wire --all"; return }

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("dontspeak-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
  $setup = Join-Path $tmp $setupName
  Say "downloading"
  Invoke-WebRequest -Headers @{ 'User-Agent' = 'dontspeak-install' } -Uri $setupUrl -OutFile $setup

  # SHA-256 verify against checksums.txt (skips cleanly if the release lacks it).
  $sumsUrl = Resolve-Asset 'checksums.txt'
  if ($sumsUrl) {
    try {
      $sums = (Invoke-WebRequest -Headers @{ 'User-Agent' = 'dontspeak-install' } -Uri $sumsUrl).Content
      $want = ($sums -split "`n" | Where-Object { $_ -match ("\*?" + [regex]::Escape($setupName) + '\s*$') } |
               Select-Object -First 1) -replace '\s.*$', ''
      if ($want) {
        $got = (Get-FileHash -Algorithm SHA256 $setup).Hash.ToLower()
        if ($got -ne $want.ToLower()) { throw "checksum mismatch for $setupName (want $want, got $got)" }
        Say "verified $setupName (sha256 ok)"
      } else { Warn "$setupName not listed in checksums.txt — skipping integrity check" }
    } catch { if ($_.Exception.Message -match 'checksum mismatch') { throw } else { Warn "checksum step skipped: $($_.Exception.Message)" } }
  } else { Warn "no checksums.txt on the release — skipping integrity check" }

  Say "installing (silent; elevation prompt is expected)"
  # /VERYSILENT runs the installer's own `wire claude_code` step and installs the app to
  # %ProgramFiles%\DontSpeak. The default component set includes the Claude Code integration.
  $p = Start-Process -FilePath $setup -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART' -Verb RunAs -Wait -PassThru
  if ($p.ExitCode -ne 0) { Warn "installer exited with code $($p.ExitCode)" }

  # Re-assert wiring for every client (idempotent) and launch the app so models download.
  $app = Join-Path $env:ProgramFiles 'DontSpeak'
  $cli = Join-Path $app 'dontspeak.exe'
  if (Test-Path $cli) { Say "wiring clients (MCP + hooks)"; & $cli wire --all 2>$null }
  else { Warn "dontspeak.exe not found under $app — open DontSpeak and use Setup Integration to wire" }

  $ui = Join-Path $app 'ds-winui.exe'
  if (Test-Path $ui) { Say "launching DontSpeak (first boot downloads the voice models)"; Start-Process $ui }

  Write-Host ""
  Write-Host "Done. Start a NEW Claude Code session to load the DontSpeak MCP server."
  Write-Host "Models download automatically in the background; watch progress in the app."
  Write-Host "Undo any time:  & '$cli' wire --all --remove"
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
