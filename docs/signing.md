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

## Windows — SignPath Foundation (free for open source)

DontSpeak is OSI-licensed (MIT), so it qualifies for **SignPath Foundation**: free managed
code signing, key on SignPath's HSM, cert issued by a trusted CA, verified against this
repo (not a personal identity).

### Apply
1. Go to <https://signpath.org/> → "Apply for the SignPath Foundation" (OSS program).
2. Application details to provide:
   - **Project name:** DontSpeak
   - **Repository:** https://github.com/dontspeak/dontspeak
   - **License:** MIT (OSI-approved) — `LICENSE` in the repo root.
   - **Description:** Local, on-device voice (STT/TTS) for Claude Code — a Rust engine +
     Windows installer (`apps/windows/installer`) and a macOS app.
   - **Artifact to sign:** `ds-setup.exe` (Inno Setup installer) produced by the
     `Release` workflow's `windows` job.
   - **Build system:** GitHub Actions (public), `release.yml`.
   - Eligibility they check: OSI license ✓, actively maintained ✓, released artifact ✓.
3. Once approved, SignPath gives you an **organization id**, a **project slug**, a
   **signing-policy slug** (e.g. `release-signing`), and an **API token**.

### Add these repo secrets
| Secret | Value |
| --- | --- |
| `SIGNPATH_API_TOKEN` | The SignPath CI user API token. |
| `SIGNPATH_ORGANIZATION_ID` | SignPath organization id (GUID). |
| `SIGNPATH_PROJECT_SLUG` | The project slug (e.g. `dontspeak`). |
| `SIGNPATH_POLICY_SLUG` | The signing-policy slug (e.g. `release-signing`). |

The `windows` job uploads the unsigned installer, submits it to SignPath
(`signpath/github-action-submit-signing-request`), and publishes the **signed** result.
Without `SIGNPATH_API_TOKEN`, it publishes the unsigned installer.

---

## macOS — Apple Developer ID + notarization

The `macos` job runs the full distribution path (`apps/macos/dist-dmgs.sh` with
`DONTSPEAK_DIST=1`): bundles `libonnxruntime.dylib`, signs inside-out with the **hardened
runtime** + entitlements, signs the DMG, then **notarizes + staples** it — a clean
Gatekeeper launch. This activates when an Apple Developer ID cert is configured.

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
Monitoring** grant breaks and you must re-grant after every `bundle.sh`. Fix: make a
self-signed code-signing cert **once**; `find_codesign_id` auto-detects it (no env var),
giving a stable signature whose TCC grants survive rebuilds.

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

| Secrets present | Windows | macOS |
| --- | --- | --- |
| none | unsigned `ds-setup.exe` | ad-hoc DMGs |
| Windows only | SignPath-signed installer | ad-hoc DMGs |
| macOS only | unsigned installer | signed + notarized DMGs |
| both | signed installer | signed + notarized DMGs |
