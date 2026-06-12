# Contributing

The app is a single Tauri v2 project: a Rust backend (`src-tauri/`) and a SvelteKit/TypeScript frontend (`src/`). Read [docs/architecture.md](docs/architecture.md) first for how the pieces fit; [docs/development-plan.md](docs/development-plan.md) records what was built when and why.

## Prerequisites

- macOS (the app is macOS-only for now: menu bar UX, LaunchAgent autostart)
- Rust (stable) — `rustup` recommended; `rustfmt` and `clippy` components
- Node 22+ and [pnpm](https://pnpm.io)
- Xcode Command Line Tools (`xcode-select --install`)

## Dev setup

```bash
git clone <repo-url>
cd farthing
pnpm install

# Run the app in dev mode (opens the tray icon + windows, with frontend HMR)
pnpm tauri dev

# Build a release .app bundle
pnpm tauri build --bundles app
```

`pnpm tauri dev` runs the real backend: the OTLP receiver binds `127.0.0.1:43177`, and the startup backfill pass reads your real transcripts under `~/.claude/projects/` (read-only). Use the env overrides below to keep dev runs away from your real data and settings.

## Dev environment overrides

Both are read at startup; set them on the `pnpm tauri dev` invocation:

| Variable                             | Effect                                                                                                                                                                                                             |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `CLAUDE_USAGE_TRACKER_DATA_DIR`      | Points the whole data dir (usage.db, pricing cache, backups) somewhere else, e.g. a seeded directory. Without it, dev and installed builds share `~/Library/Application Support/com.peason.farthing/`. |
| `CLAUDE_USAGE_TRACKER_SETTINGS_PATH` | Points the onboarding/uninstall flows at a scratch settings.json instead of `~/.claude/settings.json`. **Always set this when exercising onboarding or uninstall in dev.**                                         |

```bash
# Example: fully sandboxed dev run against seeded data
cargo run --manifest-path src-tauri/Cargo.toml --example seed_metrics_db -- /tmp/cut-dev
echo '{}' > /tmp/cut-dev/settings.json
CLAUDE_USAGE_TRACKER_DATA_DIR=/tmp/cut-dev \
CLAUDE_USAGE_TRACKER_SETTINGS_PATH=/tmp/cut-dev/settings.json \
pnpm tauri dev
```

## Checks and tests

CI (`.github/workflows/ci.yml`) runs all of these on every push to `main` and every PR; run them locally before pushing:

```bash
# Frontend (from the repo root)
pnpm format:check   # prettier (pnpm format to fix)
pnpm lint           # eslint
pnpm check          # svelte-check type checking

# Rust (from the repo root, via --manifest-path; or cd src-tauri/)
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```

Notes on the Rust suite:

- Tests are plain `cargo test` unit/module tests; nothing requires a running app. Tauri-command code is tested against `tauri::test::MockRuntime` where a runtime is needed (see `autostart.rs`).
- Tests never touch your real `~/.claude` or Application Support data: everything path-dependent is path-in/path-out and runs against `tempfile` dirs and fixtures.
- Fixtures live under `src-tauri/tests/fixtures/`: real (sanitized) OTLP payloads, transcript JSONL, and real-world settings.json shapes. If you change parsing or the settings merge, extend the fixtures rather than hand-rolling JSON in test bodies; each fixtures dir has a README explaining provenance.

## Dev harness examples

`src-tauri/examples/` contains headless harnesses for working on the backend without the app shell:

| Example                               | Purpose                                                                                                                                 |
| ------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `e2e_receiver`                        | Runs the production Db + receiver stack on `127.0.0.1:43177`, headless. Point a real `claude` run at it to test live ingest end to end. |
| `seed_metrics_db`                     | Generates a large seeded `usage.db` (used for the 1M-row query-performance gate).                                                       |
| `query_bridge`                        | Serves the query commands over plain HTTP so the desktop views can be exercised/verified in a normal browser against a seeded DB.       |
| `backfill_pass` / `parse_transcripts` | Run a backfill pass / transcript parse standalone.                                                                                      |
| `diff_report` / `refresh_pricing`     | Run the OTel-vs-backfill diff report / pricing refresh standalone.                                                                      |

```bash
cargo run --manifest-path src-tauri/Cargo.toml --example e2e_receiver -- /tmp/e2e-data
```

## Conventions

- **Never write to `~/.claude/` from tests or dev tooling.** The settings merge is the highest-blast-radius code in the app; changes to `settings_merge.rs` need fixture coverage for both merge and unmerge, and must keep the abort-on-malformed and atomic-write guarantees.
- **Schema changes are append-only migrations** in `db.rs` (`MIGRATIONS`); never edit a shipped entry. Bump expectations in tests via `meta.schema_version`.
- **Version-tolerant parsing** for anything reading Claude Code's undocumented surfaces (OTel events, transcripts): ignore unknown fields, count-and-surface missing required ones, never panic on shape drift.
- Module-level `//!` doc comments explain each module's role and invariants; keep them current when behavior changes.
- Significant design decisions get a note under `docs/notes/` with the evidence (see the dedup-key and otel-schema notes for the expected shape).

## Pull requests

- Keep PRs focused; make sure all the checks above pass locally.
- If you change a widely-asserted shape (a DTO, a command's return type), run the full Rust suite, not just the tests you edited.
- Releases are tag-driven and maintainer-cut; see [docs/release.md](docs/release.md). Fork builds work without any Apple signing secrets (the release workflow degrades to an unsigned draft).

## License

By contributing, you agree that your contributions are licensed under the [MIT License](LICENSE).
