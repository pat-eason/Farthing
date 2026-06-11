<script lang="ts">
  // Global facet controls (task 5.1): bound straight to the shared facet
  // state, so every analysis view sees the same selection and navigation
  // never resets it. Task 5.3 added the custom date range and wired the
  // project/model inputs to the real option lists (task 5.2's
  // `facet_options`) via datalists: suggestions when the backend is
  // reachable, plain free-text otherwise.
  import {
    activeFacetCount,
    clearFacets,
    facets,
    localIsoDate,
    QUERY_SOURCE_LABELS,
    QUERY_SOURCES,
    RANGE_LABELS,
    RANGE_PRESETS,
    UNKNOWN_PROJECT_OPTION,
  } from "$lib/facets.svelte";
  import { getFacetOptions, type FacetOptions } from "$lib/queries";

  let options: FacetOptions | undefined = $state();

  $effect(() => {
    getFacetOptions().then(
      (loaded) => (options = loaded),
      // No options is fine: the inputs stay free-text (e.g. browser dev
      // without a backend); the views surface real query errors themselves.
      () => {}
    );
  });

  /** Seed the custom window with the trailing week the first time the user
   * switches to it, so the range never sits half-configured. */
  function onRangeChange() {
    if (facets.range !== "custom") return;
    if (facets.customStart === "" || facets.customEnd === "") {
      const today = new Date();
      const weekAgo = new Date(today.getFullYear(), today.getMonth(), today.getDate() - 6);
      facets.customStart = facets.customStart || localIsoDate(weekAgo);
      facets.customEnd = facets.customEnd || localIsoDate(today);
    }
  }
</script>

<div class="facet-bar" role="group" aria-label="Global facets">
  <label>
    <span class="facet-label">Range</span>
    <select bind:value={facets.range} onchange={onRangeChange}>
      {#each RANGE_PRESETS as preset (preset)}
        <option value={preset}>{RANGE_LABELS[preset]}</option>
      {/each}
      <option value="custom">{RANGE_LABELS.custom}…</option>
    </select>
  </label>

  {#if facets.range === "custom"}
    <label>
      <span class="facet-label">From</span>
      <input type="date" bind:value={facets.customStart} max={facets.customEnd || undefined} />
    </label>
    <label>
      <span class="facet-label">To</span>
      <input type="date" bind:value={facets.customEnd} min={facets.customStart || undefined} />
    </label>
  {/if}

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
    <input
      type="text"
      placeholder="All projects"
      bind:value={facets.project}
      list="facet-projects"
    />
    <datalist id="facet-projects">
      {#if options?.unknown_project}
        <option value={UNKNOWN_PROJECT_OPTION}></option>
      {/if}
      {#each options?.projects ?? [] as cwd (cwd)}
        <option value={cwd}></option>
      {/each}
    </datalist>
  </label>

  <label>
    <span class="facet-label">Model</span>
    <input type="text" placeholder="All models" bind:value={facets.model} list="facet-models" />
    <datalist id="facet-models">
      {#each options?.models ?? [] as model (model)}
        <option value={model}></option>
      {/each}
    </datalist>
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

  input[type="text"] {
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

    input[type="date"] {
      color-scheme: dark;
    }

    .clear {
      color: #409cff;
    }
  }
</style>
