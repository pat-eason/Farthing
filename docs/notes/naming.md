# Naming (task 6.1)

> Checked 2026-06-11. Final name is a human decision; this doc proposes candidates with
> collision evidence and picks a working default. **Working default: BarTab.**

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

## Rename checklist (apply in one commit once the name is confirmed)

The rename deliberately did NOT land with this task: changing `identifier` moves
`app_data_dir` and would orphan the dogfooding DB mid-development. Land it with 6.2/6.3.

1. `src-tauri/tauri.conf.json`: `productName`, `identifier` → `com.peason.bartab`, both
   window `title`s.
2. `package.json` `name`; `src-tauri/Cargo.toml` package `name` + `lib.name`
   (`claude_usage_tracker_lib`, referenced from `main.rs` and every file in
   `src-tauri/examples/`).
3. `src-tauri/src/tray.rs`: "Open Claude Usage Tracker" menu item, tooltip.
4. UI strings: `src/routes/(app)/+layout.svelte` (`app-name`), `src/routes/popover/+page.svelte`
   (footer button).
5. `src-tauri/src/pricing.rs` + `src-tauri/src/autostart.rs`: any identifier/name literals
   (user-agent, LaunchAgent label notes).
6. README/docs headings ("(working name)" markers in README, PRD, development plan).
7. **Data migration:** identifier change moves
   `~/Library/Application Support/com.peason.farthing/` → `.../com.peason.bartab/`.
   Add a one-time first-boot move of `usage.db` (+ `-wal`/`-shm`) from the old dir, or
   accept losing OTel-only history (backfill only regenerates transcript-derived rows).
8. Autostart: LaunchAgent is registered per-identifier; uninstall/re-enable around the
   rename (dev machines only; no public installs exist yet).
9. GitHub repo rename `farthing` → `bartab` (old URL redirects).

## Icon placeholders

Placeholder assets generated by `scripts/generate-icons.py` (PIL; deterministic):

- `art/app-icon.png` — 1024×1024 master (dark rounded square, coral ascending bars);
  `pnpm tauri icon art/app-icon.png` regenerates everything in `src-tauri/icons/`.
- `src-tauri/icons/tray-icon.png` — 32×32 black+alpha **template image** (same bar glyph);
  loaded via `include_bytes!` in `tray.rs` with `icon_as_template(true)` so macOS recolors
  it for dark/light menu bars.

These are explicitly placeholders; real icon design is post-name-confirmation polish.
