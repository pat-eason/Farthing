<script lang="ts">
  // App-level export banner (Unit 4): the single, non-modal, view-agnostic
  // surface that reports export progress and completion (R10/R11/R12/R13/R18).
  // Mounted once in (app)/+layout.svelte so it persists across view navigation
  // during a long export. Reads the shared `exportState`; the orchestrator and
  // the progress listener own all transitions.
  //
  // Non-modal: no focus trap, no backdrop. The buttons are native <button>s so
  // they're keyboard-focusable and respond to Enter/Space for free. The
  // progress bar is determinate (rows streamed / total) — never a fake
  // animation (R11).
  import {
    exportProgressFraction,
    exportState,
    resetExport,
    revealSavedExport,
  } from "$lib/export.svelte";

  // Idle renders nothing; the banner only exists while an export is active or
  // just finished/failed/guarded.
  const visible = $derived(exportState.status !== "idle");
  const fraction = $derived(exportProgressFraction());
  const percent = $derived(Math.round(fraction * 100));
</script>

{#if visible}
  <div
    class="export-banner"
    class:is-error={exportState.status === "error"}
    class:is-done={exportState.status === "done"}
    class:is-guarded={exportState.status === "guarded"}
    role="status"
    aria-live="polite"
  >
    <div class="body">
      {#if exportState.status === "preparing"}
        <span class="label">Preparing export…</span>
      {:else if exportState.status === "working"}
        <span class="label">
          Exporting… {exportState.rowsWritten.toLocaleString()}
          {#if exportState.totalRows > 0}/ {exportState.totalRows.toLocaleString()}{/if} rows
        </span>
        <div
          class="progress"
          role="progressbar"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={percent}
        >
          <div class="fill" style:width="{percent}%"></div>
        </div>
      {:else if exportState.status === "done"}
        <span class="label">{exportState.message}</span>
      {:else}
        <!-- guarded / error -->
        <span class="label">{exportState.message}</span>
      {/if}
    </div>

    <div class="actions">
      {#if exportState.status === "done" && exportState.savedPath !== undefined}
        <button type="button" class="action" onclick={() => void revealSavedExport()}>
          Show in Finder
        </button>
      {/if}
      {#if exportState.status === "done" || exportState.status === "error"}
        <button type="button" class="dismiss" aria-label="Dismiss" onclick={() => resetExport()}>
          ✕
        </button>
      {/if}
    </div>
  </div>
{/if}

<style>
  .export-banner {
    position: sticky;
    top: 0;
    z-index: 20;
    display: flex;
    align-items: center;
    gap: 1rem;
    padding: 0.6rem 1.4rem;
    background: #0a84ff;
    color: #ffffff;
    font-size: 0.85rem;
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.18);
  }

  .export-banner.is-done {
    background: #248a3d;
  }

  .export-banner.is-guarded {
    background: #6b6b70;
  }

  .export-banner.is-error {
    background: #d70015;
  }

  .body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }

  .label {
    font-weight: 550;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .progress {
    height: 5px;
    border-radius: 3px;
    background: rgba(255, 255, 255, 0.3);
    overflow: hidden;
  }

  .fill {
    height: 100%;
    background: #ffffff;
    border-radius: 3px;
    transition: width 0.15s linear;
  }

  .actions {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }

  .action {
    padding: 0.32rem 0.7rem;
    border: 1px solid rgba(255, 255, 255, 0.7);
    border-radius: 6px;
    background: rgba(255, 255, 255, 0.16);
    color: inherit;
    font-size: 0.82rem;
    font-weight: 550;
    cursor: pointer;
  }

  .action:hover {
    background: rgba(255, 255, 255, 0.28);
  }

  .dismiss {
    width: 1.6rem;
    height: 1.6rem;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: inherit;
    font-size: 0.85rem;
    cursor: pointer;
  }

  .dismiss:hover {
    background: rgba(255, 255, 255, 0.2);
  }

  .action:focus-visible,
  .dismiss:focus-visible {
    outline: 2px solid #ffffff;
    outline-offset: 1px;
  }
</style>
