<script lang="ts">
  // Settings view: start-at-login toggle (task 2.3) and the uninstall flow
  // (task 2.4). Autostart state is always re-read from the plugin after a
  // toggle, so the UI shows what is actually registered (including changes
  // made outside the app). Dev builds refuse to enable; the toggle shows
  // why instead of pretending it worked. Uninstall never touches anything
  // until the confirmation dialog (which states exactly what will and won't
  // be removed) is confirmed.
  import { resolve } from "$app/paths";
  import { getAutostartStatus, setAutostart, type AutostartStatus } from "$lib/autostart";
  import {
    applyUninstall,
    getUninstallStatus,
    type UninstallOutcome,
    type UninstallStatus,
  } from "$lib/uninstall";
  import { getBudgetConfig, setBudgetConfig, type BudgetConfig } from "$lib/budgets";
  import {
    getUsageLimitsConfig,
    setUsageLimitsConfig,
    setDisplayMode,
    type UsageLimitsConfig,
    type DisplayMode,
  } from "$lib/usage";

  // --- Budgets (configured here in Settings) ---
  // The config also carries notify/approach_pct for the deferred
  // cost-notifications work; we never surface controls for them but preserve
  // their defaults on every save.
  const NOTIFY_DEFAULT = true;
  const APPROACH_PCT_DEFAULT = 76;
  const MIN_AMOUNT = 1;
  const SAVE_DEBOUNCE_MS = 400;
  type Period = "daily" | "monthly";

  let status: AutostartStatus | undefined = $state();
  let busy = $state(false);
  let errorMessage = $state("");

  type UninstallScreen = "idle" | "loading" | "confirm" | "working" | "done";
  let uninstallScreen: UninstallScreen = $state("idle");
  let uninstallStatus: UninstallStatus | undefined = $state();
  let uninstallOutcome: UninstallOutcome | undefined = $state();
  let uninstallError = $state("");
  let deleteDatabase = $state(false);

  let usageConfig: UsageLimitsConfig | undefined = $state();
  let usageError = $state("");
  let usageBusy = $state(false);

  // `budgetConfig` is the working/optimistic copy bound to the inputs;
  // `budgetConfirmed` is the last value the backend accepted (revert target).
  let budgetConfig: BudgetConfig | undefined = $state();
  let budgetConfirmed: BudgetConfig | undefined = $state();
  let budgetError = $state("");
  let fieldError: Record<Period, string> = $state({ daily: "", monthly: "" });
  let saveTimer: ReturnType<typeof setTimeout> | undefined;

  async function refresh() {
    errorMessage = "";
    try {
      status = await getAutostartStatus();
    } catch (err) {
      errorMessage = String(err);
    }
  }

  async function toggle() {
    if (!status || busy) return;
    busy = true;
    errorMessage = "";
    try {
      status = await setAutostart(!status.enabled);
    } catch (err) {
      errorMessage = String(err);
      // The toggle may have been refused (dev build) or failed; re-read so
      // the UI shows the real state rather than the attempted one.
      try {
        status = await getAutostartStatus();
      } catch {
        // keep the original error
      }
    } finally {
      busy = false;
    }
  }

  async function startUninstall() {
    uninstallScreen = "loading";
    uninstallError = "";
    deleteDatabase = false;
    try {
      uninstallStatus = await getUninstallStatus();
      uninstallScreen = "confirm";
    } catch (err) {
      uninstallError = String(err);
      uninstallScreen = "idle";
    }
  }

  async function confirmUninstall() {
    uninstallScreen = "working";
    uninstallError = "";
    try {
      uninstallOutcome = await applyUninstall(deleteDatabase);
      uninstallScreen = "done";
      // The LaunchAgent state changed; reflect it in the toggle above.
      void refresh();
    } catch (err) {
      uninstallError = String(err);
      uninstallScreen = "confirm";
    }
  }

  function formatBytes(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }

  function cloneBudget(c: BudgetConfig): BudgetConfig {
    return {
      daily: { ...c.daily },
      monthly: { ...c.monthly },
      show_in_tray: c.show_in_tray,
      approach_pct: c.approach_pct,
    };
  }

  async function loadUsageConfig() {
    usageError = "";
    try {
      usageConfig = await getUsageLimitsConfig();
    } catch (err) {
      usageError = String(err);
    }
  }

  async function toggleUsagePoller() {
    if (!usageConfig || usageBusy) return;
    usageBusy = true;
    try {
      const next = { ...usageConfig, enabled: !usageConfig.enabled };
      // When disabling, backend resets mode to 'api' — reflect that locally
      if (!next.enabled) next.display_mode = "api";
      await setUsageLimitsConfig(next);
      usageConfig = await getUsageLimitsConfig();
    } catch (err) {
      usageError = String(err);
    } finally {
      usageBusy = false;
    }
  }

  async function toggleDisplayMode() {
    if (!usageConfig || usageBusy) return;
    usageBusy = true;
    try {
      const next: DisplayMode = usageConfig.display_mode === "api" ? "subscription" : "api";
      await setDisplayMode(next);
      usageConfig = await getUsageLimitsConfig();
    } catch (err) {
      usageError = String(err);
    } finally {
      usageBusy = false;
    }
  }

  async function loadBudgets() {
    budgetError = "";
    try {
      const loaded = await getBudgetConfig();
      budgetConfirmed = loaded;
      budgetConfig = cloneBudget(loaded);
    } catch (err) {
      budgetError = String(err);
    }
  }

  // Enabled budgets need a finite amount >= $1; disabled budgets never block.
  function amountValid(period: Period): boolean {
    if (!budgetConfig) return false;
    const budget = budgetConfig[period];
    if (!budget.enabled) return true;
    return Number.isFinite(budget.amount_usd) && budget.amount_usd >= MIN_AMOUNT;
  }

  function validate(period: Period): boolean {
    const ok = amountValid(period);
    fieldError[period] = ok ? "" : `Budget must be at least $${MIN_AMOUNT}`;
    return ok;
  }

  function revertBudget() {
    if (budgetConfirmed) budgetConfig = cloneBudget(budgetConfirmed);
    fieldError = { daily: "", monthly: "" };
  }

  async function saveBudget() {
    if (saveTimer) {
      clearTimeout(saveTimer);
      saveTimer = undefined;
    }
    if (!budgetConfig) return;
    // Skip the save if either enabled budget has an invalid amount; the
    // field-level error is already shown and the field is left as typed.
    if (!validate("daily") || !validate("monthly")) return;

    budgetError = "";
    // Persist the full config, preserving the deferred cost-notifications
    // fields (notify/approach_pct) at their defaults.
    const payload: BudgetConfig = {
      daily: { ...budgetConfig.daily, notify: NOTIFY_DEFAULT },
      monthly: { ...budgetConfig.monthly, notify: NOTIFY_DEFAULT },
      show_in_tray: budgetConfig.show_in_tray,
      approach_pct: APPROACH_PCT_DEFAULT,
    };
    try {
      const result = await setBudgetConfig(payload);
      budgetConfirmed = result;
      // Adopt the (possibly clamped) server result.
      budgetConfig = cloneBudget(result);
    } catch (err) {
      budgetError = String(err);
      revertBudget();
    }
  }

  function scheduleSave() {
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      saveTimer = undefined;
      void saveBudget();
    }, SAVE_DEBOUNCE_MS);
  }

  // Debounced path: typing into the amount input.
  function onAmountInput(period: Period) {
    if (validate(period)) scheduleSave();
  }

  // Immediate path: leaving the amount field commits right away.
  function onAmountBlur() {
    void saveBudget();
  }

  function toggleBudgetEnabled(period: Period) {
    if (!budgetConfig) return;
    budgetConfig[period].enabled = !budgetConfig[period].enabled;
    validate(period);
    void saveBudget();
  }

  function toggleTray() {
    if (!budgetConfig) return;
    budgetConfig.show_in_tray = !budgetConfig.show_in_tray;
    void saveBudget();
  }

  $effect(() => {
    void refresh();
    void loadBudgets();
    void loadUsageConfig();
  });
