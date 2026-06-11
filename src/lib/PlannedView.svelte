<script lang="ts">
  // Placeholder body for the four analysis views (task 5.1 ships the shell;
  // tasks 5.3-5.6 replace these with real charts/tables). Echoes the shared
  // facet selection so facet preservation across navigation is visible.
  import { facets, QUERY_SOURCE_LABELS, RANGE_LABELS } from "$lib/facets.svelte";

  let { title, description, task }: { title: string; description: string; task: string } = $props();
</script>

<section class="view">
  <h1>{title}</h1>
  <p class="muted">{description}</p>

  <dl class="facet-summary" aria-label="Active facets">
    <div>
      <dt>Range</dt>
      <dd>{RANGE_LABELS[facets.range]}</dd>
    </div>
    <div>
      <dt>Source</dt>
      <dd>{QUERY_SOURCE_LABELS[facets.querySource]}</dd>
    </div>
    <div>
      <dt>Project</dt>
      <dd>{facets.project.trim() === "" ? "All projects" : facets.project}</dd>
    </div>
    <div>
      <dt>Model</dt>
      <dd>{facets.model.trim() === "" ? "All models" : facets.model}</dd>
    </div>
  </dl>

  <p class="placeholder">
    This view lands in task {task}. The facets above already apply globally.
  </p>
</section>

<style>
  .view {
    max-width: 60rem;
  }

  h1 {
    margin: 0 0 0.35rem;
    font-size: 1.25rem;
    font-weight: 650;
  }

  .muted {
    margin: 0;
    color: #6b6b6b;
  }

  .facet-summary {
    display: flex;
    flex-wrap: wrap;
    gap: 0.6rem 2rem;
    margin: 1.25rem 0 0;
    padding: 0.85rem 1rem;
    border: 1px solid rgba(0, 0, 0, 0.1);
    border-radius: 10px;
    background: rgba(0, 0, 0, 0.03);
  }

  .facet-summary div {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
  }

  dt {
    font-size: 0.68rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: #6b6b6b;
  }

  dd {
    margin: 0;
    font-size: 0.9rem;
    font-weight: 500;
  }

  .placeholder {
    margin-top: 1.25rem;
    font-size: 0.85rem;
    color: #6b6b6b;
  }

  @media (prefers-color-scheme: dark) {
    .muted,
    .placeholder,
    dt {
      color: #9b9b9f;
    }

    .facet-summary {
      border-color: rgba(255, 255, 255, 0.14);
      background: rgba(255, 255, 255, 0.05);
    }
  }
</style>
