# farthing

## 0.2.0

### Minor Changes

- [#2](https://github.com/pat-eason/Farthing/pull/2) [`467dbb0`](https://github.com/pat-eason/Farthing/commit/467dbb06a2b38b0fd9be24558455179e75e3b8f1) Thanks [@pat-eason](https://github.com/pat-eason)! - Add Changesets-driven release automation. `pnpm changeset` records changes; merging to `main` opens a "Version Packages" PR that bumps the version across `package.json`, `tauri.conf.json`, `Cargo.toml`, and `Cargo.lock` and updates the changelog. Merging that PR tags `v<version>` and runs the signed/notarized macOS release pipeline automatically.
