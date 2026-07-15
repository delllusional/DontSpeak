# Release signing

The release workflow (`.github/workflows/release.yml`) signs artifacts when the relevant
repository secrets are present, and otherwise still builds and publishes ad-hoc/unsigned.
Signing identifies the publisher; macOS notarization lets Gatekeeper verify Apple's ticket.
For non-Store Windows downloads, SmartScreen reputation is still per file hash even when the
binary is signed; Store distribution avoids that download warning.

## Windows — unsigned portable zip

Windows ships as a self-contained portable zip (`dontspeak-<version>-windows-<arch>.zip`)
that runs from an extracted folder under `%LOCALAPPDATA%\Programs\DontSpeak`. Nothing in it
is code-signed, so first launch may show the SmartScreen "unknown publisher" prompt. If
per-file Authenticode signing is added later, it belongs before `Compress-Archive` in
`apps/windows/installer/build-portable.ps1`.

## macOS — Apple Developer ID + notarization

The `macos` job runs the full distribution path (`apps/macos/dist-apps.sh` with
`DONTSPEAK_DIST=1`): bundles `libonnxruntime.dylib`, signs inside-out with the hardened
runtime + entitlements, then notarizes and staples the `.app` and zips it
(`dontspeak-<version>-macos-<arch>.app.zip`). Without an Apple Developer ID cert configured,
the same job builds ad-hoc instead — same layout, just unsigned.

### Prerequisites
- An **Apple Developer Program** membership.
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

### Local dev: stable self-signed identity (so TCC grants persist)

Ad-hoc local builds get a fresh cdhash each rebuild, which would otherwise break every
Accessibility / Input Monitoring grant on every `bundle.sh`. To keep grants stable across
rebuilds, `resolve_sign_identity` (in `scripts/lib/common.sh`) mints and imports a
self-signed `DontSpeak Local Dev` cert once, the first time no other identity is present;
`find_codesign_id` then auto-detects it on every later build. Just run
`./apps/macos/bundle.sh`, grant each permission once, and it sticks.

Opt out with `DONTSPEAK_NO_AUTOSIGN=1`; this auto-signing is skipped in dist mode and
whenever `DONTSPEAK_CODESIGN_ID` pins an identity. To create the cert by hand instead:

```sh
openssl req -x509 -newkey rsa:2048 -nodes -keyout k.key -out c.crt -days 3650 \
  -subj "/CN=DontSpeak Local Dev" \
  -addext "extendedKeyUsage=critical,codeSigning" \
  -addext "basicConstraints=critical,CA:false" -addext "keyUsage=critical,digitalSignature"
openssl pkcs12 -export -legacy -inkey k.key -in c.crt -out id.p12 -name "DontSpeak Local Dev" -passout pass:PW
security import id.p12 -k ~/Library/Keychains/login.keychain-db -P PW -T /usr/bin/codesign -A
```

`-legacy` is required (OpenSSL 3's default MAC fails Apple's `security import`). The cert
is untrusted, which is fine: `codesign` signs with it and TCC keys on its stable leaf-cert
requirement, not on trust. Override with a differently-named cert via
`DONTSPEAK_CODESIGN_ID="…" ./apps/macos/bundle.sh`.

## Quick reference: what each state produces

| Apple secrets present | Windows | macOS |
| --- | --- | --- |
| no | unsigned windows zip | ad-hoc app zips |
| yes | unsigned windows zip | signed + notarized app zips |
