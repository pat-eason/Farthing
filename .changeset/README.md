# Changesets

This folder is managed by [Changesets](https://github.com/changesets/changesets). It drives versioning and release notes for Farthing.

## Adding a changeset

When you make a user-facing change, record it:

```sh
pnpm changeset
```

Pick the bump (`patch` / `minor` / `major`) and write a short, user-facing note. This creates a markdown file here; commit it with your PR.

## What happens next

On merge to `main`, the **Version** workflow opens (or updates) a "Version Packages" PR that consumes the pending changesets, bumps the version across `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, and `Cargo.lock`, and updates `CHANGELOG.md`.

Merging that PR cuts the `v<version>` tag and runs the **Release** pipeline. See [`docs/release.md`](../docs/release.md) for the full flow.
