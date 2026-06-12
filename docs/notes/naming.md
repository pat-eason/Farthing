# Naming (task 6.1)

> Checked 2026-06-11. Final name is a human decision; this doc proposes candidates with
> collision evidence and picks a working default. **Working default: BarTab.**
>
> **Resolved 2026-06-12: final name is Farthing** (display name "Farthing", slug
> `farthing`, bundle identifier `com.peason.farthing`). BarTab was declined; Farthing
> (the runner-up below) was confirmed. The rename checklist has been applied (status
> per item below); the candidate analysis is kept as-is for the historical record.

## Why the working name has to go

"Claude Usage Tracker" / `farthing` is unusable for release:

- `hamed-elfayome/Claude-Usage-Tracker` (2,690★) is literally a native macOS menu bar
  Swift app with the same name; `masorange/ClaudeUsageTracker` (114★) is another one.
- Anthropic's brand guidance discourages "Claude" in third-party product names; every
  serious tool in this niche avoids it or gets lost in the pile below.
- PRD risk table already flags the ccusage/CCSeva collision class.

## The niche is crowded (what we must clear)

Tools an LLM/user could confuse us with, found via GitHub search + web search:

| Existing tool | What it is |
|---|---|
| `ccusage` (npm CLI) | Transcript-scanning cost CLI; the genre namer |
| CCSeva | Electron menu bar Claude usage app |
| `hamed-elfayome/Claude-Usage-Tracker` (2,690★) | Swift macOS menu bar usage-limit tracker |
| `phuryn/claude-usage` (1,783★) | Local usage dashboard |
| `Blimp-Labs/claude-usage-bar` (451★), `Artzainnn/ClaudeUsageBar` (206★) | macOS menu bar usage apps |
| metermaid.app | Paid macOS menu bar Claude/OpenAI rate-limit monitor |
| `burnbar` (4+ unrelated repos) | Several are macOS menu bar / statusline Claude usage trackers |
| `ccbar`, `claude-code-menu-bar`, etc. | Long tail of cc-prefixed menu bar tools |

Naming rules derived from this: no "Claude", no "cc" prefix, no "usage"+"bar"/"tracker"
construction; pick a real word/brand and let the tagline carry the description.

## Collision-check method

Each candidate was checked against, on 2026-06-11:

- npm registry (`registry.npmjs.org/<name>`), crates.io API, PyPI API
- Homebrew formula + cask APIs (`formulae.brew.sh`)
- Mac App Store (iTunes Search API, `entity=macSoftware`)
- GitHub repo search (`gh search repos`)
- General web search for product/company collisions

## Candidates

| Candidate | npm | crates | brew (formula/cask) | PyPI | Mac App Store | GitHub / web | Verdict |
|---|---|---|---|---|---|---|---|
| **BarTab** | free | free | free | taken | no exact macOS hit | GH: dead/out-of-niche (`philikon/BarTab` Firefox add-on, abandoned ~2011; `BARtab` bioinformatics). Web: bartab.info (iOS bar POS), Table Tap BarTab — hospitality, not dev tools | **Recommended** |
| **Farthing** | free | free | free | taken (dormant 5★) | none | GH: minor unrelated (surname, penny-farthing). Web: clean | Strong runner-up |
| Metermaid | free | free | free | free | none | metermaid.app **is a macOS menu bar Claude usage monitor** | Rejected: direct niche collision |
| Burnbar | free | free | free | free | none | 4+ GH repos, several being macOS menu bar Claude usage trackers | Rejected: crowded in exactly this niche |
| Centime | free | free | free | free | none | Centime Inc (centime.com): funded finance-automation SaaS incl. "spend & expense management" | Rejected: active trademark in spend-tracking software |
| Spenny | free | free | free | free | "spenny" on US App Store | Two fintech products (YC/CRED India; spenny.app Canada) | Rejected |
| Tokenwatch | taken | free | free | taken | "TokenWatch" exists | — | Rejected |
| TokenTally | taken | free | free | taken | none | — | Rejected (hyphenated `token-tally` is free but base name is squatted) |
| Ledgerline | taken | free | free | free | none | Several GH finance/reconciliation trackers | Rejected |

## Working default: BarTab

- **Display name:** BarTab. **Slug:** `bartab`. **Tagline:** "a menu-bar tab for your
  Claude Code spend" (the pun is the brand: it lives in the menu bar and keeps your
  running tab).
- **Bundle identifier:** `com.peason.bartab`. Reservation in the Apple sense happens by
  registering the App ID in the developer portal during 6.2 (signing); nothing else can
  squat a reverse-DNS id under `com.peason.`.
