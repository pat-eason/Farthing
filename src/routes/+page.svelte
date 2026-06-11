<script lang="ts">
  // Onboarding flow (task 2.2): detect config state, preview the exact
  // settings.json diff, surface conflicts for an explicit choice, apply the
  // merge, then tell the user to restart running Claude Code sessions.
  // Nothing is written until the user confirms on the preview (or conflict)
  // screen; re-running on a configured machine lands on "already configured".
  import { resolve } from "$app/paths";
  import {
    applyOnboarding,
    getOnboardingStatus,
    type ApplyOutcome,
    type OnboardingStatus,
  } from "$lib/onboarding";

  type Screen = "loading" | "error" | "configured" | "preview" | "conflicts" | "applying" | "done";

  let screen: Screen = $state("loading");
  let status: OnboardingStatus | undefined = $state();
  let outcome: ApplyOutcome | undefined = $state();
  let errorMessage = $state("");

  async function refresh() {
    screen = "loading";
    try {
      status = await getOnboardingStatus();
      screen = status.changed ? "preview" : "configured";
    } catch (err) {
      errorMessage = String(err);
      screen = "error";
    }
  }

  // The preview's confirm button: with conflicts present, route through the
  // conflict screen so overwriting is an explicit, separate choice.
  function confirmFromPreview() {
    if (status && status.conflicts.length > 0) {
      screen = "conflicts";
    } else {
      void apply(false);
    }
  }

  async function apply(acknowledgeConflicts: boolean) {
    screen = "applying";
    try {
      outcome = await applyOnboarding(acknowledgeConflicts);
      screen = "done";
    } catch (err) {
      errorMessage = String(err);
      screen = "error";
    }
  }

  $effect(() => {
    void refresh();
  });
</script>

<main class="container">
  {#if screen === "loading"}
    <p class="muted">Checking your Claude Code configuration…</p>
  {:else if screen === "error"}
    <h1>Something went wrong</h1>
    <p class="error-box">{errorMessage}</p>
    <p class="muted">
      Your <code>settings.json</code> has not been modified. Fix the issue and try again.
    </p>
    <button onclick={() => void refresh()}>Try again</button>
  {:else if screen === "configured"}
    <h1>Already configured</h1>
    <p>
      <code>{status?.settings_path}</code> already contains everything this app needs. Running setup again
      would change nothing.
    </p>
    <div class="row">
      <a class="button-link" href={resolve("/health")}>Open health view</a>
      <button onclick={() => void refresh()}>Re-check</button>
    </div>
  {:else if screen === "preview" && status}
    <h1>Set up usage tracking</h1>
    <p>
      To receive usage events, the app needs to add telemetry settings and a session hook to <code
        >{status.settings_path}</code
      >. Here is the exact change; <strong>nothing is written until you confirm</strong>. A
      timestamped backup is saved first.
    </p>
    {#if status.conflicts.length > 0}
      <p class="warn-box">
        Existing telemetry settings were found ({status.conflicts.length}). You'll be asked how to
        handle them on the next step.
      </p>
    {/if}
    <pre class="diff">{#each status.diff as line, i (i)}<span class="diff-{line.kind}"
          >{line.kind === "add" ? "+" : line.kind === "remove" ? "-" : " "} {line.text}
</span>{/each}</pre>
    <div class="row">
      <button class="primary" onclick={confirmFromPreview}>
        {status.conflicts.length > 0 ? "Continue" : "Apply changes"}
      </button>
      <button onclick={() => void refresh()}>Re-check</button>
    </div>
  {:else if screen === "conflicts" && status}
    <h1>Existing telemetry settings found</h1>
    <p>
      Your <code>settings.json</code> already configures telemetry. The app never changes these silently;
      choose what to do:
    </p>
    <table class="conflicts">
      <thead>
        <tr><th>Setting</th><th>Your value</th><th>App value</th></tr>
      </thead>
      <tbody>
        {#each status.conflicts as conflict (conflict.key)}
          <tr>
            <td><code>{conflict.key}</code></td>
            <td><code>{JSON.stringify(conflict.existing)}</code></td>
            <td>
              {#if conflict.proposed !== null}
                <code>{JSON.stringify(conflict.proposed)}</code>
              {:else}
                <span class="muted">left untouched</span>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    <p class="muted">
      Settings with an app value will be overwritten; the rest stay as they are but may interact
      with the app's exporter configuration.
    </p>
    <div class="row">
      <button class="primary" onclick={() => void apply(true)}> Overwrite and continue </button>
      <button onclick={() => (screen = "preview")}> Cancel, change nothing </button>
    </div>
  {:else if screen === "applying"}
    <p class="muted">Updating settings.json…</p>
  {:else if screen === "done"}
    <h1>Setup complete</h1>
    {#if outcome?.backup_path}
      <p class="muted">
        Backup of your previous settings: <code>{outcome.backup_path}</code>
      </p>
    {/if}
    <p class="warn-box">
      <strong>Restart your Claude Code sessions.</strong> Telemetry settings are read when a session starts,
      so sessions that are already running will never send usage data. Start a new session (or restart
      running ones) to begin tracking.
    </p>
    <p>
      <a class="button-link" href={resolve("/health")}>Open the health view</a> to confirm events are
      flowing.
    </p>
  {/if}
</main>

<style>
  :root {
    font-family: Inter, Avenir, Helvetica, Arial, sans-serif;
    font-size: 15px;
    line-height: 1.5;
    color: #0f0f0f;
    background-color: #f6f6f6;
    font-synthesis: none;
    text-rendering: optimizeLegibility;
    -webkit-font-smoothing: antialiased;
  }

  .container {
    max-width: 720px;
    margin: 0 auto;
    padding: 2rem 1.5rem;
    text-align: left;
  }

  h1 {
    font-size: 1.4rem;
  }

  code {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.85em;
    background: rgba(0, 0, 0, 0.06);
    padding: 0.1em 0.3em;
    border-radius: 4px;
  }

  .muted {
    color: #6b6b6b;
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

  .diff {
    background: #1e1e1e;
    color: #d4d4d4;
    border-radius: 8px;
    padding: 0.75rem;
    overflow: auto;
    max-height: 45vh;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.8rem;
    line-height: 1.45;
  }

  .diff span {
    display: block;
    white-space: pre;
  }

  .diff-add {
    background: rgba(63, 185, 80, 0.18);
    color: #7ee787;
  }

  .diff-remove {
    background: rgba(248, 81, 73, 0.18);
    color: #ffa198;
  }

  .conflicts {
    border-collapse: collapse;
    width: 100%;
  }

  .conflicts th,
  .conflicts td {
    text-align: left;
    padding: 0.4rem 0.6rem;
    border-bottom: 1px solid rgba(0, 0, 0, 0.12);
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

  button:hover,
  .button-link:hover {
    border-color: #396cd8;
  }

  @media (prefers-color-scheme: dark) {
    :root {
      color: #f6f6f6;
      background-color: #2f2f2f;
    }

    code {
      background: rgba(255, 255, 255, 0.12);
    }

    .muted {
      color: #a8a8a8;
    }

    .error-box {
      background: #4a1f1a;
      color: #ffb3a7;
    }

    .warn-box {
      background: #4a3a14;
      color: #ffd98a;
    }

    .conflicts th,
    .conflicts td {
      border-bottom-color: rgba(255, 255, 255, 0.18);
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
