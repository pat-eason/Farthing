# Release, signing & notarization

How a tagged release of this app gets built, signed, notarized, and published, and exactly what credentials a maintainer must configure to make that happen.

## Versioning (Changesets)

Versions are not bumped by hand. The flow is [Changesets](https://github.com/changesets/changesets)-driven, so the version stays in sync across all four files that must agree (`package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`) and the changelog writes itself.

1. **Record the change.** With any user-facing PR, run `pnpm changeset`, pick the bump, write a one-line note, and commit the generated `.changeset/*.md` file.
2. **Version PR.** On merge to `main`, `.github/workflows/version.yml` runs the [Changesets action](https://github.com/changesets/action). If changesets are pending, it opens (or updates) a **"Version Packages"** PR that runs `pnpm run version` -- `changeset version` bumps `package.json` and rewrites `CHANGELOG.md`, then `scripts/sync-version.mjs` fans the new version out to `tauri.conf.json`, `Cargo.toml`, and `Cargo.lock`.
3. **Release.** When that PR merges, the bump lands on `main` with no changesets left. `version.yml` detects the version change (and that `v<version>` is not already tagged) and calls the reusable `release.yml` via `workflow_call`, which creates and pushes the `v<version>` tag and runs the pipeline below. No PAT is needed: the tag is created inside the release job rather than relied upon to trigger it.

> **One-time repo setting:** Settings → Actions → General → Workflow permissions → enable **"Allow GitHub Actions to create and approve pull requests"** so the Version PR can be opened. Without it, `version.yml` fails when it tries to open the PR.

To cut a release by hand instead (e.g. a hotfix), push a tag whose version matches `tauri.conf.json`: `git tag v0.2.0 && git push origin v0.2.0`. `release.yml` still gates on the version match.

## Pipeline overview

`.github/workflows/release.yml` runs on a `v*` tag push, on a `workflow_call` from `version.yml` (which passes the tag and creates it), or on manual `workflow_dispatch` (a build-only dry run with no tag and no release):

1. Verifies the tag matches `version` in `src-tauri/tauri.conf.json` (tag `v0.1.0` requires version `0.1.0`).
2. Builds a universal (arm64 + x86_64) macOS app via `pnpm tauri build --target universal-apple-darwin --bundles app,dmg`.
3. **If signing secrets are configured**: Tauri's bundler imports the Developer ID certificate into a temporary keychain and signs the app with the hardened runtime (enabled in `tauri.conf.json` under `bundle.macOS.hardenedRuntime`).
4. **If notarization secrets are configured**: the bundler submits to Apple's notary service, waits, and staples the ticket. CI then verifies with `codesign --verify --deep --strict`, `xcrun stapler validate`, and `spctl --assess --type exec` so a release can never publish un-assessed.
5. Publishes a GitHub Release with the `.dmg` and a SHA-256 checksum file. Releases are **published** only when notarized; signed-but-unnotarized or unsigned builds are created as **drafts** with a warning in the notes, so an unsigned artifact can't silently ship.

The release pipeline does not re-run tests; tags are expected to be cut from a green `main` (the `CI` workflow gates every push).

Forks without any secrets get the same pipeline: it builds an unsigned `.dmg` and creates a draft release (warning annotations explain what's missing).

## Required repo secrets

All six secrets live in GitHub repo settings: Settings → Secrets and variables → Actions → New repository secret. The names are the exact env vars Tauri's bundler reads; no workflow changes are needed once they're added.

### Signing (Developer ID)

| Secret | Value |
|---|---|
| `APPLE_CERTIFICATE` | Base64-encoded `.p12` export of the **Developer ID Application** certificate (cert + private key) |
| `APPLE_CERTIFICATE_PASSWORD` | The password chosen when exporting the `.p12` |
| `APPLE_SIGNING_IDENTITY` | The certificate's full common name, e.g. `Developer ID Application: Pat Eason (TEAMID1234)` |

To produce them (requires a paid Apple Developer account):

1. Create a **Developer ID Application** certificate at <https://developer.apple.com/account/resources/certificates/list> (or via Xcode → Settings → Accounts → Manage Certificates).
2. In Keychain Access, export the certificate **and** its private key as `certificate.p12` with a password.
3. `base64 -i certificate.p12 | pbcopy` → paste as `APPLE_CERTIFICATE`.
4. `security find-identity -v -p codesigning` → copy the `Developer ID Application: ...` string as `APPLE_SIGNING_IDENTITY`.

### Notarization

| Secret | Value |
|---|---|
| `APPLE_ID` | Apple ID email of the developer account |
| `APPLE_PASSWORD` | An **app-specific password** for that Apple ID (generate at <https://account.apple.com> → Sign-In and Security → App-Specific Passwords); never the account password |
| `APPLE_TEAM_ID` | 10-character Team ID from <https://developer.apple.com/account#MembershipDetailsCard> |

Alternative: Tauri also supports App Store Connect API keys (`APPLE_API_ISSUER`, `APPLE_API_KEY`, `APPLE_API_KEY_PATH`) instead of the Apple ID trio. The workflow is wired for the Apple ID trio; switch the env block in `release.yml` if the API-key route is preferred.

## Cutting a release

The normal path is automated (see [Versioning](#versioning-changesets)): land changesets, merge the Version PR, and the release cuts itself. The version bump touches all four files plus `CHANGELOG.md`; no manual edits.

For a manual/hotfix release, bump the version (`pnpm changeset` + merge the Version PR, or edit `package.json` and run `node scripts/sync-version.mjs` so all four files agree), then push the matching tag:

```sh
git tag v0.2.0 && git push origin v0.2.0
```

The Release workflow publishes the notarized `.dmg` (or a draft if credentials are missing).

## Verifying a signed build locally

With the certificate in your login keychain, the same env vars drive a local signed build:

```sh
export APPLE_SIGNING_IDENTITY="Developer ID Application: Pat Eason (TEAMID1234)"
export APPLE_ID="..." APPLE_PASSWORD="..." APPLE_TEAM_ID="..."
pnpm tauri build --bundles app,dmg
```

Then assess:

```sh
APP="src-tauri/target/release/bundle/macos/Farthing.app"
codesign --verify --deep --strict --verbose=2 "$APP"
xcrun stapler validate "$APP"
spctl --assess --type exec --verbose=2 "$APP"   # must report "accepted, source=Notarized Developer ID"
```

`spctl --assess` only accepts notarized Developer ID builds; an ad-hoc or unsigned local build is expected to be rejected.

## Status / blockers

- [ ] **Blocked on human**: no Apple Developer credentials exist yet. Add the six secrets above, push a tag, and confirm the `Verify notarization` step passes and the release publishes non-draft. Until then the automated flow still works end to end: it produces an unsigned `.dmg` and a **draft** release.
- [ ] **One-time repo setting**: enable "Allow GitHub Actions to create and approve pull requests" (Settings → Actions → General) so `version.yml` can open the Version PR.
- The rename to **Farthing** (see `docs/notes/naming.md`) landed 2026-06-12 (`productName: Farthing`, `identifier: com.peason.farthing`); the workflow finds the `.app`/`.dmg` by glob, so no `release.yml` edits were needed.
