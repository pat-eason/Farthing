<script lang="ts">
  // Health & diagnostics view (task 2.5). Renders the single health_status
  // snapshot: receiver state (incl. port conflict with remediation), last
  // event received, settings.json config state, backfill progress, and the
  // ingest counters. The "configured but no events" detector's diagnosis is
  // shown as a warning box with its likely causes. Auto-refreshes while the
  // view is open so a restarted Claude Code session shows up without
  // clicking anything.
  import { resolve } from "$app/paths";
  import { getHealthStatus, type HealthStatus } from "$lib/health";

  const REFRESH_INTERVAL_MS = 5_000;

  let health: HealthStatus | undefined = $state();
  let errorMessage = $state("");

  async function refresh() {
    try {
      health = await getHealthStatus();
      errorMessage = "";
    } catch (err) {
      errorMessage = String(err);
    }
  }

  function formatWhen(ms: number): string {
    return new Date(ms).toLocaleString();
  }

  /** "just now" / "3 minutes ago" / "2 hours ago" / "5 days ago". */
  function formatAgo(ms: number): string {
    const minutes = Math.floor((Date.now() - ms) / 60_000);
    if (minutes < 1) return "just now";
    if (minutes < 60) return minutes === 1 ? "1 minute ago" : `${minutes} minutes ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return hours === 1 ? "1 hour ago" : `${hours} hours ago`;
    const days = Math.floor(hours / 24);
    return days === 1 ? "1 day ago" : `${days} days ago`;
  }

  $effect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), REFRESH_INTERVAL_MS);
    return () => clearInterval(timer);
  });
</script>

<main class="container">
  <h1>Health</h1>

  {#if errorMessage}
    <p class="bad">{errorMessage}</p>
  {:else if !health}
    <p class="muted">Checking…</p>
  {:else}
    {#if health.no_events}
      <section class="warn-box">
        <h2>
          Configured, but no events
          {#if health.no_events.minutes_since_last === null}
            received yet
          {:else}
            in the last {health.no_events.minutes_since_last} minutes
          {/if}
        </h2>
        <p>Likely causes:</p>
        <ul>
          {#each health.no_events.causes as cause (cause.kind)}
            <li>{cause.detail}</li>
          {/each}
        </ul>
      </section>
    {/if}

    <section class="card">
      <h2>Receiver</h2>
      {#if health.receiver.state === "listening"}
        <p class="good">Listening on 127.0.0.1:{health.receiver.port}.</p>
      {:else if health.receiver.state === "port_in_use"}
        <p class="bad">
          Port {health.receiver.port} is already in use by another process, so the receiver could not
          start. The port is never changed automatically (your Claude Code config points at it literally);
          quit whatever is using the port and relaunch this app.
        </p>
      {:else if health.receiver.state === "failed"}
        <p class="bad">Receiver failed: {health.receiver.message}. Relaunch this app.</p>
      {:else}
        <p class="muted">Starting…</p>
      {/if}
    </section>

    <section class="card">
      <h2>Events</h2>
      <p>
        Last event received:
        {#if health.last_event_ms === null}
          <strong>never</strong>
        {:else}
          <strong>{formatAgo(health.last_event_ms)}</strong>
          <span class="muted">({formatWhen(health.last_event_ms)})</span>
        {/if}
      </p>
      <ul class="stats">
        <li>{health.events_stored} events stored in total</li>
        <li>{health.ingest.events_ingested} ingested since launch</li>
        <li class={health.ingest.ingest_failures > 0 ? "bad" : ""}>
          {health.ingest.ingest_failures} ingest failures since launch
          {#if health.ingest.ingest_failures > 0}
            (events arrived but could not be stored, often a Claude Code schema change; please
            report this)
          {/if}
        </li>
        <li class="muted">
          {health.ingest.events_skipped} unrelated telemetry records skipped (normal)
        </li>
      </ul>
    </section>

    <section class="card">
      <h2>Configuration</h2>
      {#if health.config.state === "installed"}
        <p class="good">Installed: Claude Code is configured to send usage events here.</p>
      {:else if health.config.state === "missing"}
        <p class="bad">
          Missing: the telemetry configuration is not (fully) present, so no events will arrive.
          <a href={resolve("/")}>Run setup</a> to install it.
        </p>
      {:else if health.config.state === "conflicting"}
        <p class={health.config.installed ? "warn" : "bad"}>
          {health.config.installed
            ? "Installed, but other telemetry settings coexist with this app's configuration:"
            : "Conflicting: pre-existing telemetry settings were found and setup is not complete:"}
        </p>
        <ul>
          {#each health.config.conflicts as conflict (conflict.key)}
            <li><code>{conflict.key}</code> = <code>{JSON.stringify(conflict.existing)}</code></li>
          {/each}
        </ul>
        {#if !health.config.installed}
          <p><a href={resolve("/")}>Run setup</a> to review and resolve them.</p>
        {/if}
      {:else}
        <p class="bad">Cannot read settings: {health.config.message}</p>
      {/if}
      <p class="muted"><code>{health.settings_path}</code></p>
    </section>

    <section class="card">
      <h2>Backfill</h2>
      {#if health.backfill.state === "not_available"}
        <p class="muted">
          Transcript backfill (importing history from before this app was installed) is not
          available yet; it ships in a later release.
        </p>
      {/if}
    </section>
  {/if}

  <div class="row">
    <button onclick={() => void refresh()}>Refresh</button>
    <a class="button-link" href={resolve("/")}>Back to setup</a>
    <a class="button-link" href={resolve("/settings")}>Settings</a>
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
    margin: 0 0 0.5rem;
  }

  .card {
    border: 1px solid rgba(0, 0, 0, 0.12);
    border-radius: 10px;
    padding: 1rem 1.25rem;
    margin-bottom: 1rem;
  }

  .card p {
    margin: 0.35rem 0;
  }

  .warn-box {
    border: 1px solid #d4a72c;
    background-color: rgba(212, 167, 44, 0.08);
    border-radius: 10px;
    padding: 1rem 1.25rem;
    margin-bottom: 1rem;
  }

  .warn-box p,
  .warn-box ul {
    margin: 0.35rem 0;
  }

  .stats {
    margin: 0.35rem 0;
    padding-left: 1.25rem;
  }

  .muted {
    color: #6b6b6b;
  }

  .good {
    color: #1a7f37;
  }

  .warn {
    color: #9a6700;
  }

  .bad {
    color: #b42318;
  }

  code {
    font-size: 0.85em;
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

  button:hover,
  .button-link:hover {
    border-color: #396cd8;
  }

  @media (prefers-color-scheme: dark) {
    .card {
      border-color: rgba(255, 255, 255, 0.15);
    }

    .muted {
      color: #a8a8a8;
    }

    .good {
      color: #7ee787;
    }

    .warn {
      color: #d4a72c;
    }

    .bad {
      color: #ffa198;
    }

    button,
    .button-link {
      color: #ffffff;
      background-color: #0f0f0f98;
      border-color: rgba(255, 255, 255, 0.2);
    }
  }
</style>
