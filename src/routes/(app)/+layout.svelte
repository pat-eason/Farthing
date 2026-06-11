<script lang="ts">
  // Desktop window shell (task 5.1): persistent sidebar navigation around
  // the four analysis views plus health/settings. The window itself is
  // managed by the backend (tray.rs): opened from the tray menu or the
  // popover's "Open app" button, hidden (never destroyed) on close, with the
  // macOS activation policy flipping Regular/Accessory. Because this is an
  // SPA and the window survives close, both the route and the shared facet
  // state (facets.svelte.ts) persist across navigation and close/reopen.
  import { resolve } from "$app/paths";
  import { page } from "$app/state";
  import FacetBar from "$lib/FacetBar.svelte";

  let { children } = $props();

  const views = [
    { route: "/(app)/cost", label: "Cost over time" },
    { route: "/(app)/sessions", label: "Sessions" },
    { route: "/(app)/tokens", label: "Tokens & cache" },
    { route: "/(app)/projects", label: "Projects" },
  ] as const;
  const secondary = [
    { route: "/(app)/health", label: "Health" },
    { route: "/(app)/settings", label: "Settings" },
  ] as const;

  function isActive(route: (typeof views | typeof secondary)[number]["route"]): boolean {
    return page.url.pathname === resolve(route);
  }

  // The facet bar only frames the analysis views; health/settings are
  // operational pages the facets don't apply to.
  const showFacets = $derived(views.some((view) => isActive(view.route)));
</script>

<div class="shell">
  <nav class="sidebar" aria-label="Views">
    <p class="app-name">Claude Usage Tracker</p>
    <ul>
      {#each views as view (view.route)}
        <li>
          <a href={resolve(view.route)} aria-current={isActive(view.route) ? "page" : undefined}>
            {view.label}
          </a>
        </li>
      {/each}
    </ul>
    <ul class="secondary">
      {#each secondary as item (item.route)}
        <li>
          <a href={resolve(item.route)} aria-current={isActive(item.route) ? "page" : undefined}>
            {item.label}
          </a>
        </li>
      {/each}
    </ul>
  </nav>

  <div class="content">
    {#if showFacets}
      <header class="facet-header">
        <FacetBar />
      </header>
    {/if}
    <main class="page">
      {@render children()}
    </main>
  </div>
</div>

<style>
  :global(html, body) {
    margin: 0;
    height: 100%;
  }

  .shell {
    display: flex;
    height: 100vh;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    font-size: 0.9rem;
    color: #1d1d1f;
    background: #f6f6f7;
  }

  .sidebar {
    flex: 0 0 13rem;
    box-sizing: border-box;
    padding: 1rem 0.75rem;
    border-right: 1px solid rgba(0, 0, 0, 0.1);
    background: #ededf0;
    overflow-y: auto;
    user-select: none;
  }

  .app-name {
    margin: 0 0 0.9rem;
    padding: 0 0.5rem;
    font-size: 0.72rem;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: #6b6b6b;
  }

  .sidebar ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .sidebar ul.secondary {
    margin-top: 1.1rem;
    padding-top: 1.1rem;
    border-top: 1px solid rgba(0, 0, 0, 0.1);
  }

  .sidebar a {
    display: block;
    padding: 0.4rem 0.55rem;
    border-radius: 7px;
    color: inherit;
    text-decoration: none;
  }

  .sidebar a:hover {
    background: rgba(0, 0, 0, 0.05);
  }

  .sidebar a[aria-current="page"] {
    background: #0a84ff;
    color: #ffffff;
    font-weight: 550;
  }

  .content {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .facet-header {
    flex: none;
    padding: 0.8rem 1.4rem;
    border-bottom: 1px solid rgba(0, 0, 0, 0.1);
    background: #fbfbfc;
  }

  .page {
    flex: 1;
    overflow-y: auto;
    padding: 1.4rem;
  }

  @media (prefers-color-scheme: dark) {
    .shell {
      color: #f5f5f7;
      background: #28282a;
    }

    .sidebar {
      background: #1f1f21;
      border-right-color: rgba(255, 255, 255, 0.12);
    }

    .app-name {
      color: #9b9b9f;
    }

    .sidebar ul.secondary {
      border-top-color: rgba(255, 255, 255, 0.12);
    }

    .sidebar a:hover {
      background: rgba(255, 255, 255, 0.08);
    }

    .sidebar a[aria-current="page"] {
      background: #409cff;
    }

    .facet-header {
      background: #2e2e30;
      border-bottom-color: rgba(255, 255, 255, 0.12);
    }
  }
</style>