- **Namespace status:** npm, crates.io, Homebrew formula+cask, and Mac App Store
  (macSoftware) are all free; the only registry hit is PyPI (irrelevant: we ship no
  Python). GitHub `bartab` repos are dead or out-of-niche.
- **Trademark note (human judgment needed):** "Bartab: Point of Sale" (iOS) and
  bartab.info operate in hospitality point-of-sale. Different market and channel (we ship
  a free OSS dev tool via GitHub .dmg, not the App Store), so risk reads low — but this
  is exactly the call a human should confirm.

## Rename checklist (applied 2026-06-12, name: Farthing)

The rename deliberately did NOT land with task 6.1: changing `identifier` moves
`app_data_dir` and would orphan the dogfooding DB mid-development. It landed in one
commit ("Rename to Farthing") once the name was confirmed.

1. [x] `src-tauri/tauri.conf.json`: `productName` → `Farthing`, `identifier` →
   `com.peason.farthing`, both window `title`s.
2. [x] `package.json` `name` → `farthing`; `src-tauri/Cargo.toml` package `name` →
   `farthing` + `lib.name` → `farthing_lib` (referenced from `main.rs` and every file in
   `src-tauri/examples/`).
3. [x] `src-tauri/src/tray.rs`: "Open Farthing" menu item, "Farthing" tooltip.
4. [x] UI strings: `src/routes/(app)/+layout.svelte` (`app-name`), `src/routes/popover/+page.svelte`
   (footer button).
5. [x] `src-tauri/src/pricing.rs` + `src-tauri/src/autostart.rs`: identifier/name literals
   (the bundled pricing file's provenance entry is now `_farthing`; LaunchAgent label
   notes reference `target/debug/farthing`). Env overrides renamed for consistency:
   `FARTHING_DATA_DIR` / `FARTHING_SETTINGS_PATH` / `FARTHING_PROJECTS_DIR`.
6. [x] README/docs headings ("(working name)" markers in README, PRD, development plan;
   release.md notes).
7. [x] **Data migration:** identifier change moves
   `~/Library/Application Support/com.peason.farthing/` → `.../com.peason.farthing/`.
   One-time first-boot move of `usage.db` (+ `-wal`/`-shm`) from the old dir implemented
   as `db::migrate_legacy_data_dir` (unit-tested; move-not-copy; no-op when the new dir
   already has a `usage.db`, old dir left untouched).
8. [ ] Autostart: LaunchAgent is registered per-identifier; uninstall/re-enable around the
   rename (dev machines only; no public installs exist yet). Manual step, not part of the
   rename commit.
9. [ ] GitHub repo rename `farthing` → `farthing` (old URL redirects).
   Deferred; the local/remote repo directory is still `farthing`, which is why
   docs that reference the real path (e.g. `docs/notes/otel-schema.md`, CONTRIBUTING's
   `cd farthing`) intentionally keep the old slug.

## Icons

Real art ships now, from two masters with one generator script each (both PIL,
deterministic):

**App icon + sidebar** — master is `art/farthing-icon.png` (full-color coin-with-bird
mark; the three-bar placeholder era is over), derived by `scripts/generate-icons.py`:

- `art/app-icon.png` — 1024×1024 square master (coin centered on the macOS icon grid);
  `pnpm tauri icon art/app-icon.png` regenerates everything in `src-tauri/icons/`.
- `src/lib/assets/farthing-icon.png` — 128px full-color downscale for the desktop
  sidebar wordmark (Vite-imported in `src/routes/(app)/+layout.svelte`).

**Menu bar tray glyph** — master is `art/tray-source-bird.png` (stylized bird + arrow +
dot render), derived by `scripts/generate-tray-candidates.py`:

- `art/tray-candidates/` — the candidate set (template-full, template-bird, color, plus
  a simulated light/dark menu bar `preview.png`), kept for provenance.
- `src-tauri/icons/tray-icon.png` — the chosen candidate (**template-bird**): 82×64
  black+alpha **template image**, a bird-only silhouette (the full mark is too busy at
  menu bar sizes, and the old coin master read as a featureless disc); loaded via
  `include_bytes!` in `tray.rs` with `icon_as_template(true)` so macOS recolors it for
  dark/light menu bars. macOS renders tray images 18pt tall, so 64px height = @2x
  retina. The decode test in `tray.rs` pins the 82×64 size.

After changing either master: run its script (re-tune the extraction constants at the
top if the art changed shape), and for the app icon also re-run
`pnpm tauri icon art/app-icon.png`.
