<script lang="ts">
  // Minimal health view stub: the onboarding "done" screen links here so
  // users can confirm the receiver is up. The full diagnostics view
  // (config state, last-event-at, ingest failures) is task 2.5.
  import { resolve } from "$app/paths";
  import { invoke } from "@tauri-apps/api/core";

  interface ReceiverStatus {
    state: "starting" | "listening" | "port_in_use" | "failed";
    port?: number;
    message?: string;
  }

  let status: ReceiverStatus | undefined = $state();
  let errorMessage = $state("");

  async function refresh() {
    errorMessage = "";
    try {
      status = await invoke<ReceiverStatus>("receiver_status");
    } catch (err) {
      errorMessage = String(err);
    }
  }

  $effect(() => {
    void refresh();
  });
</script>

<main class="container">
  <h1>Health</h1>
  {#if errorMessage}
    <p class="bad">{errorMessage}</p>
  {:else if !status}
    <p class="muted">Checking receiver…</p>
  {:else if status.state === "listening"}
    <p class="good">Receiver listening on 127.0.0.1:{status.port}.</p>
    <p class="muted">
      New Claude Code sessions will send usage events here. Full diagnostics (last event received,
      configuration state) are coming soon.
    </p>
  {:else if status.state === "port_in_use"}
    <p class="bad">
      Port {status.port} is already in use by another process, so the receiver could not start. Quit whatever
      is using the port and relaunch this app.
    </p>
  {:else if status.state === "failed"}
    <p class="bad">Receiver failed: {status.message}</p>
  {:else}
    <p class="muted">Receiver is starting…</p>
  {/if}
  <div class="row">
    <button onclick={() => void refresh()}>Refresh</button>
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

  .muted {
    color: #6b6b6b;
  }

  .good {
    color: #1a7f37;
  }

  .bad {
    color: #b42318;
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
    .muted {
      color: #a8a8a8;
    }

    .good {
      color: #7ee787;
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
