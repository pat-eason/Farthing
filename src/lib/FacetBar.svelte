<script lang="ts">
  // Global facet controls (task 5.1): bound straight to the shared facet
  // state, so every analysis view sees the same selection and navigation
  // never resets it. Project/model are free-text filters until task 5.2
  // provides the real option lists.
  import {
    activeFacetCount,
    clearFacets,
    facets,
    QUERY_SOURCE_LABELS,
    QUERY_SOURCES,
    RANGE_LABELS,
    RANGE_PRESETS,
  } from "$lib/facets.svelte";
</script>

<div class="facet-bar" role="group" aria-label="Global facets">
  <label>
    <span class="facet-label">Range</span>
    <select bind:value={facets.range}>
      {#each RANGE_PRESETS as preset (preset)}
        <option value={preset}>{RANGE_LABELS[preset]}</option>
      {/each}
    </select>
  </label>

  <label>
    <span class="facet-label">Source</span>
    <select bind:value={facets.querySource}>
      {#each QUERY_SOURCES as source (source)}
        <option value={source}>{QUERY_SOURCE_LABELS[source]}</option>
      {/each}
    </select>
  </label>

  <label>
    <span class="facet-label">Project</span>
    <input type="text" placeholder="All projects" bind:value={facets.project} />
  </label>

  <label>
    <span class="facet-label">Model</span>
    <input type="text" placeholder="All models" bind:value={facets.model} />
  </label>

  {#if activeFacetCount() > 0}
    <button type="button" class="clear" onclick={clearFacets}>
      Clear ({activeFacetCount()})
    </button>
  {/if}
</div>

<style>
  .facet-bar {
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    gap: 0.75rem;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
  }

  .facet-label {
    font-size: 0.68rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: #6b6b6b;
  }

  select,
  input {
    font: inherit;
    font-size: 0.82rem;
    color: #1d1d1f;
    background: #ffffff;
    border: 1px solid rgba(0, 0, 0, 0.18);
    border-radius: 6px;
    padding: 0.3rem 0.45rem;
  }

  input {
    width: 11rem;
  }

  .clear {
    appearance: none;
    font: inherit;
    font-size: 0.78rem;
    font-weight: 500;
    color: #0a84ff;
    background: transparent;
    border: none;
    padding: 0.35rem 0.2rem;
    cursor: pointer;
  }

  @media (prefers-color-scheme: dark) {
    .facet-label {
      color: #9b9b9f;
    }

    select,
    input {
      color: #f5f5f7;
      background: #3a3a3c;
      border-color: rgba(255, 255, 255, 0.22);
    }

    .clear {
      color: #409cff;
    }
  }
</style>
