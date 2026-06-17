<script>
  import { invoke } from "./api.js";
  import Modal from "./Modal.svelte";

  let { onClose } = $props();

  let hosts = $state([]);
  let status = $state("");

  async function reload() {
    hosts = await invoke("known_hosts_list");
  }
  $effect(() => {
    reload();
  });

  async function remove(host) {
    try {
      const n = await invoke("known_hosts_remove", { host });
      status = `Removed ${n} entr${n === 1 ? "y" : "ies"} for ${host}`;
      await reload();
    } catch (e) {
      status = String(e);
    }
  }
</script>

<Modal title="Trusted host keys" {onClose}>
  <p class="muted">SCP Commander's own trusted-host store. Removing an entry makes the next connection re-prompt.</p>
  {#if hosts.length}
    <ul>
      {#each hosts as h}
        <li>
          <span class="mono">{h.host}</span>
          <span class="kt">{h.key_type}</span>
          <button class="danger" onclick={() => remove(h.host)}>Forget</button>
        </li>
      {/each}
    </ul>
  {:else}
    <div class="empty">No trusted hosts yet.</div>
  {/if}
  {#if status}<p class="status">{status}</p>{/if}
  <div class="dlg-actions"><button onclick={onClose}>Close</button></div>
</Modal>

<style>
  .muted { font-size: 12px; opacity: 0.7; margin: 0 0 10px; }
  ul { list-style: none; margin: 0 0 8px; padding: 0; max-height: 280px; overflow: auto; }
  li { display: flex; align-items: center; gap: 8px; padding: 4px 0; border-bottom: 1px solid color-mix(in srgb, var(--border) 60%, transparent); }
  .mono { flex: 1; font-family: ui-monospace, monospace; font-size: 13px; overflow: hidden; text-overflow: ellipsis; }
  .kt { font-size: 11px; opacity: 0.6; }
  .empty { opacity: 0.5; font-size: 13px; margin-bottom: 8px; }
  .status { font-size: 12px; opacity: 0.8; }
  button.danger { border-color: tomato; color: tomato; font-size: 12px; padding: 3px 8px; }
  .dlg-actions { display: flex; justify-content: flex-end; }
  .dlg-actions button { padding: 5px 12px; }
</style>
