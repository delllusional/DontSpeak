# Release signing

`.github/workflows/release.yml` signs when secrets exist; otherwise publishes
ad-hoc/unsigned. Signing identifies the publisher; macOS notarization is Gatekeeper.
Windows SmartScreen reputation is still per-hash for non-Store downloads.

## Windows — unsigned portable zip

`dontspeak-<version>-windows-<arch>.zip` under `%LOCALAPPDATA%\Programs\DontSpeak`.
Not Authenticode-signed → possible first-launch SmartScreen. If signing is added, do
it before `Compress-Archive` in `apps/windows/installer/build-portable.ps1`.

## macOS — Developer ID + notarization

`apps/macos/dist-apps.sh` with `DONTSPEAK_DIST=1`: bundle ORT dylib, hardened runtime
sign inside-out, notarize + staple, zip `.app`. No cert → same layout, ad-hoc.

### Secrets

| Secret | Value |
| --- | --- |
| `APPLE_CERT_P12_BASE64` | base64 of Developer ID `.p12` |
| `APPLE_CERT_PASSWORD` | export password |
| `APPLE_DEVELOPER_ID` | optional; auto-detected if omitted |
| `APPLE_ID` | Apple ID email |
| `APPLE_TEAM_ID` | 10-char team id |
| `APPLE_APP_PASSWORD` | app-specific password |

Needs Apple Developer Program + Developer ID Application cert + app-specific password.

### Local dev: stable self-signed identity

Ad-hoc rebuilds get a new cdhash and drop TCC grants. `resolve_sign_identity` mints
`DontSpeak Local Dev` once; later builds reuse it. Run `./apps/macos/bundle.sh`, grant
once. Opt out: `DONTSPEAK_NO_AUTOSIGN=1` (also skipped in dist / when
`DONTSPEAK_CODESIGN_ID` is set).

Manual cert:

```sh
openssl req -x509 -newkey rsa:2048 -nodes -keyout k.key -out c.crt -days 3650 \
  -subj "/CN=DontSpeak Local Dev" \
  -addext "extendedKeyUsage=critical,codeSigning" \
  -addext "basicConstraints=critical,CA:false" -addext "keyUsage=critical,digitalSignature"
openssl pkcs12 -export -legacy -inkey k.key -in c.crt -out id.p12 -name "DontSpeak Local Dev" -passout pass:PW
security import id.p12 -k ~/Library/Keychains/login.keychain-db -P PW -T /usr/bin/codesign -A
```

`-legacy` required (OpenSSL 3 MAC). Untrusted is fine — TCC keys on leaf cert, not trust.

## Quick reference

| Apple secrets | Windows | macOS |
| --- | --- | --- |
| no | unsigned zip | ad-hoc app zips |
| yes | unsigned zip | signed + notarized app zips |
