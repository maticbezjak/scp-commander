<script>
  import { invoke, listen, emit, humanSize, humanRate, fmtEta, updateProgress } from "./api.js";

  // This window builds its own copy of the queue from the global "xfer" events
  // (the same ones the main window listens to), seeded once on open with a
  // snapshot of anything already in flight.
  let queue = $state([]);

  function apply(p) {
    const key = (x) => x.session === p.session && x.id === p.id;
    const t = queue.find(key);
    switch (p.event) {
      case "started":
        if (!t)
          queue.push({
            id: p.id, session: p.session, name: p.name, upload: p.upload,
            done: 0, total: p.total, state: "active",
            local: p.local, remote: p.remote, is_dir: p.is_dir, overwrite: p.overwrite,
            speed: 0, eta: null, lastAt: null, lastDone: 0,
          });
        break;
      case "progress":
        if (t) updateProgress(t, p.done, p.total);
        break;
      case "done":
        if (t) { t.state = "done"; t.done = t.total || t.done; }
        break;
      case "failed":
        if (t) { t.state = "failed"; t.error = p.message; }
        break;
      case "cancelled":
        if (t) t.state = "cancelled";
        break;
    }
  }

  $effect(() => {
    let unXfer, unSnap;
    (async () => {
      unXfer = await listen("xfer", (e) => apply(e.payload));
      unSnap = await listen("xfer-snapshot", (e) => {
        // Seed pre-existing items we missed (merge, don't clobber live ones).
        for (const it of e.payload) {
          if (!queue.find((x) => x.session === it.session && x.id === it.id)) queue.push({ ...it });
        }
      });
      emit("request-xfer-snapshot");
    })();
    return () => { unXfer?.(); unSnap?.(); };
  });

  let active = $derived(queue.filter((t) => t.state === "active"));
  let agg = $derived.by(() => {
    const done = active.reduce((s, t) => s + t.done, 0);
    const total = active.reduce((s, t) => s + t.total, 0);
    const rate = active.reduce((s, t) => s + (t.speed || 0), 0);
    return { count: active.length, pct: total > 0 ? Math.round((done / total) * 100) : 0, rate };
  });

  function pctOf(t) {
    return t.total > 0 ? Math.min(100, Math.round((t.done / t.total) * 100)) : 0;
  }
  function statusText(t) {
    if (t.state === "done") return "done";
    if (t.state === "failed") return `failed: ${t.error ?? ""}`;
    if (t.state === "cancelled") return "cancelled";
    const size = `${humanSize(t.done)}${t.total ? " / " + humanSize(t.total) : ""}`;
    const rate = t.speed ? ` · ${humanRate(t.speed)}` : "";
    const eta = t.eta ? ` · ${fmtEta(t.eta)} left` : "";
    return `${size}${rate}${eta}`;
  }
  function cancel(t) {
    invoke("cancel_transfer", { sessionId: t.session, id: t.id });
  }
  function remove(t) {
    queue = queue.filter((x) => !(x.session === t.session && x.id === t.id));
  }
  function retry(t) {
    invoke("enqueue", {
      sessionId: t.session, upload: t.upload, isDir: t.is_dir,
      name: t.name, local: t.local, remote: t.remote, overwrite: t.overwrite ?? 0,
      resume: true,
    });
    remove(t);
  }
  function clearFinished() {
    queue = queue.filter((t) => t.state === "active");
  }
</script>

<div class="win">
  <header>
    <strong>Transfers</strong>
    {#if agg.count}
      <span class="agg">{agg.count} active · {agg.pct}%{#if agg.rate} · {humanRate(agg.rate)}{/if}</span>
      <progress max="100" value={agg.pct}></progress>
    {:else}
      <span class="agg muted">idle</span>
    {/if}
    <button class="clear" onclick={clearFinished} disabled={!queue.some((t) => t.state !== "active")}>
      Clear finished
    </button>
  </header>

  <div class="rows">
    {#if !queue.length}
      <div class="empty">No transfers yet.</div>
    {/if}
    {#each queue as t (t.session + ":" + t.id)}
      <div class="row {t.state}">
        <span class="dir">{t.upload ? "↑" : "↓"}</span>
        <span class="name" title={t.name}>{t.name}</span>
        <progress max="100" value={pctOf(t)}></progress>
        <span class="stat">{statusText(t)}</span>
        {#if t.state === "active"}
          <button class="x" onclick={() => cancel(t)} title="Cancel">✕</button>
        {:else if t.state === "failed"}
          <button class="x" onclick={() => retry(t)} title="Retry">↻</button>
        {:else}
          <button class="x" onclick={() => remove(t)} title="Remove">✕</button>
        {/if}
      </div>
    {/each}
  </div>
</div>

<style>
  .win {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg);
    color: var(--text);
  }
  header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
    background: var(--panel);
    font-size: 13px;
  }
  .agg { color: var(--text-2); font-size: 12px; }
  .agg.muted { opacity: 0.6; }
  header progress { width: 140px; }
  .clear { margin-left: auto; font-size: 12px; }
  .rows { flex: 1; overflow: auto; padding: 6px 12px; }
  .empty { opacity: 0.5; font-size: 13px; padding: 20px; text-align: center; }
  .row {
    display: grid;
    grid-template-columns: 16px 1fr 150px auto 22px;
    gap: 10px;
    align-items: center;
    font-size: 12px;
    padding: 4px 0;
    border-bottom: 1px solid color-mix(in srgb, var(--border) 55%, transparent);
  }
  .dir { color: var(--text-3); }
  .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .stat { text-align: right; color: var(--text-2); white-space: nowrap; }
  .row.failed .stat { color: var(--danger); }
  .row.done .stat { color: var(--ok); }
  .x { font-size: 11px; padding: 0 5px; min-width: 22px; }
  button.x { border: none; background: transparent; color: var(--text-3); cursor: pointer; }
  button.x:hover { color: var(--danger); }
</style>
