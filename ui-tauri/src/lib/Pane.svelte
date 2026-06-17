<script>
  import { humanSize } from "./api.js";

  let { title, path = "/", entries = [], busy = false, onOpen, onUp, onNavigate } =
    $props();

  let pathInput = $state(path);
  $effect(() => {
    pathInput = path; // reflect external navigation
  });

  // Folders first, then case-insensitive by name.
  let sorted = $derived(
    [...entries].sort(
      (a, b) => Number(b.is_dir) - Number(a.is_dir) || a.name.localeCompare(b.name),
    ),
  );

  function rowClass(e) {
    return e.is_symlink ? "link" : e.is_dir ? "dir" : "file";
  }
</script>

<div class="pane">
  <div class="pane-head">
    <span class="pane-title">{title}</span>
    <button onclick={onUp} title="Parent directory">⬆</button>
    <input
      class="pathbar"
      bind:value={pathInput}
      onkeydown={(e) => e.key === "Enter" && onNavigate(pathInput)}
    />
  </div>

  <div class="rows" class:busy>
    <table>
      <thead>
        <tr><th class="name">Name</th><th class="size">Size</th></tr>
      </thead>
      <tbody>
        <tr class="dir">
          <td class="name" onclick={onUp}>..</td>
          <td></td>
        </tr>
        {#each sorted as e (e.name)}
          <tr class={rowClass(e)}>
            <td
              class="name"
              ondblclick={() => e.is_dir && onOpen(e)}
              onclick={() => e.is_dir && onOpen(e)}
            >
              {e.name}
            </td>
            <td class="size">{e.is_dir ? "" : humanSize(e.size)}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
</div>

<style>
  .pane {
    display: flex;
    flex-direction: column;
    flex: 1 1 0;
    min-width: 0;
    border: 1px solid var(--border);
    border-radius: 6px;
    overflow: hidden;
    background: var(--panel);
  }
  .pane-head {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 5px 6px;
    border-bottom: 1px solid var(--border);
  }
  .pane-title {
    font-weight: 600;
    font-size: 12px;
    opacity: 0.75;
  }
  .pathbar {
    flex: 1;
    font-family: ui-monospace, monospace;
    font-size: 12px;
    padding: 3px 6px;
  }
  .rows {
    overflow: auto;
    flex: 1;
  }
  .rows.busy {
    opacity: 0.5;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
  }
  th,
  td {
    text-align: left;
    padding: 2px 8px;
    border-bottom: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  th.size,
  td.size {
    text-align: right;
    width: 90px;
    font-variant-numeric: tabular-nums;
  }
  td.name {
    max-width: 0;
    width: 100%;
  }
  tr.dir td.name {
    font-weight: 600;
    cursor: pointer;
  }
  tr.dir td.name::before {
    content: "📁 ";
  }
  tr.file td.name::before {
    content: "📄 ";
  }
  tr.link td.name::before {
    content: "🔗 ";
  }
</style>
