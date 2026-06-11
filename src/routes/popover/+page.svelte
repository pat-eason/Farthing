<script lang="ts">
  // Popover shell (task 4.1). This window is created hidden at startup and
  // toggled by the tray icon; the Rust side handles positioning and
  // click-away dismissal. Task 4.2 fills this with today's metrics (cost,
  // token split, sessions, top projects) and 4.3 adds the sparkline.
</script>

<main class="popover">
  <header>
    <h1>Today</h1>
  </header>
  <p class="placeholder">
    Usage metrics land here next: today's cost, token split, session count, and top projects.
  </p>
</main>

<style>
  :global(html, body) {
    margin: 0;
    overflow: hidden;
    background: transparent;
  }

  .popover {
    box-sizing: border-box;
    height: 100vh;
    padding: 0.9rem 1rem;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    font-size: 0.85rem;
    color: #1d1d1f;
    background: #f6f6f7;
    user-select: none;
    cursor: default;
  }

  header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    border-bottom: 1px solid rgba(0, 0, 0, 0.12);
    padding-bottom: 0.5rem;
  }

  h1 {
    margin: 0;
    font-size: 0.95rem;
    font-weight: 600;
  }

  .placeholder {
    color: #6b6b6b;
    line-height: 1.4;
  }

  @media (prefers-color-scheme: dark) {
    .popover {
      color: #f5f5f7;
      background: #28282a;
    }

    header {
      border-bottom-color: rgba(255, 255, 255, 0.16);
    }

    .placeholder {
      color: #9b9b9f;
    }
  }
</style>