</script>

<main class="container">
  <h1>Settings</h1>

  {#if budgetError}
    <p class="error-box">{budgetError}</p>
  {/if}
  {#if budgetConfig}
    {#each [{ period: "daily" as const, title: "Daily budget", help: "Tracks today's spend against this amount." }, { period: "monthly" as const, title: "Monthly budget", help: "Tracks spend across the current calendar month." }] as card (card.period)}
      <section class="setting">
        <div class="setting-text">
          <h2>{card.title}</h2>
          <p class="muted">{card.help}</p>
          <label class="amount-label">
            <span>Amount (USD)</span>
            <input
              type="number"
              min={MIN_AMOUNT}
              step="0.01"
              inputmode="decimal"
              disabled={!budgetConfig[card.period].enabled}
              bind:value={budgetConfig[card.period].amount_usd}
              oninput={() => onAmountInput(card.period)}
              onblur={onAmountBlur}
              aria-invalid={fieldError[card.period] ? "true" : undefined}
            />
          </label>
          {#if fieldError[card.period]}
            <p class="error-box">{fieldError[card.period]}</p>
          {/if}
        </div>
        <div class="setting-control">
          <button
            class:primary={!budgetConfig[card.period].enabled}
            onclick={() => toggleBudgetEnabled(card.period)}
            aria-pressed={budgetConfig[card.period].enabled}
          >
            {budgetConfig[card.period].enabled ? "Turn off" : "Turn on"}
          </button>
          <span class="state {budgetConfig[card.period].enabled ? 'good' : 'muted'}">
            {budgetConfig[card.period].enabled ? "On" : "Off"}
          </span>
        </div>
      </section>
    {/each}

    <section class="setting">
      <div class="setting-text">
        <h2>Show budgets in tray</h2>
        <p class="muted">
          Adds a budget band indicator next to today's cost in the menu-bar readout.
        </p>
      </div>
      <div class="setting-control">
        <button
          class:primary={!budgetConfig.show_in_tray}
          onclick={toggleTray}
          aria-pressed={budgetConfig.show_in_tray}
        >
          {budgetConfig.show_in_tray ? "Turn off" : "Turn on"}
        </button>
        <span class="state {budgetConfig.show_in_tray ? 'good' : 'muted'}">
          {budgetConfig.show_in_tray ? "On" : "Off"}
        </span>
      </div>
    </section>
  {/if}

  {#if usageError}
    <p class="error-box">{usageError}</p>
  {/if}
  {#if usageConfig}
    <section class="setting">
      <div class="setting-text">
        <h2>Plan Usage polling</h2>
        <p class="muted">
          Reads your Claude Code login token from the macOS keychain and queries Anthropic for your
          plan-usage percentages (5h session and weekly limits) every 5 minutes. Subscription plan
          only.
        </p>
        {#if usageConfig.enabled}
          <p class="muted" style="margin-top: 0.4rem; font-size: 0.85em;">
            ✓ Enabled — check the <a href={resolve("/(app)/usage")}>Plan Usage</a> view for current windows.
          </p>
        {/if}
      </div>
      <div class="setting-control">
        <button
          class:primary={!usageConfig.enabled}
          disabled={usageBusy}
          onclick={() => void toggleUsagePoller()}
          aria-pressed={usageConfig.enabled}
        >
          {usageBusy ? "Working…" : usageConfig.enabled ? "Turn off" : "Turn on"}
        </button>
        <span class="state {usageConfig.enabled ? 'good' : 'muted'}">
          {usageConfig.enabled ? "On" : "Off"}
        </span>
      </div>
    </section>

    {#if usageConfig.enabled}
      <section class="setting">
        <div class="setting-text">
          <h2>Display mode</h2>
          <p class="muted">
            Decides what the menu-bar readout shows as primary.<br />
            <strong>API Mode:</strong> today's cost (e.g. $1.23).<br />
            <strong>Subscription Mode:</strong> window utilization (e.g. 5h 4%) with cost secondary.
          </p>
        </div>
        <div class="setting-control">
          <button
            class:primary={usageConfig.display_mode === "api"}
            disabled={usageBusy}
            onclick={() => void toggleDisplayMode()}
          >
            {usageConfig.display_mode === "api" ? "Switch to Subscription" : "Switch to API"}
          </button>
          <span class="state good">
            {usageConfig.display_mode === "api" ? "API Mode" : "Subscription Mode"}
          </span>
        </div>
      </section>
    {/if}
  {/if}

  <section class="setting">
    <div class="setting-text">
      <h2>Start at login</h2>
      <p class="muted">
        Keeps the receiver running so usage events are never missed. Registered as a macOS
        LaunchAgent; turning this off removes it.
      </p>
      {#if status?.dev_build}
        <p class="warn-box">
          Dev build: enabling is blocked because the LaunchAgent would point at the dev binary. The
          toggle works in the installed app.
        </p>
      {/if}
      {#if errorMessage}
        <p class="error-box">{errorMessage}</p>
      {/if}
    </div>
    <div class="setting-control">
      {#if !status}
        <span class="muted">Loading…</span>
      {:else}
        <button
          class:primary={!status.enabled}
          disabled={busy}
          onclick={() => void toggle()}
          aria-pressed={status.enabled}
        >
          {busy ? "Working…" : status.enabled ? "Turn off" : "Turn on"}
        </button>
        <span class="state {status.enabled ? 'good' : 'muted'}">
          {status.enabled ? "On: app starts at login" : "Off"}
        </span>
      {/if}
    </div>
  </section>

  <section class="setting uninstall">
    <div class="setting-text full">
      <h2>Uninstall</h2>
      {#if uninstallScreen === "idle"}
        <p class="muted">
          Removes everything this app set up: the settings.json entries, the start-at-login
          LaunchAgent, and (only if you choose) the usage database. You'll see exactly what will be
          removed before anything happens.
        </p>
        {#if uninstallError}
          <p class="error-box">{uninstallError}</p>
        {/if}
        <button class="danger" onclick={() => void startUninstall()}>Uninstall…</button>
      {:else if uninstallScreen === "loading"}
        <p class="muted">Checking what an uninstall would remove…</p>
      {:else if uninstallScreen === "confirm" && uninstallStatus}
        <h3>This will be removed:</h3>
        <ul>
          <li>
            {#if uninstallStatus.settings_changed}
              The telemetry settings and session hook this app added to
              <code>{uninstallStatus.settings_path}</code>. A timestamped backup is saved first.
              <details>
                <summary>Show the exact change</summary>
                <pre class="diff">{#each uninstallStatus.diff as line, i (i)}<span
                      class="diff-{line.kind}"
                      >{line.kind === "add" ? "+" : line.kind === "remove" ? "-" : " "} {line.text}
</span>{/each}</pre>
              </details>
            {:else}
              Nothing from <code>{uninstallStatus.settings_path}</code>: no app-added entries were
              found there.
            {/if}
          </li>
          <li>
            {#if uninstallStatus.autostart_enabled}
              The start-at-login LaunchAgent.
            {:else}
              No LaunchAgent: start at login is not currently registered.
            {/if}
          </li>
          <li>
            {#if uninstallStatus.database_exists}
              <label>
                <input type="checkbox" bind:checked={deleteDatabase} />
                Also delete the usage database ({formatBytes(uninstallStatus.database_size_bytes)} at
                <code>{uninstallStatus.database_path}</code>). Unchecked, your usage history is
                kept.
              </label>
            {:else}
              No usage database found on disk.
            {/if}
          </li>
        </ul>
        <h3>This will <em>not</em> be removed:</h3>
        <ul>
          <li>
            Everything else in <code>{uninstallStatus.settings_path}</code>: only the entries this
            app added are touched, and only if you haven't edited them.
          </li>
          <li>
            Your settings.json backups in <code>{uninstallStatus.backups_dir}</code>, kept so you
            can restore any earlier state.
          </li>
          {#if !deleteDatabase && uninstallStatus.database_exists}
            <li>The usage database (leave the box above unchecked to keep it).</li>
          {/if}
          <li>The app itself: quit it and drag it to the Trash afterwards.</li>
        </ul>
        {#if uninstallError}
          <p class="error-box">{uninstallError}</p>
        {/if}
        <div class="row">
          <button class="danger" onclick={() => void confirmUninstall()}>
            {deleteDatabase ? "Uninstall and delete database" : "Uninstall"}
          </button>
          <button onclick={() => (uninstallScreen = "idle")}>Cancel</button>
        </div>
      {:else if uninstallScreen === "working"}
        <p class="muted">Uninstalling…</p>
      {:else if uninstallScreen === "done" && uninstallOutcome}
        <h3>Uninstalled</h3>
        <ul>
          <li>
            {#if uninstallOutcome.settings_changed}
              settings.json entries removed.
              {#if uninstallOutcome.backup_path}
                Backup saved to <code>{uninstallOutcome.backup_path}</code>.
              {/if}
            {:else}
              settings.json was already clean; nothing to remove.
            {/if}
          </li>
          <li>
            {#if uninstallOutcome.autostart_note}
              LaunchAgent: {uninstallOutcome.autostart_note}
            {:else if uninstallOutcome.autostart_enabled}
              LaunchAgent is still registered.
            {:else}
              LaunchAgent removed (or was never registered).
            {/if}
          </li>
          <li>
            {#if uninstallOutcome.database_deleted}
              Usage database deleted.
            {:else if uninstallOutcome.database_note}
              Usage database: {uninstallOutcome.database_note}
            {:else}
              Usage database kept.
            {/if}
          </li>
        </ul>
        <p>
          Claude Code sessions started from now on won't export anything to this app and run no app
          hook. Already-running sessions stop exporting when you restart them. To finish, quit this
          app and move it to the Trash.
        </p>
      {/if}
    </div>
  </section>

  <div class="row">
    <button onclick={() => void refresh()}>Refresh</button>
    <a class="button-link" href={resolve("/(app)/health")}>Health</a>
    <a class="button-link" href={resolve("/")}>Back to setup</a>
  </div>
</main>

<style>
  .container {
    max-width: 720px;
    margin: 0 auto;
    padding: 2rem 1.5rem;
    text-align: left;
  }

  h1 {
    font-size: 1.4rem;
  }

  h2 {
    font-size: 1.05rem;
    margin: 0 0 0.25rem;
  }

  h3 {
    font-size: 0.95rem;
    margin: 1rem 0 0.25rem;
  }

  .muted {
    color: #6b6b6b;
  }

  .good {
    color: #1a7f37;
  }

  .setting {
    display: flex;
    gap: 1.5rem;
    align-items: flex-start;
    justify-content: space-between;
    padding: 1rem 0;
    border-bottom: 1px solid rgba(0, 0, 0, 0.12);
  }

  .setting-text {
    max-width: 65%;
  }

  .setting-text.full {
    max-width: 100%;
  }

  .setting-control {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    white-space: nowrap;
  }

  .state {
    font-size: 0.9em;
  }

  .amount-label {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    margin-top: 0.6rem;
    font-size: 0.9em;
    max-width: 12rem;
  }

  .amount-label input {
    border-radius: 8px;
    border: 1px solid rgba(0, 0, 0, 0.15);
    padding: 0.45em 0.6em;
    font-size: 0.95em;
    font-family: inherit;
    color: #0f0f0f;
    background-color: #ffffff;
  }

  .amount-label input:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .amount-label input[aria-invalid="true"] {
    border-color: #b42318;
  }

  ul {
    margin: 0.25rem 0 0.5rem;
    padding-left: 1.25rem;
  }

  li {
    margin: 0.35rem 0;
  }

  .diff {
    background: rgba(0, 0, 0, 0.05);
    border-radius: 8px;
    padding: 0.75rem 1rem;
    overflow-x: auto;
    font-size: 0.82em;
    line-height: 1.45;
    margin: 0.5rem 0 0;
  }

  .diff span {
    display: block;
    white-space: pre;
  }

  .diff-add {
    color: #1a7f37;
  }

  .diff-remove {
    color: #b42318;
  }

  .error-box,
  .warn-box {
    border-radius: 8px;
    padding: 0.75rem 1rem;
  }

  .error-box {
    background: #fdecea;
    color: #8a1f11;
  }

  .warn-box {
    background: #fff4e0;
    color: #6b4a00;
  }

  .row {
    display: flex;
    gap: 0.75rem;
    margin-top: 1rem;
    align-items: center;
  }

  button,
  .button-link {
    border-radius: 8px;
    border: 1px solid rgba(0, 0, 0, 0.15);
    padding: 0.55em 1.1em;
    font-size: 0.95em;
    font-weight: 500;
    font-family: inherit;
    color: #0f0f0f;
    background-color: #ffffff;
    cursor: pointer;
    text-decoration: none;
    display: inline-block;
  }

  button.primary {
    background-color: #396cd8;
    border-color: #396cd8;
    color: #ffffff;
  }

  button.danger {
    background-color: #b42318;
    border-color: #b42318;
    color: #ffffff;
  }

  button:disabled {
    opacity: 0.6;
    cursor: default;
  }

  button:hover:not(:disabled),
  .button-link:hover {
    border-color: #396cd8;
  }

  button.danger:hover:not(:disabled) {
    border-color: #8a1f11;
  }

  @media (prefers-color-scheme: dark) {
    .muted {
      color: #a8a8a8;
    }

    .good {
      color: #7ee787;
    }

    .setting {
      border-bottom-color: rgba(255, 255, 255, 0.18);
    }

    .diff {
      background: rgba(255, 255, 255, 0.08);
    }

    .diff-add {
      color: #7ee787;
    }

    .diff-remove {
      color: #ffa198;
    }

    .error-box {
      background: #4a1f1a;
      color: #ffb3a7;
    }

    .warn-box {
      background: #4a3a14;
      color: #ffd98a;
    }

    .amount-label input {
      color: #ffffff;
      background-color: #0f0f0f98;
      border-color: rgba(255, 255, 255, 0.2);
    }

    button,
    .button-link {
      color: #ffffff;
      background-color: #0f0f0f98;
      border-color: rgba(255, 255, 255, 0.2);
    }

    button.primary {
      background-color: #396cd8;
      border-color: #396cd8;
    }

    button.danger {
      background-color: #b42318;
      border-color: #b42318;
    }
  }
</style>
