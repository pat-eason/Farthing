<script lang="ts">
  // Spend view: daily and monthly budget cards (USD amount + enable toggle)
  // plus a "show budgets in tray" toggle. Editing is debounced auto-save:
  // typing a valid amount (>= $1) schedules a save ~400ms later; toggles and
  // blur save immediately. The config field carries notify/approach_pct for
  // the deferred cost-notifications work, so we preserve their defaults on
  // every save rather than surfacing controls for them. On a save failure we
  // revert to the last server-confirmed config and show an inline error.
  import { resolve } from "$app/paths";
  import { getBudgetConfig, setBudgetConfig, type BudgetConfig } from "$lib/budgets";

  // The deferred cost-notifications config fields. We never surface controls
  // for these, but every save must preserve their defaults.
  const NOTIFY_DEFAULT = true;
  const APPROACH_PCT_DEFAULT = 76;
  const MIN_AMOUNT = 1;
  const SAVE_DEBOUNCE_MS = 400;

  type Period = "daily" | "monthly";

  // `config` is the working/optimistic copy bound to the inputs; `confirmed`
  // is the last value the backend accepted, used to revert on save failure.
  let config: BudgetConfig | undefined = $state();
  let confirmed: BudgetConfig | undefined = $state();
  let errorMessage = $state("");
  // Per-field inline validation messages (cleared once the field is valid).
  let fieldError: Record<Period, string> = $state({ daily: "", monthly: "" });

  let saveTimer: ReturnType<typeof setTimeout> | undefined;

  function clone(c: BudgetConfig): BudgetConfig {
    return {
      daily: { ...c.daily },
      monthly: { ...c.monthly },
      show_in_tray: c.show_in_tray,
      approach_pct: c.approach_pct,
    };
  }

  async function load() {
    errorMessage = "";
    try {
      const loaded = await getBudgetConfig();
      confirmed = loaded;
      config = clone(loaded);
    } catch (err) {
      errorMessage = String(err);
    }
  }

  // True when the amount for an enabled budget is a valid number >= $1.
  // Disabled budgets never block a save (their amount is ignored).
  function amountValid(period: Period): boolean {
    if (!config) return false;
    const budget = config[period];
    if (!budget.enabled) return true;
    return Number.isFinite(budget.amount_usd) && budget.amount_usd >= MIN_AMOUNT;
  }

  function validate(period: Period): boolean {
    const ok = amountValid(period);
    fieldError[period] = ok ? "" : `Budget must be at least $${MIN_AMOUNT}`;
    return ok;
  }

  function revert() {
    if (confirmed) config = clone(confirmed);
    fieldError = { daily: "", monthly: "" };
  }

  async function save() {
    if (saveTimer) {
      clearTimeout(saveTimer);
      saveTimer = undefined;
    }
    if (!config) return;
    // Skip the save entirely if either enabled budget has an invalid amount;
    // the field-level error is already shown and the field is left as typed.
    if (!validate("daily") || !validate("monthly")) return;

    errorMessage = "";
    // Always persist the full config, preserving the deferred CN fields at
    // their defaults rather than carrying over any stale values.
    const payload: BudgetConfig = {
      daily: { ...config.daily, notify: NOTIFY_DEFAULT },
      monthly: { ...config.monthly, notify: NOTIFY_DEFAULT },
      show_in_tray: config.show_in_tray,
      approach_pct: APPROACH_PCT_DEFAULT,
    };
    try {
      const result = await setBudgetConfig(payload);
      confirmed = result;
      // Adopt the (possibly clamped) server result so the inputs reflect what
      // was actually stored.
      config = clone(result);
    } catch (err) {
      errorMessage = String(err);
      revert();
    }
  }

  function scheduleSave() {
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      saveTimer = undefined;
      void save();
    }, SAVE_DEBOUNCE_MS);
  }

  // Debounced path: typing into the amount input. Validate inline (without
  // reverting the field) and only schedule a save when valid.
  function onAmountInput(period: Period) {
    if (validate(period)) scheduleSave();
  }

  // Immediate path: leaving the amount field commits right away.
  function onAmountBlur() {
    void save();
  }

  function toggleEnabled(period: Period) {
    if (!config) return;
    config[period].enabled = !config[period].enabled;
    // Re-validate so an enabled-but-empty amount surfaces its error, and a
    // disabled budget clears any stale error.
    validate(period);
    void save();
  }

  function toggleTray() {
    if (!config) return;
    config.show_in_tray = !config.show_in_tray;
    void save();
  }

  $effect(() => {
    void load();
  });
</script>

<main class="container">
  <h1>Spend</h1>

  {#if !config}
    <p class="muted">Loading…</p>
  {:else}
    {#if errorMessage}
      <p class="error-box">{errorMessage}</p>
    {/if}

    {#each [{ period: "daily" as const, title: "Daily budget", help: "Alerts you as today's spend approaches this amount." }, { period: "monthly" as const, title: "Monthly budget", help: "Tracks spend across the current calendar month." }] as card (card.period)}
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
              disabled={!config[card.period].enabled}
              bind:value={config[card.period].amount_usd}
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
            class:primary={!config[card.period].enabled}
            onclick={() => toggleEnabled(card.period)}
            aria-pressed={config[card.period].enabled}
          >
            {config[card.period].enabled ? "Turn off" : "Turn on"}
          </button>
          <span class="state {config[card.period].enabled ? 'good' : 'muted'}">
            {config[card.period].enabled ? "On" : "Off"}
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
          class:primary={!config.show_in_tray}
          onclick={toggleTray}
          aria-pressed={config.show_in_tray}
        >
          {config.show_in_tray ? "Turn off" : "Turn on"}
        </button>
        <span class="state {config.show_in_tray ? 'good' : 'muted'}">
          {config.show_in_tray ? "On" : "Off"}
        </span>
      </div>
    </section>
  {/if}

  <div class="row">
    <a class="button-link" href={resolve("/(app)/cost")}>Cost over time</a>
    <a class="button-link" href={resolve("/(app)/settings")}>Settings</a>
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

  .error-box {
    border-radius: 8px;
    padding: 0.75rem 1rem;
    background: #fdecea;
    color: #8a1f11;
    margin: 0.5rem 0 0;
  }

  .row {
    display: flex;
    gap: 0.75rem;
    margin-top: 1.25rem;
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

  button:disabled {
    opacity: 0.6;
    cursor: default;
  }

  button:hover:not(:disabled),
  .button-link:hover {
    border-color: #396cd8;
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

    .amount-label input {
      color: #ffffff;
      background-color: #0f0f0f98;
      border-color: rgba(255, 255, 255, 0.2);
    }

    .error-box {
      background: #4a1f1a;
      color: #ffb3a7;
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
  }
</style>
