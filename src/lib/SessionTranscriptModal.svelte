<script lang="ts">
  import { getSessionTranscript, type SessionTranscript } from "$lib/queries";
  import { renderMarkdown } from "$lib/markdown";
  import { onMount } from "svelte";

  let { sessionId, onclose }: { sessionId: string; onclose: () => void } = $props();

  let transcript: SessionTranscript | null = $state(null);
  let error: string | null = $state(null);
  let loading = $state(true);

  onMount(() => {
    getSessionTranscript(sessionId).then(
      (t) => {
        transcript = t;
        loading = false;
      },
      (e) => {
        error = String(e);
        loading = false;
      }
    );
  });

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }

  function handleBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) onclose();
  }

  function formatTime(ms: number): string {
    return new Date(ms).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  }

  function formatCost(usd: number | null): string {
    if (usd === null) return "unpriced";
    return "$" + usd.toFixed(4);
  }

  function toJson(v: unknown): string {
    return JSON.stringify(v, null, 2);
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="backdrop" role="presentation" onclick={handleBackdropClick}>
  <div class="modal" role="dialog" aria-modal="true" aria-label="Session transcript">
    <button type="button" class="close-btn" onclick={onclose} aria-label="Close transcript"
      >×</button
    >
    <h2 class="modal-title">Session transcript</h2>

    {#if loading}
      <p class="muted">Loading transcript…</p>
    {:else if error}
      <p class="err">{error}</p>
    {:else if !transcript || transcript.chunks.length === 0}
      <p class="muted">No transcript available for this session.</p>
    {:else}
      <div class="chunks">
        {#each transcript.chunks as chunk (chunk.request_id)}
          <div class="chunk">
            <div class="chunk-header">
              <span class="chunk-meta">
                {#if chunk.model}<span class="chunk-model">{chunk.model}</span>{/if}
                <span class="muted chunk-time">{formatTime(chunk.timestamp_ms)}</span>
              </span>
              <span class="chunk-cost" class:unpriced={chunk.cost_usd === null}>
                {formatCost(chunk.cost_usd)}
              </span>
            </div>

            <div class="turns">
              {#each chunk.turns as turn (turn.timestamp_ms + turn.role)}
                <div
                  class="turn"
                  class:turn-user={turn.role === "user"}
                  class:turn-assistant={turn.role === "assistant"}
                >
                  <div class="turn-meta">
                    <span class="turn-role">{turn.role === "user" ? "User" : "Assistant"}</span>
                    <span class="muted turn-time">{formatTime(turn.timestamp_ms)}</span>
                  </div>
                  <div class="turn-blocks">
                    {#each turn.blocks as block, bi (bi)}
                      {#if block.kind === "text"}
                        {#await renderMarkdown(block.text)}
                          <p class="muted">…</p>
                        {:then html}
                          <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized by DOMPurify in renderMarkdown -->
                          {@html html}
                        {/await}
                      {:else if block.kind === "thinking"}
                        <details class="thinking">
                          <summary>Thinking</summary>
                          {#await renderMarkdown(block.thinking)}
                            <p class="muted">…</p>
                          {:then html}
                            <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized by DOMPurify in renderMarkdown -->
                            {@html html}
                          {/await}
                        </details>
                      {:else if block.kind === "tool_use"}
                        <div class="tool-use-block">
                          <span class="chip tool-chip">{block.name}</span>
                          <pre class="code-block"><code>{toJson(block.input)}</code></pre>
                        </div>
                      {:else if block.kind === "tool_result"}
                        <details class="tool-result" class:tool-result-error={block.is_error}>
                          <summary class:err-summary={block.is_error}
                            >Tool result{block.is_error ? " (error)" : ""}</summary
                          >
                          {#if typeof block.content === "string"}
                            {#await renderMarkdown(block.content)}
                              <p class="muted">…</p>
                            {:then html}
                              <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized by DOMPurify in renderMarkdown -->
                              {@html html}
                            {/await}
                          {:else}
                            <pre class="code-block"><code>{toJson(block.content)}</code></pre>
                          {/if}
                        </details>
                      {:else}
                        <pre class="code-block"><code>{toJson(block)}</code></pre>
                      {/if}
                    {/each}
                  </div>
                </div>
              {/each}
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  /* Shiki dual-theme CSS variables */
  :global(.shiki),
  :global(.shiki span) {
    color: var(--shiki-light);
    background-color: var(--shiki-light-bg);
  }

  @media (prefers-color-scheme: dark) {
    :global(.shiki),
    :global(.shiki span) {
      color: var(--shiki-dark);
      background-color: var(--shiki-dark-bg);
    }
  }

  :global(.shiki) {
    padding: 0.5em 0.75em;
    border-radius: 6px;
    font-size: 0.78em;
    overflow-x: auto;
  }

  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 1000;
    background: rgba(0, 0, 0, 0.45);
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding: 2rem 1rem;
    overflow-y: auto;
  }

  .modal {
    background: #fff;
    border-radius: 12px;
    max-width: 780px;
    width: 100%;
    padding: 1.25rem 1.5rem 1.5rem;
    position: relative;
    box-shadow: 0 4px 24px rgba(0, 0, 0, 0.18);
  }

  .close-btn {
    position: absolute;
    top: 0.8rem;
    right: 0.8rem;
    appearance: none;
    border: none;
    background: none;
    padding: 0.1rem 0.3rem;
    margin: 0;
    font-size: 1.4rem;
    line-height: 1;
    cursor: pointer;
    color: #6b6b6b;
  }

  .close-btn:hover {
    color: #1c1c1e;
  }

  .modal-title {
    margin: 0 2rem 0.9rem 0;
    font-size: 0.9rem;
    font-weight: 650;
  }

  .chunks {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .chunk {
    border: 1px solid rgba(0, 0, 0, 0.1);
    border-radius: 8px;
    overflow: hidden;
  }

  .chunk-header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.75rem;
    padding: 0.45rem 0.75rem;
    background: rgba(0, 0, 0, 0.03);
    border-bottom: 1px solid rgba(0, 0, 0, 0.07);
  }

  .chunk-meta {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    min-width: 0;
  }

  .chunk-model {
    font-size: 0.72rem;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .chunk-time {
    font-size: 0.68rem;
    white-space: nowrap;
  }

  .chunk-cost {
    font-size: 0.75rem;
    font-weight: 650;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .chunk-cost.unpriced {
    font-weight: 400;
    color: #6b6b6b;
  }

  .turns {
    display: flex;
    flex-direction: column;
    gap: 0;
  }

  .turn {
    padding: 0.6rem 0.75rem;
    border-left: 3px solid transparent;
  }

  .turn + .turn {
    border-top: 1px solid rgba(0, 0, 0, 0.05);
  }

  .turn-user {
    border-left-color: rgba(0, 0, 0, 0.1);
    background: rgba(0, 0, 0, 0.015);
  }

  .turn-assistant {
    border-left-color: rgba(10, 132, 255, 0.35);
  }

  .turn-meta {
    display: flex;
    gap: 0.5rem;
    align-items: baseline;
    margin-bottom: 0.3rem;
  }

  .turn-role {
    font-weight: 600;
    font-size: 0.68rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .turn-time {
    font-size: 0.65rem;
  }

  .turn-blocks {
    font-size: 0.81rem;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .thinking {
    font-style: italic;
  }

  .thinking summary {
    cursor: pointer;
    color: #6b6b6b;
    font-size: 0.74rem;
  }

  .tool-use-block {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }

  .chip {
    display: inline-block;
    padding: 0.05rem 0.35rem;
    border-radius: 999px;
    font-size: 0.65rem;
    font-weight: 600;
    background: rgba(0, 0, 0, 0.07);
    color: #4b4b4d;
    vertical-align: 0.08rem;
  }

  .tool-chip {
    background: rgba(10, 132, 255, 0.12);
    color: #0a6ad1;
    font-style: normal;
  }

  .code-block {
    background: rgba(0, 0, 0, 0.04);
    border-radius: 6px;
    padding: 0.5em 0.75em;
    font-size: 0.76em;
    overflow-x: auto;
    white-space: pre-wrap;
    word-break: break-all;
    margin: 0;
  }

  .tool-result summary {
    cursor: pointer;
    font-size: 0.74rem;
    color: #6b6b6b;
  }

  .err-summary {
    color: #b42318 !important;
  }

  .tool-result-error {
    border-left: 2px solid rgba(180, 35, 24, 0.4);
    padding-left: 0.5rem;
  }

  .err {
    color: #b42318;
  }

  .muted {
    color: #6b6b6b;
  }

  @media (prefers-color-scheme: dark) {
    .backdrop {
      background: rgba(0, 0, 0, 0.6);
    }

    .modal {
      background: #1f1f21;
    }

    .close-btn {
      color: #9b9b9f;
    }

    .close-btn:hover {
      color: #f2f2f4;
    }

    .chunk {
      border-color: rgba(255, 255, 255, 0.1);
    }

    .chunk-header {
      background: rgba(255, 255, 255, 0.04);
      border-bottom-color: rgba(255, 255, 255, 0.08);
    }

    .chunk-cost.unpriced {
      color: #9b9b9f;
    }

    .turn + .turn {
      border-top-color: rgba(255, 255, 255, 0.06);
    }

    .turn-user {
      border-left-color: rgba(255, 255, 255, 0.12);
      background: rgba(255, 255, 255, 0.02);
    }

    .turn-assistant {
      border-left-color: rgba(64, 156, 255, 0.4);
    }

    .thinking summary {
      color: #9b9b9f;
    }

    .chip {
      background: rgba(255, 255, 255, 0.12);
      color: #cfcfd2;
    }

    .tool-chip {
      background: rgba(64, 156, 255, 0.2);
      color: #8cc1ff;
    }

    .code-block {
      background: rgba(255, 255, 255, 0.06);
    }

    .tool-result summary {
      color: #9b9b9f;
    }

    .err-summary {
      color: #ffa198 !important;
    }

    .err {
      color: #ffa198;
    }

    .muted {
      color: #9b9b9f;
    }
  }
</style>
