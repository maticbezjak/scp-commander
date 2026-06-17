<script>
  let { title, onClose, children } = $props();
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && onClose()} />
<div class="backdrop" onclick={onClose} role="presentation">
  <div class="modal" onclick={(e) => e.stopPropagation()} role="dialog">
    <div class="mtitle">{title}</div>
    {@render children()}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: grid;
    place-items: center;
    z-index: 50;
  }
  .modal {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 14px 16px;
    min-width: 320px;
    max-width: 70vw;
    box-shadow: 0 10px 40px rgba(0, 0, 0, 0.35);
  }
  .mtitle {
    font-weight: 600;
    margin-bottom: 10px;
  }
</style>
