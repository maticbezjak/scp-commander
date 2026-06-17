<script>
  import { humanSize } from "./api.js";

  let {
    title,
    path = "/",
    entries = [],
    busy = false,
    selected = [],
    transferLabel = "",
    canTransfer = false,
    onUp,
    onNavigate,
    onOpen, // folder double-click
    onTransferOne, // file double-click
    onTransfer, // toolbar button (selection)
    onRowClick, // (entry, index, event)
    onContext, // (entry, event) right-click
    onNewFolder,
    onRefresh,
    showHidden = true,
  } = $props();

  let pathInput = $state("");
  $effect(() => {
    pathInput = path; // mirror external navigation into the editable field
  });

  // Folders first, then case-insensitive by name. Hidden dotfiles are dropped
  // unless the show-hidden preference is on.
  let sorted = $derived(
    [...entries]
      .filter((e) => showHidden || !e.name.startsWith("."))
      .sort((a, b) => Number(b.is_dir) - Number(a.is_dir) || a.name.localeCompare(b.name)),
  );

  function rowClass(e) {
    return e.is_symlink ? "link" : e.is_dir ? "dir" : "file";
  }
  function dbl(e) {
    if (e.is_dir) onOpen(e);
    else onTransferOne(e);
  }
</script>

<div class="pane">
  <div class="pane-head">
    <span class="pane-title">{title}</span>
    <button onclick={onUp} title="Parent directory">⬆</button>
    <button onclick={onRefresh} title="Refresh">⟳</button>
    <button onclick={onNewFolder} title="New folder">＋</button>
    <input
      class="pathbar"
      bind:value={pathInput}
      onkeydown={(e) => e.key === "Enter" && onNavigate(pathInput)}
    />
    {#if transferLabel}
      <button class="xfer" disabled={!canTransfer || selected.length === 0} onclick={onTransfer}>
        {transferLabel}
      </button>
    {/if}
  </div>

  <div class="rows" class:busy>
    <table>
      <thead>
        <tr><th class="name">Name</th><th class="size">Size</th></tr>
      </thead>
      <tbody>
        <tr class="dir"><td class="name" ondblclick={onUp}>..</td><td></td></tr>
        {#each sorted as e, i (e.name)}
          <tr
            class={rowClass(e)}
            class:sel={selected.includes(e.name)}
            onclick={(ev) => onRowClick(e, i, ev)}
            ondblclick={() => dbl(e)}
            oncontextmenu={(ev) => (ev.preventDefault(), onContext(e, i, ev))}
          >
            <td class="name">{e.name}</td>
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
  .xfer {
    font-size: 12px;
    white-space: nowrap;
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
    user-select: none;
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
  tbody tr {
    cursor: default;
  }
  tbody tr.sel {
    background: color-mix(in srgb, var(--accent, dodgerblue) 30%, transparent);
  }
  tr.dir td.name {
    font-weight: 600;
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
