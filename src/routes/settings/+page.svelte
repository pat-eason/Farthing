<script lang="ts">
  // Settings view (task 2.3): start-at-login toggle backed by the
  // autostart commands. State is always re-read from the plugin after a
  // toggle, so the UI shows what is actually registered (including changes
  // made outside the app). Dev builds refuse to enable; the toggle shows
  // why instead of pretending it worked.
  import { resolve } from "$app/paths";
  import { getAutostartStatus, setAutostart, type AutostartStatus } from "$lib/autostart";

  let status: AutostartStatus | undefined = $state();
  let busy = $state(false);
  let errorMessage = $state("");

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

  $effect(() => {
    void refresh();
  });
</script>

<main class="container">
  <h1>Settings</h1>

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

  <div class="row">
    <button onclick={() => void refresh()}>Refresh</button>
    <a class="button-link" href={resolve("/health")}>Health</a>
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

    .error-box {
      background: #4a1f1a;
      color: #ffb3a7;
    }

    .warn-box {
      background: #4a3a14;
      color: #ffd98a;
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
