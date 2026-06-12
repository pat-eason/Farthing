<script lang="ts">
  // Spend view (cost-notifications plan, Unit 6): hosts the two budget-
  // independent alerts — a recurring delta ("every $N of spend") and a
  // session/burst rate rule ("$N in a rolling window") — plus notification
  // permission management, a residency warning, the "I pay per-token" copy
  // flag, and per-rule test buttons. (Budget approach/breach cards arrive
  // with the Budgets plan; this page is built so they slot in beside these.)
  //
  // Save model: toggles persist immediately; numeric/time inputs persist on a
  // 500ms debounce after the last keystroke AND on blur. Every save round-trips
  // the backend's returned config back into $state, so the UI shows what was
  // stored, not what was typed. A validation failure blocks the save and shows
  // an inline error WITHOUT reverting (so the user can fix the value); a backend
  // failure reverts the field to the last confirmed value and shows the error.
  //
  // Race mitigations:
  //   - Per-rule edit generation counters: a save captures its generation at
  //     dispatch; if the user edits the rule during the round-trip (generation
  //     advances), the server echo is discarded for that rule's inputs.
  //   - Scoped sync helpers (syncDelta/syncBurst/syncMeta): a delta save/revert
  //     only touches delta mirrors; burst mirrors are left alone, and vice versa.
  //   - Blur guards: blur clears the pending debounce timer and bails if the
  //     rule is already busy (no double-save on blur + pending debounce).
  import { getAutostartStatus, setAutostart, type AutostartStatus } from "$lib/autostart";
  import {
    getAlertConfig,
    setAlertConfig,
    getNotificationPermission,
    requestNotificationPermission,
    sendTestNotification,
    type AlertConfig,
    type DeltaConfig,
    type BurstConfig,
    type QuietWindow,
    type AlertRuleType,
  } from "$lib/spend";
  import { openUrl } from "@tauri-apps/plugin-opener";

  // The macOS deep link straight to the notifications pane of System Settings.
  // The only recovery path once permission is Denied: macOS won't let us
  // re-prompt programmatically (plan / notify.rs), so we open the pane the user
  // flips the switch in.
  const NOTIFICATION_SETTINGS_URL = "x-apple.systempreferences:com.apple.preference.notifications";

  // Debounce window for numeric/time inputs: long enough to coalesce a burst of
  // keystrokes, short enough that a save feels immediate after the user stops.
  const SAVE_DEBOUNCE_MS = 500;

  // --- Loading + shared state ----------------------------------------------
  // `config` is the last value the backend confirmed; inputs bind to local
  // mirrors (below) so a failed save can revert to it. `undefined` while the
  // page is still resolving its three reads, which gates the skeleton.
  let config: AlertConfig | undefined = $state();
  let permission = $state("");
  let autostart: AutostartStatus | undefined = $state();
  let loadError = $state("");

  // API-billing copy flag drives whether labels read as real money or neutral
  // usage. Page-scoped: it persists in the alert config but does not touch the
  // tray or other sections (cost-notifications plan scope).
  let apiBilling = $state(false);

  // Per-rule local input mirrors. Bound to the form fields so we can validate
  // and revert independently of the confirmed `config`. Strings for numeric
  // fields so an in-progress/empty entry doesn't coerce to 0 mid-edit.
  let deltaEnabled = $state(false);
  let deltaStep = $state("");
  let deltaQuietOpen = $state(false);
  let deltaQuietStart = $state("");
  let deltaQuietEnd = $state("");

  let burstEnabled = $state(false);
  let burstThreshold = $state("");
  let burstWindow = $state("");
  let burstCooldown = $state("");
  let burstQuietOpen = $state(false);
  let burstQuietStart = $state("");
  let burstQuietEnd = $state("");

  // Inline error text, keyed by rule. Cleared on the next successful save.
  let deltaError = $state("");
  let burstError = $state("");
  // A save is in flight for the rule; disables its controls briefly.
  let deltaBusy = $state(false);
  let burstBusy = $state(false);

  // Permission request transient + error.
  let permissionBusy = $state(false);
  let permissionError = $state("");
  // Separate test-notification errors per rule so each card-foot shows its own.
  let deltaTestError = $state("");
  let burstTestError = $state("");

  // Residency (autostart) toggle state.
  let residencyBusy = $state(false);
  let residencyError = $state("");

  // Pending debounce timers, one per rule, so the latest keystroke wins.
  let deltaTimer: ReturnType<typeof setTimeout> | undefined;
  let burstTimer: ReturnType<typeof setTimeout> | undefined;

  // Per-rule monotonic edit-generation counters. Incremented on every user
  // input event. A save captures the current generation at dispatch; if it has
  // advanced by the time the await resolves, the user edited during the round-
  // trip and we discard the server echo for that rule's numeric/time mirrors.
  let deltaGen = 0;
  let burstGen = 0;

  // --- Scoped sync helpers --------------------------------------------------

  /** Hydrate delta mirrors only from a confirmed config. */
  function syncDelta(c: AlertConfig) {
    config = c;
    deltaEnabled = c.delta.enabled;
    deltaStep = String(c.delta.step_usd);
    deltaQuietStart = c.delta.quiet?.start ?? "";
    deltaQuietEnd = c.delta.quiet?.end ?? "";
    deltaQuietOpen = deltaQuietOpen || c.delta.quiet !== null;
  }

  /** Hydrate burst mirrors only from a confirmed config. */
  function syncBurst(c: AlertConfig) {
    config = c;
    burstEnabled = c.burst.enabled;
    burstThreshold = String(c.burst.threshold_usd);
    burstWindow = String(c.burst.window_minutes);
    burstCooldown = String(c.burst.cooldown_minutes);
    burstQuietStart = c.burst.quiet?.start ?? "";
    burstQuietEnd = c.burst.quiet?.end ?? "";
    burstQuietOpen = burstQuietOpen || c.burst.quiet !== null;
  }

  /** Hydrate the api_billing mirror only from a confirmed config. */
  function syncMeta(c: AlertConfig) {
    config = c;
    apiBilling = c.api_billing;
  }

  /** Full hydration for the initial mount load. */
  function syncAll(c: AlertConfig) {
    syncMeta(c);
    syncDelta(c);
    syncBurst(c);
  }

  async function refresh() {
    loadError = "";
    try {
      const [c, p, a] = await Promise.all([
        getAlertConfig(),
        getNotificationPermission(),
        getAutostartStatus(),
      ]);
      syncAll(c);
      permission = p;
      autostart = a;
    } catch (err) {
      loadError = String(err);
    }
  }

  // --- Quiet-hours helpers --------------------------------------------------

  /**
   * Resolve the two time strings into a {@link QuietWindow}, or `null` (the JSON
   * boundary value the backend stores for "always allowed"). `start === end` is
   * treated as unset, so dragging both fields to the same time clears the window
   * rather than persisting a zero-width one.
   */
  function resolveQuiet(start: string, end: string): QuietWindow | null {
    if (!start || !end || start === end) return null;
    return { start, end };
  }

  /** A 24h "HH:MM" rendered as "10:00 PM" for the collapsed summary. */
  function formatClock(hhmm: string): string {
    const [h, m] = hhmm.split(":").map((n) => Number(n));
    if (Number.isNaN(h) || Number.isNaN(m)) return hhmm;
    const period = h < 12 ? "AM" : "PM";
    const hour12 = h % 12 === 0 ? 12 : h % 12;
    return `${hour12}:${String(m).padStart(2, "0")} ${period}`;
  }

  /** Collapsed quiet-hours summary: "off", a window, or "overnight" when it wraps. */
  function quietSummary(start: string, end: string): string {
    const quiet = resolveQuiet(start, end);
    if (!quiet) return "Quiet hours: off";
    const wraps = quiet.start > quiet.end;
    return `Quiet hours: ${formatClock(quiet.start)} – ${formatClock(quiet.end)}${
      wraps ? " (overnight)" : ""
    }`;
  }

  // --- Save plumbing --------------------------------------------------------

  /**
   * Persist the whole config (the backend takes the full object). On success,
   * run `applyEcho` with the returned config (caller decides which mirrors to
   * update, taking into account whether the user edited during the round-trip).
   * On failure, run `revert` (which restores the field) and surface the error.
   * `validationError`, when set, blocks the save and shows inline WITHOUT
   * reverting so the user can correct the bad value in place.
   */
  async function save(
    next: AlertConfig,
    setBusy: (b: boolean) => void,
    setError: (s: string) => void,
    revert: () => void,
    applyEcho: (saved: AlertConfig) => void,
    validationError?: string
  ) {
    if (validationError) {
      setError(validationError);
      return;
    }
    setBusy(true);
    setError("");
    try {
      const saved = await setAlertConfig(next);
      applyEcho(saved);
    } catch (err) {
      revert();
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  /** The current confirmed config, or throw — callers only run with it loaded. */
  function current(): AlertConfig {
    if (!config) throw new Error("config not loaded");
    return config;
  }

  // --- Delta rule ------------------------------------------------------------

  function buildDelta(): { next: AlertConfig; validation?: string } {
    const c = current();
    const step = Number(deltaStep);
    let validation: string | undefined;
    if (deltaStep.trim() === "" || Number.isNaN(step) || step <= 0) {
      validation = "Step must be greater than $0.";
    }
    const delta: DeltaConfig = {
      enabled: deltaEnabled,
      step_usd: validation ? c.delta.step_usd : step,
      quiet: resolveQuiet(deltaQuietStart, deltaQuietEnd),
    };
    return { next: { ...c, delta }, validation };
  }

  function revertDelta() {
    if (config) syncDelta(config);
  }

  function saveDelta() {
    const { next, validation } = buildDelta();
    // Capture the generation at dispatch; discard server echo if the user edits
    // during the round-trip (generation will have advanced).
    const genAtDispatch = deltaGen;
    void save(
      next,
      (b) => (deltaBusy = b),
      (s) => (deltaError = s),
      revertDelta,
      (saved) => {
        // Always update the authoritative config reference; only overwrite the
        // numeric/time mirrors if the user hasn't typed since dispatch.
        if (deltaGen === genAtDispatch) {
          syncDelta(saved);
        } else {
          config = saved;
        }
      },
      validation
    );
  }

  /** Enable toggles save immediately. */
  function toggleDelta() {
    if (!config || deltaBusy) return;
    deltaEnabled = !deltaEnabled;
    saveDelta();
  }

  /** Debounced save for the delta numeric/time fields. */
  function deltaChanged() {
    deltaGen++;
    clearTimeout(deltaTimer);
    deltaTimer = setTimeout(saveDelta, SAVE_DEBOUNCE_MS);
  }

  /**
   * Blur flushes a pending debounce immediately; bails if a save is already
   * in flight so blur + a live timer don't double-save.
   */
  function deltaBlur() {
    if (deltaBusy) return;
    clearTimeout(deltaTimer);
    deltaTimer = undefined;
    saveDelta();
  }

  // --- Burst rule ------------------------------------------------------------

  function buildBurst(): { next: AlertConfig; validation?: string } {
    const c = current();
    const threshold = Number(burstThreshold);
    const windowMin = Number(burstWindow);
    const cooldown = Number(burstCooldown);
    let validation: string | undefined;
    if (burstThreshold.trim() === "" || Number.isNaN(threshold) || threshold <= 0) {
      validation = "Threshold must be greater than $0.";
    } else if (burstWindow.trim() === "" || Number.isNaN(windowMin) || windowMin < 1) {
      validation = "Window must be at least 1 minute.";
    } else if (burstCooldown.trim() === "" || Number.isNaN(cooldown) || cooldown < 1) {
      validation = "Cooldown must be at least 1 minute.";
    }
    const burst: BurstConfig = {
      enabled: burstEnabled,
      threshold_usd: validation ? c.burst.threshold_usd : threshold,
      window_minutes: validation ? c.burst.window_minutes : Math.round(windowMin),
      cooldown_minutes: validation ? c.burst.cooldown_minutes : Math.round(cooldown),
      quiet: resolveQuiet(burstQuietStart, burstQuietEnd),
    };
    return { next: { ...c, burst }, validation };
  }

  function revertBurst() {
    if (config) syncBurst(config);
  }

  function saveBurst() {
    const { next, validation } = buildBurst();
    const genAtDispatch = burstGen;
    void save(
      next,
      (b) => (burstBusy = b),
      (s) => (burstError = s),
      revertBurst,
      (saved) => {
        if (burstGen === genAtDispatch) {
          syncBurst(saved);
        } else {
          config = saved;
        }
      },
      validation
    );
  }

  function toggleBurst() {
    if (!config || burstBusy) return;
    burstEnabled = !burstEnabled;
    saveBurst();
  }

  function burstChanged() {
    burstGen++;
    clearTimeout(burstTimer);
    burstTimer = setTimeout(saveBurst, SAVE_DEBOUNCE_MS);
  }

  /**
   * Blur flushes a pending debounce immediately; bails if a save is already
   * in flight so blur + a live timer don't double-save.
   */
  function burstBlur() {
    if (burstBusy) return;
    clearTimeout(burstTimer);
    burstTimer = undefined;
    saveBurst();
  }

  // --- API-billing copy flag -------------------------------------------------

  function toggleApiBilling() {
    if (!config) return;
    apiBilling = !apiBilling;
    const next: AlertConfig = { ...current(), api_billing: apiBilling };
    void save(
      next,
      () => {},
      (s) => (loadError = s),
      () => {
        if (config) syncMeta(config);
      },
      (saved) => syncMeta(saved)
    );
  }

  // Copy that flips with the billing flag. Neutral "usage" wording by default;
  // real-money wording when the user pays per token.
  const unitWord = $derived(apiBilling ? "spend" : "usage");
  const deltaTitle = $derived(apiBilling ? "Spend milestone alert" : "Usage milestone alert");
  const burstTitle = $derived(apiBilling ? "Spend spike alert" : "Usage spike alert");

  // --- Permission surface ----------------------------------------------------

  async function requestPermission() {
    permissionBusy = true;
    permissionError = "";
    try {
      permission = await requestNotificationPermission();
    } catch (err) {
      permissionError = String(err);
    } finally {
      permissionBusy = false;
    }
  }

  async function openSettings() {
    permissionError = "";
    try {
      await openUrl(NOTIFICATION_SETTINGS_URL);
    } catch (err) {
      permissionError = String(err);
    }
  }

  async function sendTest(rule: AlertRuleType) {
    if (rule === "delta") {
      deltaTestError = "";
      try {
        await sendTestNotification(rule);
      } catch (err) {
        deltaTestError = String(err);
      }
    } else {
      burstTestError = "";
      try {
        await sendTestNotification(rule);
      } catch (err) {
        burstTestError = String(err);
      }
    }
  }

  // Permission "lost" (revoked after a grant) is surfaced by the backend on the
  // alert runtime, but it manifests here the same way as a plain Denied: the OS
  // reports "denied" and the only recovery is System Settings. Both render the
  // warn-box; the never-requested ("prompt"/"") state gets the request button.
  // never-requested ("prompt"/"") gets the request button (the template's
  // {:else}); granted shows "On"; denied/lost both deep-link to System Settings.
  const permissionGranted = $derived(permission === "granted");
  const permissionDenied = $derived(permission === "denied");

  // Residency: the app must be running for alerts to fire. Warn only when
  // autostart is off AND this is a real build (dev builds can't enable it, so
  // the warning would be unactionable noise).
  const showResidencyWarning = $derived(
    autostart !== undefined && !autostart.enabled && !autostart.dev_build
  );

  async function enableAutostart() {
    residencyBusy = true;
    residencyError = "";
    try {
      autostart = await setAutostart(true);
    } catch (err) {
      residencyError = String(err);
      try {
        autostart = await getAutostartStatus();
      } catch {
        // keep the original error
      }
    } finally {
      residencyBusy = false;
    }
  }

  $effect(() => {
    void refresh();
    // Return teardown so pending debounce timers don't fire into a torn-down
    // component after navigation.
    return () => {
      clearTimeout(deltaTimer);
      clearTimeout(burstTimer);
    };
  });
</script>

<main class="container">
  <h1>Spend alerts</h1>

  {#if !config}
    <p class="muted">Loading…</p>
    {#if loadError}
      <p class="error-box">{loadError}</p>
    {/if}
  {:else}
    {#if loadError}
      <p class="error-box">{loadError}</p>
    {/if}

    <!-- Notification permission gate: nothing fires without it. -->
    <section class="setting">
      <div class="setting-text">
        <h2>Notifications</h2>
        {#if permissionGranted}
          <p class="muted">Allowed. Alerts below will fire as native notifications.</p>
        {:else if permissionDenied}
          <p class="warn-box">
            Notifications are turned off for Farthing, so no alert can reach you. macOS won't let
            the app re-ask; flip it back on in System Settings.
          </p>
        {:else}
          <p class="muted">
            Alerts are delivered as native notifications. Grant permission once to turn them on.
          </p>
        {/if}
        {#if permissionError}
          <p class="error-box">{permissionError}</p>
        {/if}
      </div>
      <div class="setting-control">
        {#if permissionGranted}
          <span class="state good">On</span>
        {:else if permissionDenied}
          <button onclick={() => void openSettings()}>Open System Settings</button>
        {:else}
          <button
            class="primary"
            disabled={permissionBusy}
            onclick={() => void requestPermission()}
          >
            {permissionBusy ? "Requesting…" : "Allow notifications"}
          </button>
        {/if}
      </div>
    </section>

    {#if showResidencyWarning}
      <!-- Alerts only run in-process: warn if the app won't be around to fire them. -->
      <section class="setting">
        <div class="setting-text">
          <h2>Keep Farthing running</h2>
          <p class="warn-box">
            Alerts only run while Farthing is open. Start it at login so a spike or milestone isn't
            missed while the app is closed.
          </p>
          {#if residencyError}
            <p class="error-box">{residencyError}</p>
          {/if}
        </div>
        <div class="setting-control">
          <button class="primary" disabled={residencyBusy} onclick={() => void enableAutostart()}>
            {residencyBusy ? "Working…" : "Start at login"}
          </button>
        </div>
      </section>
    {/if}

    <!-- Page-scoped copy flag: switches neutral "usage" labels to real money. -->
    <section class="setting">
      <div class="setting-text">
        <h2>I pay per token</h2>
        <p class="muted">
          Switches alert wording on this page from neutral "usage" to real-money "spend". Doesn't
          change the tray or other views.
        </p>
      </div>
      <div class="setting-control">
        <button aria-pressed={apiBilling} onclick={() => toggleApiBilling()}>
          {apiBilling ? "On" : "Off"}
        </button>
      </div>
    </section>

    <!-- Delta rule: every $N of accumulated monthly spend. -->
    <section class="card" class:disabled-card={!deltaEnabled}>
      <div class="card-head">
        <div>
          <h2>{deltaTitle}</h2>
          <p class="muted">
            Fires once for every milestone of {unitWord} this month — a steady drumbeat as cost accumulates.
          </p>
        </div>
        <button
          class:primary={!deltaEnabled}
          disabled={deltaBusy}
          aria-pressed={deltaEnabled}
          onclick={() => toggleDelta()}
        >
          {deltaEnabled ? "On" : "Off"}
        </button>
      </div>

      <div class="field">
        <label for="delta-step">Notify every</label>
        <div class="input-wrap">
          <span class="prefix">$</span>
          <input
            id="delta-step"
            type="number"
            min="1"
            step="1"
            inputmode="decimal"
            disabled={!deltaEnabled || deltaBusy}
            bind:value={deltaStep}
            oninput={deltaChanged}
            onblur={deltaBlur}
          />
        </div>
        <span class="suffix">of {unitWord}</span>
      </div>

      {#if deltaError}
        <p class="error-box">{deltaError}</p>
      {/if}

      <div class="quiet">
        <button
          type="button"
          class="quiet-toggle"
          aria-expanded={deltaQuietOpen}
          onclick={() => (deltaQuietOpen = !deltaQuietOpen)}
        >
          <span class="chevron" class:open={deltaQuietOpen}>›</span>
          {quietSummary(deltaQuietStart, deltaQuietEnd)}
        </button>
        {#if deltaQuietOpen}
          <div class="quiet-fields">
            <label>
              From
              <input
                type="time"
                disabled={!deltaEnabled || deltaBusy}
                bind:value={deltaQuietStart}
                onchange={deltaBlur}
              />
            </label>
            <label>
              To
              <input
                type="time"
                disabled={!deltaEnabled || deltaBusy}
                bind:value={deltaQuietEnd}
                onchange={deltaBlur}
              />
            </label>
            <span class="muted hint">Same start and end clears quiet hours.</span>
          </div>
        {/if}
      </div>

      <div class="card-foot">
        <button disabled={!permissionGranted} onclick={() => void sendTest("delta")}>
          Send a test
        </button>
        {#if deltaTestError}
          <span class="error-inline">{deltaTestError}</span>
        {/if}
      </div>
    </section>

    <!-- Burst rule: $N inside a rolling window — catches a runaway loop. -->
    <section class="card" class:disabled-card={!burstEnabled}>
      <div class="card-head">
        <div>
          <h2>{burstTitle}</h2>
          <p class="muted">
            Fires when {unitWord} climbs fast inside a short window — the signal a runaway agent loop
            gives off that the tray total can't.
          </p>
        </div>
        <button
          class:primary={!burstEnabled}
          disabled={burstBusy}
          aria-pressed={burstEnabled}
          onclick={() => toggleBurst()}
        >
          {burstEnabled ? "On" : "Off"}
        </button>
      </div>

      <div class="field">
        <label for="burst-threshold">Alert at</label>
        <div class="input-wrap">
          <span class="prefix">$</span>
          <input
            id="burst-threshold"
            type="number"
            min="1"
            step="1"
            inputmode="decimal"
            disabled={!burstEnabled || burstBusy}
            bind:value={burstThreshold}
            oninput={burstChanged}
            onblur={burstBlur}
          />
        </div>
        <span class="suffix">of {unitWord} within</span>
        <div class="input-wrap">
          <input
            id="burst-window"
            class="narrow"
            type="number"
            min="1"
            step="1"
            inputmode="numeric"
            disabled={!burstEnabled || burstBusy}
            bind:value={burstWindow}
            oninput={burstChanged}
            onblur={burstBlur}
          />
        </div>
        <span class="suffix">min</span>
      </div>

      <div class="field">
        <label for="burst-cooldown">Then wait</label>
        <div class="input-wrap">
          <input
            id="burst-cooldown"
            class="narrow"
            type="number"
            min="1"
            step="1"
            inputmode="numeric"
            disabled={!burstEnabled || burstBusy}
            bind:value={burstCooldown}
            oninput={burstChanged}
            onblur={burstBlur}
          />
        </div>
        <span class="suffix">min before alerting again</span>
      </div>

      {#if burstError}
        <p class="error-box">{burstError}</p>
      {/if}

      <div class="quiet">
        <button
          type="button"
          class="quiet-toggle"
          aria-expanded={burstQuietOpen}
          onclick={() => (burstQuietOpen = !burstQuietOpen)}
        >
          <span class="chevron" class:open={burstQuietOpen}>›</span>
          {quietSummary(burstQuietStart, burstQuietEnd)}
        </button>
        {#if burstQuietOpen}
          <div class="quiet-fields">
            <label>
              From
              <input
                type="time"
                disabled={!burstEnabled || burstBusy}
                bind:value={burstQuietStart}
                onchange={burstBlur}
              />
            </label>
            <label>
              To
              <input
                type="time"
                disabled={!burstEnabled || burstBusy}
                bind:value={burstQuietEnd}
                onchange={burstBlur}
              />
            </label>
            <span class="muted hint">Same start and end clears quiet hours.</span>
          </div>
        {/if}
      </div>

      <div class="card-foot">
        <button disabled={!permissionGranted} onclick={() => void sendTest("burst")}>
          Send a test
        </button>
        {#if burstTestError}
          <span class="error-inline">{burstTestError}</span>
        {/if}
      </div>
    </section>
  {/if}
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

  .muted {
    color: #6b6b6b;
  }

  .good {
    color: #1a7f37;
  }

  .hint {
    font-size: 0.85em;
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

  .setting-control {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    white-space: nowrap;
  }

  .state {
    font-size: 0.9em;
  }

  .card {
    margin-top: 1.25rem;
    padding: 1.1rem 1.25rem;
    border: 1px solid rgba(0, 0, 0, 0.12);
    border-radius: 12px;
    background: #ffffff;
  }

  .card.disabled-card {
    background: #fbfbfc;
  }

  .card-head {
    display: flex;
    gap: 1rem;
    align-items: flex-start;
    justify-content: space-between;
  }

  .card-head .muted {
    max-width: 36ch;
  }

  .field {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.5rem;
    margin-top: 0.9rem;
  }

  .field > label {
    font-weight: 500;
  }

  .input-wrap {
    display: inline-flex;
    align-items: center;
    border: 1px solid rgba(0, 0, 0, 0.2);
    border-radius: 8px;
    padding: 0 0.5em;
    background: #ffffff;
  }

  .input-wrap:focus-within {
    border-color: #396cd8;
  }

  .prefix {
    color: #6b6b6b;
  }

  input[type="number"] {
    width: 5rem;
    border: none;
    padding: 0.45em 0.3em;
    font-size: 0.95em;
    font-family: inherit;
    background: transparent;
    color: inherit;
  }

  input[type="number"].narrow {
    width: 3.5rem;
  }

  input:focus {
    outline: none;
  }

  input:disabled {
    color: #9b9b9b;
  }

  input[type="time"] {
    border: 1px solid rgba(0, 0, 0, 0.2);
    border-radius: 8px;
    padding: 0.4em 0.5em;
    font-family: inherit;
    font-size: 0.95em;
    background: #ffffff;
    color: inherit;
  }

  .suffix {
    color: #6b6b6b;
  }

  .quiet {
    margin-top: 0.9rem;
  }

  .quiet-toggle {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    border: none;
    background: transparent;
    padding: 0.25rem 0;
    color: #396cd8;
    cursor: pointer;
    font-size: 0.9em;
  }

  .chevron {
    display: inline-block;
    transition: transform 0.12s ease;
  }

  .chevron.open {
    transform: rotate(90deg);
  }

  .quiet-fields {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.75rem;
    margin-top: 0.5rem;
    padding-left: 1rem;
  }

  .quiet-fields label {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.9em;
  }

  .card-foot {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-top: 1rem;
  }

  .error-inline {
    color: #b42318;
    font-size: 0.85em;
  }

  .error-box,
  .warn-box {
    border-radius: 8px;
    padding: 0.75rem 1rem;
    margin: 0.5rem 0 0;
  }

  .error-box {
    background: #fdecea;
    color: #8a1f11;
  }

  .warn-box {
    background: #fff4e0;
    color: #6b4a00;
  }

  button {
    border-radius: 8px;
    border: 1px solid rgba(0, 0, 0, 0.15);
    padding: 0.55em 1.1em;
    font-size: 0.95em;
    font-weight: 500;
    font-family: inherit;
    color: #0f0f0f;
    background-color: #ffffff;
    cursor: pointer;
  }

  button.primary {
    background-color: #396cd8;
    border-color: #396cd8;
    color: #ffffff;
  }

  button:disabled {
    opacity: 0.6;
    cursor: default;
  }

  button:hover:not(:disabled) {
    border-color: #396cd8;
  }

  button.quiet-toggle:hover:not(:disabled) {
    border-color: transparent;
  }

  @media (prefers-color-scheme: dark) {
    .muted {
      color: #a8a8a8;
    }

    .good {
      color: #7ee787;
    }

    .prefix,
    .suffix {
      color: #a8a8a8;
    }

    .setting {
      border-bottom-color: rgba(255, 255, 255, 0.18);
    }

    .card {
      background: #1f1f21;
      border-color: rgba(255, 255, 255, 0.16);
    }

    .card.disabled-card {
      background: #242426;
    }

    .input-wrap {
      background: #0f0f0f60;
      border-color: rgba(255, 255, 255, 0.2);
    }

    input[type="time"] {
      background: #0f0f0f60;
      border-color: rgba(255, 255, 255, 0.2);
      color: #f5f5f7;
    }

    input:disabled {
      color: #6b6b6b;
    }

    .error-box {
      background: #4a1f1a;
      color: #ffb3a7;
    }

    .warn-box {
      background: #4a3a14;
      color: #ffd98a;
    }

    .quiet-toggle {
      color: #6ea8ff;
    }

    .error-inline {
      color: #ffa198;
    }

    button {
      color: #ffffff;
      background-color: #0f0f0f98;
      border-color: rgba(255, 255, 255, 0.2);
    }

    button.primary {
      background-color: #396cd8;
      border-color: #396cd8;
    }

    button.quiet-toggle {
      background: transparent;
    }
  }
</style>
