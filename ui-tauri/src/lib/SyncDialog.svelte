<script>
  import { invoke, listen, humanSize } from "./api.js";
  import Modal from "./Modal.svelte";

  let { localPath, remotePath, onClose } = $props();

  let direction = $state("upload"); // "upload" = local→remote, "download" = remote→local
  let mirror = $state(false);
  let plan = $state(null); // { items, dirs, deletes }
  let running = $state(false);
  let busy = $state(false);
  let progress = $state({ done: 0, total: 0 });
  let result = $state(null); // { copied, skipped, bytes }
  let error = $state("");

  // Live sync progress from the backend worker.
  $effect(() => {
    const un = listen("sync", (e) => {
      const p = e.payload;
      if (p.event === "progress") progress = { done: p.done, total: p.total };
      else if (p.event === "done") { result = p; running = false; }
      else if (p.event === "failed") { error = p.message; running = false; }
    });
    return () => un.then((f) => f());
  });

  async function preview() {
    busy = true;
    error = "";
    result = null;
    try {
      plan = await invoke("sync_plan", {
        local: localPath,
        remote: remotePath,
        direction,
        mirror,
      });
    } catch (e) {
      error = String(e);
      plan = null;
    } finally {
      busy = false;
    }
  }

  async function run() {
    error = "";
    result = null;
    progress = { done: 0, total: 0 };
    running = true;
    try {
      await invoke("sync_run", {
        local: localPath,
        remote: remotePath,
        direction,
        mirror,
      });
    } catch (e) {
      error = String(e);
      running = false;
    }
  }

  const pct = $derived(progress.total ? Math.round((progress.done / progress.total) * 100) : 0);
</script>

<Modal title="Synchronize" {onClose}>
  <div class="row">
    <select bind:value={direction} disabled={running}>
      <option value="upload">Local → Remote (upload)</option>
      <option value="download">Remote → Local (download)</option>
    </select>
    <label class="chk"><input type="checkbox" bind:checked={mirror} disabled={running} /> Mirror (delete extras)</label>
  </div>
  <p class="paths">
    <span class="mono">{localPath}</span>
    {direction === "upload" ? "→" : "←"}
    <span class="mono">{remotePath}</span>
  </p>

  {#if error}<p class="err">{error}</p>{/if}

  {#if plan}
    <div class="plan">
      <div class="plan-head">
        {plan.items.length} to copy · {plan.dirs.length} new folders
        {#if plan.deletes.length}· {plan.deletes.length} to delete{/if}
      </div>
      <ul>
        {#each plan.items as it}
          <li><span class="mono">{it.rel}</span><span class="reason">{it.reason}</span><span class="sz">{humanSize(it.size)}</span></li>
        {/each}
        {#each plan.deletes as d}
          <li class="del"><span class="mono">{d}</span><span class="reason">delete</span><span class="sz"></span></li>
        {/each}
      </ul>
      {#if !plan.items.length && !plan.deletes.length}<div class="empty">Already in sync.</div>{/if}
    </div>
  {/if}

  {#if running}
    <div class="prog"><div class="bar" style="width:{pct}%"></div></div>
    <p class="muted">{humanSize(progress.done)} / {humanSize(progress.total)} ({pct}%)</p>
  {/if}

  {#if result}
    <p class="ok">Done — copied {result.copied}, skipped {result.skipped}, {humanSize(result.bytes)} transferred.</p>
  {/if}

  <div class="dlg-actions">
    <button onclick={onClose}>Close</button>
    <button onclick={preview} disabled={busy || running}>{busy ? "Scanning…" : "Preview"}</button>
    <button class="primary" onclick={run} disabled={running}>Synchronize</button>
  </div>
</Modal>

<style>
  .row { display: flex; gap: 12px; align-items: center; margin-bottom: 8px; }
  .chk { display: flex; align-items: center; gap: 4px; font-size: 13px; }
  .paths { font-size: 12px; opacity: 0.8; margin: 0 0 10px; }
  .mono { font-family: ui-monospace, monospace; }
  .plan { border: 1px solid var(--border); border-radius: 6px; margin-bottom: 10px; }
  .plan-head { padding: 5px 8px; font-size: 12px; border-bottom: 1px solid var(--border); opacity: 0.8; }
  .plan ul { list-style: none; margin: 0; padding: 0; max-height: 200px; overflow: auto; font-size: 12px; }
  .plan li { display: flex; gap: 8px; padding: 2px 8px; }
  .plan li .mono { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .plan li.del { color: tomato; }
  .reason { opacity: 0.6; }
  .sz { width: 70px; text-align: right; font-variant-numeric: tabular-nums; }
  .empty { padding: 8px; opacity: 0.6; font-size: 13px; }
  .prog { height: 6px; background: var(--border); border-radius: 3px; overflow: hidden; margin-bottom: 4px; }
  .bar { height: 100%; background: var(--accent, dodgerblue); transition: width 0.15s; }
  .muted { font-size: 12px; opacity: 0.7; margin: 0 0 8px; }
  .err { color: tomato; font-size: 13px; margin: 0 0 8px; }
  .ok { color: seagreen; font-size: 13px; margin: 0 0 8px; }
  .dlg-actions { display: flex; justify-content: flex-end; gap: 8px; }
  .dlg-actions button { padding: 5px 12px; }
  button.primary { border-color: var(--accent, dodgerblue); }
</style>
