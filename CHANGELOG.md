# farthing

## 0.3.0

### Minor Changes

- [#9](https://github.com/pat-eason/Farthing/pull/9) [`14814f3`](https://github.com/pat-eason/Farthing/commit/14814f322793d912562f48c41becdaacd5f9c31c) Thanks [@pat-eason](https://github.com/pat-eason)! - Add daily and monthly spend budgets: set them in the new Spend view, see percent-used in the tray title (with ⚠ at amber+) and color-coded progress bars in the popover. Budget breach/approach notifications are deferred to the cost-notifications work.

- [#7](https://github.com/pat-eason/Farthing/pull/7) [`c447bb8`](https://github.com/pat-eason/Farthing/commit/c447bb822fcd77e89d721b71b3ad6c9a66ed9258) Thanks [@pat-eason](https://github.com/pat-eason)! - Add per-view report export: Export action on cost, tokens, sessions, and projects views snapshots current filters and writes a `.zip` bundle (self-contained HTML report, aggregated CSV, raw request-row CSV). Raw rows stream from a dedicated read-only SQLite connection; app-level progress banner, gated desktop notification, and reveal-in-Finder included.

- [#6](https://github.com/pat-eason/Farthing/pull/6) [`1c0d9be`](https://github.com/pat-eason/Farthing/commit/1c0d9be6a95c63cb73b9911bf0dc397b85e3fca2) Thanks [@pat-eason](https://github.com/pat-eason)! - Add desktop spend notifications: recurring delta and rolling-window burst alerts with quiet hours, permission management, and a Spend settings section.

## 0.2.0

### Minor Changes

- [#2](https://github.com/pat-eason/Farthing/pull/2) [`467dbb0`](https://github.com/pat-eason/Farthing/commit/467dbb06a2b38b0fd9be24558455179e75e3b8f1) Thanks [@pat-eason](https://github.com/pat-eason)! - Add Changesets-driven release automation. `pnpm changeset` records changes; merging to `main` opens a "Version Packages" PR that bumps the version across `package.json`, `tauri.conf.json`, `Cargo.toml`, and `Cargo.lock` and updates the changelog. Merging that PR tags `v<version>` and runs the signed/notarized macOS release pipeline automatically.
