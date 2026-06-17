<script>
  // items: [{ label, action, danger? }]
  let { x = 0, y = 0, items = [], onClose } = $props();
</script>

<svelte:window onclick={onClose} oncontextmenu={onClose} onkeydown={(e) => e.key === "Escape" && onClose()} />
<div class="ctx" style="left:{x}px; top:{y}px" role="menu">
  {#each items as it}
    <button
      class:danger={it.danger}
      onclick={(e) => {
        e.stopPropagation();
        onClose();
        it.action();
      }}
    >
      {it.label}
    </button>
  {/each}
</div>

<style>
  .ctx {
    position: fixed;
    z-index: 60;
    min-width: 160px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 4px;
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.3);
  }
  .ctx button {
    display: block;
    width: 100%;
    text-align: left;
    border: none;
    background: none;
    padding: 5px 10px;
    font-size: 13px;
    border-radius: 4px;
  }
  .ctx button:hover {
    background: color-mix(in srgb, var(--accent, dodgerblue) 30%, transparent);
  }
  .ctx button.danger {
    color: tomato;
  }
</style>
