# Release signing

The release workflow (`.github/workflows/release.yml`) signs artifacts **only when the
relevant repository secrets are present**. With no secrets it still builds and publishes,
but unsigned/ad-hoc — first launch then hits **SmartScreen** (Windows) / **Gatekeeper**
(macOS). Add the secrets below to turn signing on; no workflow edits needed.

> Reality check (2026): no certificate removes the Windows SmartScreen warning
> *instantly* anymore — Microsoft dropped EV's instant-reputation perk in March 2024.
> Signing replaces "unknown publisher" with the verified name immediately; the
> SmartScreen prompt then fades as download reputation accrues. Only the Microsoft Store
> (Windows) and notarization (macOS) give a clean first launch — and macOS notarization
> *is* wired here.

---

## Windows — unsigned portable zip

Windows ships as a **self-contained portable zip** (`dontspeak-portable-<arch>.zip`), not an
installer, so there is **no Windows code signing** configured. The app runs from an extracted
folder under `%LOCALAPPDATA%\Programs\DontSpeak`; first launch may show a SmartScreen
"unknown publisher" prompt that fades as download reputation accrues. (The previous Inno
installer + SignPath Foundation path was removed when the interactive installer was dropped;
if per-file Authenticode signing is wanted later, sign the binaries inside the zip before
`Compress-Archive` in `apps/windows/installer/build-portable.ps1`.)

---

## macOS — Apple Developer ID + notarization

The `macos` job runs the full distribution path (`apps/macos/dist-apps.sh` with
`DONTSPEAK_DIST=1`): bundles `libonnxruntime.dylib`, signs inside-out with the **hardened
runtime** + entitlements, then **notarizes + staples the `.app`** and zips it
(`DontSpeak-<arch>.app.zip`) — a clean Gatekeeper launch. This activates when an Apple
Developer ID cert is configured.

### Prerequisites
- An **Apple Developer Program** membership ($99/yr).
- A **Developer ID Application** certificate exported as a `.p12` (cert + private key).
- An **app-specific password** for notarization (appleid.apple.com → Sign-In & Security).

### Add these repo secrets
| Secret | Value |
| --- | --- |
| `APPLE_CERT_P12_BASE64` | `base64 -i DeveloperIDApp.p12` (the whole file, base64). |
| `APPLE_CERT_PASSWORD` | The password set when exporting the `.p12`. |
| `APPLE_DEVELOPER_ID` | Identity string, e.g. `Developer ID Application: Your Name (TEAMID)`. Optional — auto-detected from the imported cert if omitted. |
| `APPLE_ID` | Your Apple ID email (for notarytool). |
| `APPLE_TEAM_ID` | Your 10-char Team ID. |
| `APPLE_APP_PASSWORD` | The app-specific password. |

Without `APPLE_CERT_P12_BASE64`, the job builds **ad-hoc** (no notarization) — the app,
helper, Kokoro shim and separator model are still bundled, so the layout is signing-ready.

### Local dev: stable self-signed identity (so TCC grants persist)

Ad-hoc local builds get a fresh cdhash each rebuild, so every **Accessibility / Input
Monitoring** grant breaks and you must re-grant after every `bundle.sh`. Fix: a stable
self-signed code-signing cert whose TCC grants survive rebuilds.

**This is now automatic.** On a clean install, `resolve_sign_identity` (in
`scripts/lib/common.sh`) calls `ensure_local_sign_identity`, which mints + imports a
self-signed `DontSpeak Local Dev` cert **once** when no other identity is present;
`find_codesign_id` then auto-detects it on every later build. No manual step — run
`./apps/macos/bundle.sh`, grant each permission once, and they stick.

Opt out with `DONTSPEAK_NO_AUTOSIGN=1` (build stays ad-hoc); auto-skipped in dist mode and
when `DONTSPEAK_CODESIGN_ID` pins an identity. To create the cert by hand instead:

```sh
openssl req -x509 -newkey rsa:2048 -nodes -keyout k.key -out c.crt -days 3650 \
  -subj "/CN=DontSpeak Local Dev" \
  -addext "extendedKeyUsage=critical,codeSigning" \
  -addext "basicConstraints=critical,CA:false" -addext "keyUsage=critical,digitalSignature"
openssl pkcs12 -export -legacy -inkey k.key -in c.crt -out id.p12 -name "DontSpeak Local Dev" -passout pass:PW
security import id.p12 -k ~/Library/Keychains/login.keychain-db -P PW -T /usr/bin/codesign -A
```

`-legacy` is required (OpenSSL 3's default MAC fails Apple's `security import`). The cert is
untrusted — harmless: `codesign` signs with it and TCC keys on its stable leaf-cert
requirement. After the first build, grant each permission once; they stick thereafter.
To override (e.g. a differently-named cert): `DONTSPEAK_CODESIGN_ID="…" ./apps/macos/bundle.sh`.

---

## Quick reference: what each state produces

| Apple secrets present | Windows | macOS |
| --- | --- | --- |
| no | unsigned `dontspeak-portable-<arch>.zip` | ad-hoc app zips |
| yes | unsigned `dontspeak-portable-<arch>.zip` | signed + notarized app zips |

(Windows is always the unsigned portable zip — there is no Windows signing path.)
