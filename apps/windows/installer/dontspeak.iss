; DontSpeak — Windows installer (Inno Setup 6).
;
; Lays down the MINIMAL framework-dependent app, ensures the two runtimes via winget
; (.NET 10 Desktop + Windows App Runtime 2.0), OPTIONALLY pre-downloads the voice models
; and/or the CUDA GPU runtime via `ds-helper --prefetch` (so the download URLs
; stay single-sourced in ds-model — the installer never hardcodes them), wires the
; shortcuts with the brand icon, and best-effort registers the dontspeak MCP.
;
; Build with windows/installer/build.ps1 (which passes /DPayloadDir + /O). Do not run
; ISCC directly without PayloadDir.

#ifndef PayloadDir
  #error Pass /DPayloadDir=<staged payload> (use build.ps1)
#endif
; build.ps1 passes /DAppVersion read from rust/Cargo.toml; fall back if built by hand.
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
; build.ps1 passes /DTargetArch=x64|arm64 (the arch cargo/dotnet built the payload for);
; default x64. Drives ArchitecturesAllowed + the per-arch output filename below.
#ifndef TargetArch
  #define TargetArch "x64"
#endif

[Setup]
AppId={{8F1E5B6A-3C2D-4E7A-9B0F-5A1C2D3E4F60}
AppName=DontSpeak
AppVersion={#AppVersion}
AppPublisher=DontSpeak
; Clean Add/Remove Programs name (the uninstaller's identity). Without this Inno defaults
; the entry to "DontSpeak version 0.2.0"; the version still shows in its own DisplayVersion
; column. The uninstaller executable itself is always unins000.exe — Inno provides no
; directive to rename it — so this DisplayName + the {uninstallexe} "Uninstall DontSpeak"
; Start-menu shortcut are how the uninstaller is named properly.
UninstallDisplayName=DontSpeak
DefaultDirName={autopf}\DontSpeak
DefaultGroupName=DontSpeak
DisableProgramGroupPage=yes
; The Welcome page is HIDDEN by default in Inno 6 — opt in explicitly so our branded
; intro (image + WelcomeLabel1/2) shows on launch.
DisableWelcomePage=no
; No "Ready to Install" page — straight from the choices into installing; the shortcut
; + start-at-login options live on the FINISHED page (postinstall checkboxes below).
DisableReadyPage=yes
UninstallDisplayIcon={app}\ds-winui.exe
OutputBaseFilename=dontspeak-setup-{#TargetArch}
SetupIconFile={#PayloadDir}\AppIcon.ico
; Modern wizard + branded, frameless Welcome page (the intro blurb lives in [Messages]
; WelcomeLabel1/2 below). Two image sizes each so they stay crisp on high-DPI displays.
WizardStyle=modern
WizardImageFile=wizard-large.bmp,wizard-large-2x.bmp
; PNG (Inno 6.3+) so the rounded app tile's transparency blends into the header.
WizardSmallImageFile=wizard-small.png,wizard-small-2x.png
Compression=lzma2
SolidCompression=yes
; Auto-write a setup log to %TEMP%\Setup Log*.txt — records the "prefetch: queued N file(s)"
; line + any helper-probe failure, so a "download page didn't show" can be diagnosed.
SetupLogging=yes
; ARM64-native install for the arm64 build; x64 (incl. arm64 running x64 emulation) otherwise.
#if TargetArch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#else
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#endif
PrivilegesRequired=admin
; The autostart entry is intentionally per-user (HKCU Run) — the SAME value the in-app
; "Start at login" toggle manages — so the two stay in sync. Correct when the user
; elevates their own account (the normal case); silence the admin-mode HKCU advisory.
UsedUserAreasWarning=no

; Frameless Welcome-page text (the branded image sits to the left). Keep it short — the
; Welcome page does not scroll. %n is a line break.
[Messages]
; Wizard window caption — show the version (from {#AppVersion}, stamped by build.ps1 from
; rust/Cargo.toml, the single source of truth). %1 is Inno's AppName ("DontSpeak").
; No "Setup -" prefix — just the branded name + version.
SetupWindowTitle=%1 {#AppVersion}
WelcomeLabel1=DontSpeak
WelcomeLabel2=DontSpeak runs an MCP server that gives an LLM a voice — speaking replies aloud and transcribing dictation for any MCP-capable client.%n%nThe Integrations section wires it into your AI client: Claude Code (MCP + voice hooks, on by default — restart Claude Code after) and Claude Desktop (MCP server, pre-checked when detected). Uncheck what you don't use.%n%nClick Next to choose what to install.

[Types]
Name: "voice";   Description: "Recommended - voice models (runs on CPU)"
Name: "full";    Description: "Full - voice models + NVIDIA GPU acceleration"
Name: "minimal"; Description: "App only - download models later in the app"
Name: "custom";  Description: "Custom"; Flags: iscustom

; .NET + the Windows App Runtime are REQUIRED prerequisites — force-checked + greyed in
; [Code] (CurPageChanged) and winget-installed only if missing. ONNX is the dependency the
; voice models pull in; ONNX + its children are normally selectable (Inno handles the
; parent/child checkbox coupling natively). Sizes are the approximate DOWNLOAD size.
; NOTE: CurPageChanged matches the two required rows by the '.NET' / 'Windows App Runtime'
; substrings in these Descriptions — keep those strings and the matcher in sync.
[Components]
Name: "dotnet"; Description: "Microsoft .NET 10 Desktop Runtime  (required, ~60 MB)"; Types: voice full minimal
Name: "winapp"; Description: "Windows App Runtime 2.0  (required, ~90 MB)"; Types: voice full minimal
Name: "onnx";           Description: "ONNX runtime - needed by the voice models  (~20 MB)"; Types: voice full
Name: "onnx\kokoro";    Description: "Kokoro - text-to-speech voice  (~354 MB)";  Types: voice full
Name: "onnx\parakeet";  Description: "Parakeet - speech-to-text  (~660 MB)";      Types: voice full
Name: "onnx\cuda";      Description: "CUDA GPU acceleration - NVIDIA only  (~1.4 GB)"; Types: full
; ── Integrations (optional) — per-client wiring, grouped under one parent so they read
;    as a set on the component page. The MCP server works with ANY MCP client; these just
;    do the registration / hook wiring for you. The parent is on in every preset (so Claude
;    Code, its checked child, is on by default); the Codex/Desktop children are opt-in. ──
Name: "integrations"; Description: "Integrations"; Types: voice full minimal
; Claude Code: register the MCP with `claude` AND merge the voice hooks (speak replies,
; dictation) into %USERPROFILE%\.claude\settings.json. On by default.
Name: "integrations\claude"; Description: "Claude Code — register the MCP + wire voice hooks"; Types: voice full minimal
; OpenAI Codex: wire the narration hooks (UserPromptSubmit→provide for the spec, Stop→notify
; to speak the reply) into %USERPROFILE%\.codex\config.toml — same binary as Claude Code.
; No Types → OFF by default (opt-in).
Name: "integrations\codex"; Description: "OpenAI Codex — wire the narration hooks"
; Claude Desktop: register the dontspeak stdio MCP server in %APPDATA%\Claude\claude_desktop_config.json
; so Desktop can call speak/listen on demand (no hooks → registration only). No Types → OFF by
; default, but [Code] CurPageChanged PRE-CHECKS it when Claude Desktop is detected; wire claude_desktop
; self-skips if it's absent.
Name: "integrations\claudedesktop"; Description: "Claude Desktop — register the voice MCP server"

; Clear the DontSpeak binaries from {app} BEFORE copying the new payload. [InstallDelete]
; runs at the start of the install step (after PrepareToInstall's taskkill unlocks them,
; before [Files] copies), so a reinstall can't leave an ORPHANED binary behind — both a
; current binary renamed/dropped in a newer version (which `ignoreversion` would never
; remove) AND the LEGACY ds-mcp/-speak/-narrate the single-binary consolidation
; replaced. Either could otherwise keep the Claude Code hooks pointed at an old
; binary. Models live in %LOCALAPPDATA%\DontSpeak\models, not {app}, so this touches no user data.
;
; The CURRENT names mirror ds_config::INSTALLED_BINS (rust/crates/ds-config/src/lib.rs) —
; the one source of truth the cross-platform `wire` prune uses directly; it runs as
; the NON-elevated user and can't clear {app} under Program Files, so this elevated,
; pre-copy delete is duplicated here declaratively. Keep in sync.
[InstallDelete]
; current binaries (mirror INSTALLED_BINS)
Type: files; Name: "{app}\dontspeak.exe"
Type: files; Name: "{app}\ds-helper.exe"
Type: files; Name: "{app}\ds-winui.exe"
; legacy binaries replaced by the single-binary consolidation
Type: files; Name: "{app}\ds-mcp.exe"
Type: files; Name: "{app}\ds-speak.exe"
Type: files; Name: "{app}\ds-narrate.exe"
Type: files; Name: "{app}\ds-tray.exe"

[Files]
Source: "{#PayloadDir}\*"; DestDir: "{app}"; Flags: recursesubdirs ignoreversion
; Also kept as a temp-extractable copy so the download page can ask the helper for
; the manifest (URLs from ds-model) BEFORE the app is installed. Same binary.
Source: "{#PayloadDir}\ds-helper.exe"; Flags: dontcopy

; Start-menu shortcuts are always created; the DESKTOP shortcut + start-at-login are
; offered as checkboxes on the FINISHED page (see the postinstall [Run] entries below).
[Icons]
; AppUserModelID MUST match App.xaml.cs's AppUserModelId ("DontSpeak"): Windows maps the
; running process's AUMID to THIS shortcut and uses the shortcut name ("DontSpeak") as the app's
; display name in the taskbar + Task Manager "Apps" group (else it falls back to "ds-winui").
Name: "{group}\DontSpeak"; Filename: "{app}\ds-winui.exe"; IconFilename: "{app}\AppIcon.ico"; \
  AppUserModelID: "DontSpeak"
Name: "{group}\Uninstall DontSpeak"; Filename: "{uninstallexe}"

[Run]
; --- Prerequisites (install only if missing) ---
; BOTH runtimes are fetched on the IN-WIZARD download page (real progress bar, no console
; window) and installed silently here — no winget (winget isn't on PATH in the elevated
; installer context, so the old `winget install` path silently no-op'd and left a
; non-launching app behind). The DOWNLOAD URLs come from ds-model (urls.rs, the single
; registry) via `ds-helper --print-manifest dotnet|winapp` — the installer hardcodes none;
; the saved file name ({code:...}) is whatever that manifest returns. .NET installs with
; /quiet; the Windows App Runtime redistributable with --quiet. Each Check guards on
; "missing AND the download landed", so a present runtime or a skipped download is a no-op.
Filename: "{tmp}\{code:DotNetExeName}"; Parameters: "/install /quiet /norestart"; \
  StatusMsg: "Installing .NET 10 Desktop Runtime..."; \
  Flags: runhidden waituntilterminated; Check: DotNetExeReady
Filename: "{tmp}\{code:WinAppExeName}"; Parameters: "--quiet"; \
  StatusMsg: "Installing Windows App Runtime..."; \
  Flags: runhidden waituntilterminated; Check: WinAppRtExeReady

; --- Place/extract the components the download page already fetched into {tmp}
;     (verify + extract only, NO network here). If a file wasn't pre-downloaded
;     (e.g. the download page failed/was skipped), --install-prefetched falls back to
;     a normal fetch. Routed through ds-model so URLs/SHAs stay single-sourced. ---
Filename: "{app}\ds-helper.exe"; Parameters: "--install-prefetched ""{tmp}"" onnx"; \
  StatusMsg: "Installing ONNX runtime..."; \
  Flags: runhidden waituntilterminated runasoriginaluser; Components: onnx
Filename: "{app}\ds-helper.exe"; Parameters: "--install-prefetched ""{tmp}"" kokoro_model"; \
  StatusMsg: "Installing Kokoro (text-to-speech)..."; \
  Flags: runhidden waituntilterminated runasoriginaluser; Components: onnx\kokoro
Filename: "{app}\ds-helper.exe"; Parameters: "--install-prefetched ""{tmp}"" parakeet_model"; \
  StatusMsg: "Installing Parakeet (speech-to-text)..."; \
  Flags: runhidden waituntilterminated runasoriginaluser; Components: onnx\parakeet
Filename: "{app}\ds-helper.exe"; Parameters: "--install-prefetched ""{tmp}"" cuda"; \
  StatusMsg: "Installing CUDA GPU runtime..."; \
  Flags: runhidden waituntilterminated runasoriginaluser; Components: onnx\cuda

; --- Claude Code integration (optional component). `wire claude_code` does the WHOLE
;     integration in ONE step — voice hooks into %USERPROFILE%\.claude\settings.json AND the
;     stdio MCP server into %USERPROFILE%\.claude.json (additive, backed-up, re-points on
;     reinstall). No external `claude` CLI needed. AS THE ORIGINAL USER so it touches the user's
;     config, not the elevating admin's. ---
Filename: "{app}\dontspeak.exe"; Parameters: "wire claude_code"; \
  StatusMsg: "Wiring Claude Code (voice hooks + MCP server)..."; \
  Flags: runhidden waituntilterminated runasoriginaluser; Components: integrations\claude

; --- Codex integration (optional component). `wire codex` wires the narration hooks into
;     %USERPROFILE%\.codex\config.toml, as the original user; self-skips if ~/.codex is absent. ---
Filename: "{app}\dontspeak.exe"; Parameters: "wire codex"; \
  StatusMsg: "Wiring Codex narration hooks..."; \
  Flags: runhidden waituntilterminated runasoriginaluser; Components: integrations\codex

; --- Claude Desktop integration (optional component). `wire claude_desktop` registers the
;     dontspeak stdio MCP server in %APPDATA%\Claude\claude_desktop_config.json (additive,
;     backed-up, re-points on reinstall). AS THE ORIGINAL USER so it edits the user's %APPDATA%,
;     not the elevating admin's. Self-skips if Claude Desktop isn't actually present. ---
Filename: "{app}\dontspeak.exe"; Parameters: "wire claude_desktop"; \
  StatusMsg: "Registering the MCP server with Claude Desktop..."; \
  Flags: runhidden waituntilterminated runasoriginaluser; Components: integrations\claudedesktop

; --- FINISHED-page checkboxes (postinstall). Run as the original user so the shortcut
;     lands on the user's desktop and the autostart entry is HKCU. The PowerShell scripts
;     use single-quotes / [char]34 so they carry NO literal double-quote (clean nesting). ---
Filename: "powershell.exe"; \
  Parameters: "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -Command ""$w=New-Object -ComObject WScript.Shell;$s=$w.CreateShortcut([Environment]::GetFolderPath('Desktop')+'\DontSpeak.lnk');$s.TargetPath='{app}\ds-winui.exe';$s.IconLocation='{app}\AppIcon.ico';$s.Save()"""; \
  Description: "Create a desktop shortcut"; \
  Flags: postinstall runhidden runasoriginaluser skipifsilent
Filename: "powershell.exe"; \
  Parameters: "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -Command ""$q=[char]34;Set-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'DontSpeak' -Value ($q+'{app}\ds-winui.exe'+$q+' --hidden')"""; \
  Description: "Start automatically when I sign in"; \
  Flags: postinstall runhidden runasoriginaluser skipifsilent

; --- Offer to launch AS THE ORIGINAL (non-elevated) USER. An elevated WinUI app's tray
;     icon is blocked/hidden by UIPI; running at the user's integrity makes the tray show
;     and its clicks work (the desktop/Start shortcuts already launch non-elevated). ---
Filename: "{app}\ds-winui.exe"; Description: "Launch DontSpeak"; \
  Flags: nowait postinstall skipifsilent runasoriginaluser

; --- Undo the Finished-page artifacts on uninstall. They were created by PowerShell (not
;     [Icons]/[Registry]), so Inno doesn't track them — remove them explicitly, as the
;     original user, so the desktop shortcut and the HKCU autostart don't outlive the app. ---
[UninstallDelete]
Type: files; Name: "{userdesktop}\DontSpeak.lnk"

[UninstallRun]
; These run in the UNINSTALLER's own context. NOTE: runasoriginaluser is NOT a supported flag
; in [UninstallRun], and it isn't needed for the usual same-account elevation — the elevated and
; normal tokens of one user share the same HKCU + %USERPROFILE%, so removing the autostart key /
; voice hooks / MCP registrations here hits the user's own profile. (Known limitation: a
; cross-account elevated uninstall — standard user + a DIFFERENT admin's credentials — would
; clean the admin's profile, not the original user's; there is no [UninstallRun] mechanism to
; redirect that.) These run BEFORE the app files are removed, so {app}\dontspeak.exe is present.
Filename: "powershell.exe"; \
  Parameters: "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -Command ""Remove-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'DontSpeak' -ErrorAction SilentlyContinue"""; \
  Flags: runhidden; RunOnceId: "RemoveAutostart"
; Strip each client's WHOLE DontSpeak integration (only OUR entries; a no-op if absent):
; claude_code = voice hooks (settings.json) + MCP server (~/.claude.json); claude_desktop = MCP;
; codex = narration hooks. No external `claude` CLI needed.
Filename: "{app}\dontspeak.exe"; Parameters: "wire claude_code --remove"; \
  Flags: runhidden; RunOnceId: "UnwireClaudeCode"
Filename: "{app}\dontspeak.exe"; Parameters: "wire claude_desktop --remove"; \
  Flags: runhidden; RunOnceId: "UnwireClaudeDesktop"
Filename: "{app}\dontspeak.exe"; Parameters: "wire codex --remove"; \
  Flags: runhidden; RunOnceId: "UnwireCodex"

[Code]
var
  DownloadPage: TDownloadWizardPage;
  NeedDownload, HelperReady: Boolean;
  QueuedCount: Integer;
  DesktopDefaulted: Boolean;  { so the Claude Desktop auto-pre-check happens once, not on every revisit }
  { Saved-as file names for the two prerequisite runtimes, set when their download is queued
    (from the basename ds-model returns) — the Run-section code-constant filenames and the
    *ExeReady Checks read these so the .iss never hardcodes a download name. }
  DotNetFile, WinAppFile: String;

{ ── Dark title bar ─────────────────────────────────────────────────────────────
  Inno Setup has no real dark-mode theming, and a half-dark wizard (dark body, light
  bottom buttons) looks worse than a clean light one. So we ONLY give the wizard a
  dark TITLE BAR (DWM immersive dark mode) under system dark mode, matching the rest
  of the OS chrome, and leave the body as the standard light wizard. Guarded so an
  older OS just shows the fully light wizard. }
function DwmSetWindowAttribute(Wnd: HWND; Attr: Integer; var Value: Integer; Size: Integer): Integer;
  external 'DwmSetWindowAttribute@dwmapi.dll stdcall';

function SystemUsesDarkMode: Boolean;
var v: Cardinal;
begin
  Result := RegQueryDWordValue(HKEY_CURRENT_USER,
    'Software\Microsoft\Windows\CurrentVersion\Themes\Personalize',
    'AppsUseLightTheme', v) and (v = 0);
end;

procedure EnableDarkTitleBarIfNeeded;
var v: Integer;
begin
  if not SystemUsesDarkMode then exit;
  v := 1;  { DWMWA_USE_IMMERSIVE_DARK_MODE = 20 (Win10 2004+/Win11) }
  try DwmSetWindowAttribute(WizardForm.Handle, 20, v, SizeOf(v)); except end;
end;

{ True when Claude Desktop appears installed: its per-user config dir (%APPDATA%\Claude)
  exists, or its install location under %LOCALAPPDATA% is present. Used to PRE-CHECK the
  optional Claude Desktop component (the registration step itself self-skips if absent). }
function IsClaudeDesktopPresent: Boolean;
begin
  Result := DirExists(ExpandConstant('{userappdata}\Claude')) or
            DirExists(ExpandConstant('{localappdata}\AnthropicClaude')) or
            DirExists(ExpandConstant('{localappdata}\Programs\claude'));
end;

{ True when the Windows App Runtime (major 2 — the WinAppSDK 2.x family the app links) isn't
  present. Detected via Get-AppxPackage (always available — NO winget, which isn't reachable
  from the elevated installer): the framework package family is Microsoft.WindowsAppRuntime.2.
  Within a major version the runtime rolls forward and is back-compatible, so presence of the
  family is the gate (if the major is absent we install the build's matching version). Exit
  0 = present; any other code (incl. a failed Exec) = missing, the conservative default. The
  "2" tracks the WinAppSDK major in ds-model's WINDOWS_APP_RUNTIME_VERSION (urls.rs). }
function WinAppRtMissing: Boolean;
var rc: Integer;
begin
  if Exec('powershell.exe',
       '-NoProfile -ExecutionPolicy Bypass -Command "if (Get-AppxPackage -Name '
       + 'Microsoft.WindowsAppRuntime.2) { exit 0 } else { exit 1 }"',
       '', SW_HIDE, ewWaitUntilTerminated, rc) then
    Result := (rc <> 0)
  else
    Result := True;
end;

{ Run the downloaded Windows App Runtime redistributable ONLY if it's actually present (the
  download page saved it as WinAppFile) AND still needed — mirrors DotNetExeReady. }
function WinAppRtExeReady: Boolean;
begin
  Result := WinAppRtMissing and (WinAppFile <> '') and
            FileExists(ExpandConstant('{tmp}\') + WinAppFile);
end;

{ The code-constant filenames for the two prerequisite installers in the Run section —
  whatever name the download was saved under (from ds-model's manifest). Empty until queued;
  the *ExeReady Checks gate the Run entries, so an empty name is never actually executed. }
function DotNetExeName(Param: String): String;
begin
  Result := DotNetFile;
end;
function WinAppExeName(Param: String): String;
begin
  Result := WinAppFile;
end;

{ True when no .NET 10 Desktop Runtime is present. }
function DotNetMissing: Boolean;
var FR: TFindRec; base: String; found: Boolean;
begin
  base := ExpandConstant('{commonpf}\dotnet\shared\Microsoft.WindowsDesktop.App\');
  found := False;
  if FindFirst(base + '10.*', FR) then
  begin
    try
      repeat
        if (FR.Attributes and FILE_ATTRIBUTE_DIRECTORY) <> 0 then found := True;
      until not FindNext(FR);
    finally
      FindClose(FR);
    end;
  end;
  Result := not found;
end;

{ Run the downloaded .NET installer ONLY if it's actually present (the download page put
  it in the temp dir); guards against a "cannot find the file" error if it was skipped. }
function DotNetExeReady: Boolean;
begin
  Result := DotNetMissing and (DotNetFile <> '') and
            FileExists(ExpandConstant('{tmp}\') + DotNetFile);
end;

{ Extract the helper to the temp dir once (it answers --print-manifest; URLs from ds-model). }
function EnsureHelper: Boolean;
begin
  if not HelperReady then
    try
      ExtractTemporaryFile('ds-helper.exe');
      HelperReady := True;
    except
      Log('prefetch: ExtractTemporaryFile failed: ' + GetExceptionMessage);
    end;
  Result := HelperReady;
end;

{ A component's still-needed files as `url|basename|sha` lines (empty => already present). }
function ComponentManifest(const What: String): TArrayOfString;
var rc: Integer; manifest: String; lines: TArrayOfString;
begin
  SetArrayLength(Result, 0);
  if not EnsureHelper then exit;
  manifest := ExpandConstant('{tmp}\sm-manifest.txt');
  DeleteFile(manifest);
  // Run the probe ELEVATED (plain Exec), NOT as the original user. The helper lives in the
  // elevated installer's {tmp} and must WRITE the manifest there, but a medium-integrity
  // ExecAsOriginalUser process can't write into the high-integrity {tmp} — so the manifest
  // came back EMPTY, the download PAGE was skipped entirely, and the heavy fetch fell through
  // to the [Run] --install-prefetched step, which downloads inline (waituntilterminated) and
  // makes the wizard look frozen with a dead Cancel button. Elevated can read+exec the {tmp}
  // helper and write the manifest. Presence resolves against the elevating user's
  // %LOCALAPPDATA%; for the normal same-account UAC consent that's the SAME profile the
  // install writes to, so it's correct. (Cross-account elevation may over-report "missing" →
  // a harmless re-download; --install-prefetched still installs to the original user.)
  rc := -1;
  if not Exec(ExpandConstant('{tmp}\ds-helper.exe'),
       '--print-manifest ' + What + ' "' + manifest + '"', '', SW_HIDE, ewWaitUntilTerminated, rc) then
  begin
    Log('prefetch[' + What + ']: could not launch helper'); exit;
  end;
  if rc <> 0 then
  begin
    Log('prefetch[' + What + ']: helper exited ' + IntToStr(rc)); exit;
  end;
  if LoadStringsFromFile(manifest, lines) then Result := lines;
end;

{ Queue a selected component's still-needed files on the download page. Best-effort: if
  the page is skipped/fails, the [Run] --install-prefetched step falls back to a fetch. }
procedure QueueComponent(const Comp, What: String);
var i, bar: Integer; url, rest, base, sha: String; lines: TArrayOfString;
begin
  if not WizardIsComponentSelected(Comp) then exit;
  lines := ComponentManifest(What);
  for i := 0 to GetArrayLength(lines) - 1 do
  begin
    rest := lines[i];
    if Trim(rest) = '' then continue;
    bar := Pos('|', rest);  url  := Copy(rest, 1, bar - 1);  rest := Copy(rest, bar + 1, Length(rest));
    bar := Pos('|', rest);  base := Copy(rest, 1, bar - 1);  sha  := Copy(rest, bar + 1, Length(rest));
    if (url = '') or (base = '') then continue;
    DownloadPage.Add(url, base, sha);
    NeedDownload := True;  QueuedCount := QueuedCount + 1;
  end;
end;

{ Queue a prerequisite runtime's installer on the download page and return the saved-as
  basename (for the Run-section code-constant filename). The URL comes from ds-model — `ds-helper
  --print-manifest dotnet|winapp` returns a single `url|basename|` line (no sha; aka.ms
  permalinks aren't sha-pinnable) — so the .iss hardcodes no download URL. Returns '' if the
  manifest is unavailable; the caller's *ExeReady Check then skips the install step. }
function QueuePrereq(const What: String): String;
var bar: Integer; url, rest, base: String; lines: TArrayOfString;
begin
  Result := '';
  lines := ComponentManifest(What);
  if GetArrayLength(lines) = 0 then exit;
  rest := Trim(lines[0]);
  bar := Pos('|', rest);  if bar = 0 then exit;
  url := Copy(rest, 1, bar - 1);  rest := Copy(rest, bar + 1, Length(rest));
  bar := Pos('|', rest);  if bar > 0 then base := Copy(rest, 1, bar - 1) else base := rest;
  if (url = '') or (base = '') then exit;
  DownloadPage.Add(url, base, '');
  NeedDownload := True;  QueuedCount := QueuedCount + 1;
  Result := base;
end;

procedure InitializeWizard;
begin
  { Standard Inno download page (progress bar + the wizard's own Cancel button), so it
    matches every other page. nil progress callback = Inno's built-in per-file display. }
  DownloadPage := CreateDownloadPage('Downloading DontSpeak components',
    'Fetching the selected voice models and runtime. The first install can take a while.', nil);
  EnableDarkTitleBarIfNeeded;
end;

{ The two required runtimes (.NET + Windows App Runtime) must always show
  selected-and-greyed regardless of the chosen setup type: force them checked, then
  disable the row so the user can't uncheck. Matched by caption so row order/count
  changes can't mis-target. (We never rewrite Items[i] — that desyncs the control.) }
procedure CurPageChanged(CurPageID: Integer);
var lst: TNewCheckListBox; i: Integer; cap: String;
begin
  if CurPageID <> wpSelectComponents then exit;
  lst := WizardForm.ComponentsList;
  for i := 0 to lst.Items.Count - 1 do
  begin
    cap := lst.Items[i];
    if (Pos('.NET', cap) > 0) or (Pos('Windows App Runtime', cap) > 0) then
    begin
      lst.Checked[i] := True;
      lst.ItemEnabled[i] := False;
    end;
  end;
  { Default the optional "Claude Desktop integration" row ON when Desktop is detected —
    but ONLY the first time the page is shown, so a user who unchecks it isn't overridden
    when they navigate back. The row stays user-toggleable (never disabled). }
  if not DesktopDefaulted then
  begin
    for i := 0 to lst.Items.Count - 1 do
      if Pos('Claude Desktop', lst.Items[i]) > 0 then
        lst.Checked[i] := IsClaudeDesktopPresent;
    DesktopDefaulted := True;
  end;
end;

{ Run the download right after the LAST choice page (Components) — there is no Ready
  page (DisableReadyPage), so this is the final Next before the files are copied. }
function NextButtonClick(CurPageID: Integer): Boolean;
var DlRetry: Boolean;
begin
  Result := True;
  if CurPageID <> wpSelectComponents then exit;
  DownloadPage.Clear;
  NeedDownload := False;  QueuedCount := 0;
  QueueComponent('onnx', 'onnx');
  QueueComponent('onnx\kokoro', 'kokoro_model');
  QueueComponent('onnx\parakeet', 'parakeet_model');
  QueueComponent('onnx\cuda', 'cuda');
  { Prerequisite runtimes — same in-wizard download UX as the models (silent install runs
    in the Run section), URLs from ds-model (NO hardcoded URL, NO winget). Each is queued only
    when its own presence probe says it's missing; the saved-as name is kept for the install. }
  if DotNetMissing then DotNetFile := QueuePrereq('dotnet');
  if WinAppRtMissing then WinAppFile := QueuePrereq('winapp');
  Log('prefetch: queued ' + IntToStr(QueuedCount) + ' file(s)');
  if not NeedDownload then exit;
  { The standard wizard Cancel stays active during the download; clicking it aborts the
    fetch, then we confirm and either close Setup or resume (nothing is installed yet). }
  DownloadPage.Show;
  try
    repeat
      DlRetry := False;
      try
        DownloadPage.Download;
      except
        if DownloadPage.AbortedByUser then
        begin
          if ExitSetupMsgBox then
          begin
            Result := False;
            WizardForm.Close;   { cancel download + install, close Setup }
          end
          else
            DlRetry := True;    { declined — keep downloading }
        end
        else
        begin
          SuppressibleMsgBox(AddPeriod(GetExceptionMessage), mbCriticalError, MB_OK, IDOK);
          Result := False;
        end;
      end;
    until not DlRetry;
  finally
    DownloadPage.Hide;
  end;
end;

{ If DontSpeak is already running, its binaries (Program Files) and OPEN model files
  (%LOCALAPPDATA% — the engine memory-maps the .onnx / loads the CUDA DLLs) are locked, so
  Windows refuses to overwrite them. Stop it before the install step copies binaries and
  --install-prefetched replaces models; the postinstall "Launch" step starts the new build. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var rc: Integer;
begin
  Exec('taskkill.exe',
    '/F /IM ds-winui.exe /IM ds-helper.exe /IM dontspeak.exe /IM ds-tray.exe',
    '', SW_HIDE, ewWaitUntilTerminated, rc);
  Result := '';
end;

{ Verify the REQUIRED runtime actually landed and warn otherwise — a runtime that's still
  missing here means the in-wizard download was skipped/failed (e.g. no network), and must
  not leave a non-launching app behind a "successful" install. The only fix path is online,
  so point the user at re-running setup with a connection rather than at any manual command. }
procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WinAppRtMissing then
    SuppressibleMsgBox('DontSpeak could not download the Windows App Runtime (no network?).'
      + ' The app may not start until it is installed — re-run this setup with an internet'
      + ' connection to finish installing it.',
      mbError, MB_OK, IDOK);
end;

{ Stop a running DontSpeak before uninstalling — otherwise its Program Files binaries and
  the OPEN model files (the engine memory-maps the .onnx / loads the CUDA DLLs) are locked
  and uninstall fails to remove them, leaving an orphaned tray app behind. The mirror of
  PrepareToInstall on the install side. Runs at the START of usUninstall, BEFORE the
  uninstall-run unwire steps spawn their (fresh, short-lived) dontspeak.exe processes. }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var rc: Integer;
begin
  if CurUninstallStep <> usUninstall then exit;
  Exec('taskkill.exe',
    '/F /IM ds-winui.exe /IM ds-helper.exe /IM dontspeak.exe /IM ds-tray.exe',
    '', SW_HIDE, ewWaitUntilTerminated, rc);
  { Offer to remove the downloaded voice models + cached runtime — the re-downloadable blobs in
    the LOCAL DontSpeak app-data folder (models, the CUDA runtime, logs), potentially several GB.
    SETTINGS in the ROAMING DontSpeak folder are KEPT (Windows etiquette: a reinstall remembers
    prefs + enrolled voices). Default No; skipped in a silent uninstall (data kept). DelTree on
    the local-app-data path targets the uninstalling user's profile (correct for the normal
    same-account uninstall; it can't be redirected to a cross-account original user — same
    limitation as the uninstall-run steps above). }
  if UninstallSilent then exit;
  if SuppressibleMsgBox('Also remove the downloaded voice models and cached runtime?'
       + #13#10#13#10 + 'This frees several GB of disk space. Your settings and enrolled voices are kept.',
       mbConfirmation, MB_YESNO, IDNO) = IDYES then
    DelTree(ExpandConstant('{localappdata}\DontSpeak'), True, True, True);
end;
