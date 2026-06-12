---
"farthing": minor
---

Add Changesets-driven release automation. `pnpm changeset` records changes; merging to `main` opens a "Version Packages" PR that bumps the version across `package.json`, `tauri.conf.json`, `Cargo.toml`, and `Cargo.lock` and updates the changelog. Merging that PR tags `v<version>` and runs the signed/notarized macOS release pipeline automatically.
