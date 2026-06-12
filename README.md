# Claude Usage Tracker (working name)

A macOS menu bar app that makes Claude Code token/cost usage visible with zero manual configuration. It embeds a localhost OTLP receiver for Claude Code's telemetry events, stores per-request usage in SQLite, and surfaces metrics via a menu bar popover and a full desktop UI.

See [docs/prd.md](docs/prd.md) for product requirements and [docs/development-plan.md](docs/development-plan.md) for the build plan.

## Stack

- **Backend:** Rust (Tauri v2)
- **Frontend:** TypeScript + Svelte (SvelteKit, static adapter)
- **Database:** SQLite

## Development

Prerequisites: Rust (stable), Node 22+, pnpm, Xcode Command Line Tools.

```bash
pnpm install

# Run the app in dev mode (opens a window with HMR)
pnpm tauri dev

# Build the release .app bundle
pnpm tauri build

# Frontend checks
pnpm check          # svelte-check (type checking)
pnpm lint           # eslint
pnpm format:check   # prettier
pnpm format         # prettier --write

# Rust checks (from src-tauri/, or use --manifest-path src-tauri/Cargo.toml)
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

CI runs format/lint/check on the frontend and fmt/clippy/build on the Rust side for every push to `main` and every PR (see `.github/workflows/ci.yml`).

## Releases

Pushing a `v*` tag builds a universal macOS `.dmg`, signs and notarizes it (when Apple credentials are configured as repo secrets), and publishes a GitHub Release (see `.github/workflows/release.yml`). Signing/notarization is optional and secret-gated, so forks without credentials still get an unsigned draft release. [docs/release.md](docs/release.md) documents the pipeline, the exact secrets to configure, and how to cut a release.

## Recommended IDE Setup

[VS Code](https://code.visualstudio.com/) + [Svelte](https://marketplace.visualstudio.com/items?itemName=svelte.svelte-vscode) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer).
