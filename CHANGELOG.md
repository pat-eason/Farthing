# farthing

## 0.4.0

### Minor Changes

- [#11](https://github.com/pat-eason/Farthing/pull/11) [`8afd004`](https://github.com/pat-eason/Farthing/commit/8afd004cebbdfe93ef897c2503f48693bd3477fd) Thanks [@pat-eason](https://github.com/pat-eason)! - Add conversation transcript viewer. Clicking a request row in Sessions opens a modal showing the full reasoning chain (user prompt → tool calls → assistant answer) with markdown rendering and collapsible thinking blocks. A "View full transcript" button on expanded sessions opens a session-level view with every request as an outlined chunk, each showing its cost.

- [#13](https://github.com/pat-eason/Farthing/pull/13) [`81af89d`](https://github.com/pat-eason/Farthing/commit/81af89dec37ddb799936ecd83892435977613936) Thanks [@pat-eason](https://github.com/pat-eason)! - Add Linux support: Farthing now builds and runs on Ubuntu 22.04+, Debian Bookworm+, Fedora 38+, and Arch/Manjaro. Includes deb, rpm, and AppImage release artifacts. No changes to macOS behavior.

- [#10](https://github.com/pat-eason/Farthing/pull/10) [`a147d38`](https://github.com/pat-eason/Farthing/commit/a147d382d66385041eebf874a21d8bc4e7ad772e) Thanks [@pat-eason](https://github.com/pat-eason)! - Add subscription plan usage (rolling-window limits) and display mode

  Claude Max/Pro subscribers can now opt in to see their 5-hour session and weekly usage windows — as utilization percentages with reset countdowns — instead of (or alongside) API-equivalent cost. A new display mode toggle in Settings and onboarding switches the menu-bar readout between "$1.23" (API Mode) and "5h 4% · $1.23" (Subscription Mode). A near-limit warning (>75%) appears in both modes. New Plan Usage desktop view, compact popover block, and a mode-choice step in onboarding.

### Patch Changes

- [#15](https://github.com/pat-eason/Farthing/pull/15) [`7a8c4f7`](https://github.com/pat-eason/Farthing/commit/7a8c4f7f3636fb022fabb74f934a15ae669dd8f3) Thanks [@pat-eason](https://github.com/pat-eason)! - Notify the farthing-web marketing site on published releases: a new `notify-web.yml` workflow sends a `repository_dispatch` carrying the release tag, so the site's version badge stays in sync. Requires a `FARTHING_WEB_DISPATCH_TOKEN` secret (see docs/release.md).

## 0.3.0

### Minor Changes

- [#9](https://github.com/pat-eason/Farthing/pull/9) [`14814f3`](https://github.com/pat-eason/Farthing/commit/14814f322793d912562f48c41becdaacd5f9c31c) Thanks [@pat-eason](https://github.com/pat-eason)! - Add daily and monthly spend budgets: set them in the new Spend view, see percent-used in the tray title (with ⚠ at amber+) and color-coded progress bars in the popover. Budget breach/approach notifications are deferred to the cost-notifications work.

- [#7](https://github.com/pat-eason/Farthing/pull/7) [`c447bb8`](https://github.com/pat-eason/Farthing/commit/c447bb822fcd77e89d721b71b3ad6c9a66ed9258) Thanks [@pat-eason](https://github.com/pat-eason)! - Add per-view report export: Export action on cost, tokens, sessions, and projects views snapshots current filters and writes a `.zip` bundle (self-contained HTML report, aggregated CSV, raw request-row CSV). Raw rows stream from a dedicated read-only SQLite connection; app-level progress banner, gated desktop notification, and reveal-in-Finder included.

- [#6](https://github.com/pat-eason/Farthing/pull/6) [`1c0d9be`](https://github.com/pat-eason/Farthing/commit/1c0d9be6a95c63cb73b9911bf0dc397b85e3fca2) Thanks [@pat-eason](https://github.com/pat-eason)! - Add desktop spend notifications: recurring delta and rolling-window burst alerts with quiet hours, permission management, and a Spend settings section.

## 0.2.0

### Minor Changes

- [#2](https://github.com/pat-eason/Farthing/pull/2) [`467dbb0`](https://github.com/pat-eason/Farthing/commit/467dbb06a2b38b0fd9be24558455179e75e3b8f1) Thanks [@pat-eason](https://github.com/pat-eason)! - Add Changesets-driven release automation. `pnpm changeset` records changes; merging to `main` opens a "Version Packages" PR that bumps the version across `package.json`, `tauri.conf.json`, `Cargo.toml`, and `Cargo.lock` and updates the changelog. Merging that PR tags `v<version>` and runs the signed/notarized macOS release pipeline automatically.
