<script>
  import { humanSize, humanRate, fmtEta } from "./api.js";

  let { queue = [], onCancel, onClear, onRetry, onRemove } = $props();

  let active = $derived(queue.filter((t) => t.state === "active"));
  let agg = $derived.by(() => {
    const done = active.reduce((s, t) => s + t.done, 0);
    const total = active.reduce((s, t) => s + t.total, 0);
    const rate = active.reduce((s, t) => s + (t.speed || 0), 0);
    const pct = total > 0 ? Math.round((done / total) * 100) : 0;
    return { count: active.length, done, total, pct, rate };
  });

  function pctOf(t) {
    return t.total > 0 ? Math.min(100, Math.round((t.done / t.total) * 100)) : 0;
  }
  function label(t) {
    const arrow = t.upload ? "↑" : "↓";
    if (t.state === "done") return "done";
    if (t.state === "failed") return `failed: ${t.error ?? ""}`;
    if (t.state === "cancelled") return "cancelled";
    const size = `${humanSize(t.done)}${t.total ? " / " + humanSize(t.total) : ""}`;
    const rate = t.speed ? ` · ${humanRate(t.speed)}` : "";
    const eta = t.eta ? ` · ${fmtEta(t.eta)} left` : "";
    return `${arrow} ${size}${rate}${eta}`;
  }
</script>

{#if queue.length}
  <div class="queue">
    <div class="qhead">
      <strong>Transfers</strong>
      {#if agg.count}
        <span class="agg">
          {agg.count} active · {humanSize(agg.done)} / {humanSize(agg.total)} · {agg.pct}%{#if agg.rate} · {humanRate(agg.rate)}{/if}
        </span>
        <progress class="aggbar" max="100" value={agg.pct}></progress>
      {/if}
      <button class="clear" onclick={onClear} disabled={!queue.some((t) => t.state !== "active")}>
        Clear finished
      </button>
    </div>
    <div class="qrows">
      {#each queue as t (t.id)}
        <div class="qrow {t.state}">
          <span class="qname" title={t.name}>{t.name}</span>
          <progress max="100" value={pctOf(t)}></progress>
          <span class="qstat">{label(t)}</span>
          <span class="qacts">
            {#if t.state === "active"}
              <button class="qcancel" onclick={() => onCancel(t)} title="Cancel">✕</button>
            {:else if t.state === "failed"}
              <button class="qcancel" onclick={() => onRetry(t)} title="Retry">↻</button>
              <button class="qcancel" onclick={() => onRemove(t)} title="Remove">✕</button>
            {:else}
              <button class="qcancel" onclick={() => onRemove(t)} title="Remove">✕</button>
            {/if}
          </span>
        </div>
      {/each}
    </div>
  </div>
{/if}

<style>
  .queue {
    border-top: 1px solid var(--border);
    background: var(--panel);
    max-height: 33%;
    display: flex;
    flex-direction: column;
  }
  .qhead {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 5px 10px;
    font-size: 12px;
  }
  .agg {
    opacity: 0.8;
  }
  .aggbar {
    width: 160px;
  }
  .clear {
    margin-left: auto;
    font-size: 12px;
  }
  .qrows {
    overflow: auto;
    padding: 0 10px 6px;
  }
  .qrow {
    display: grid;
    grid-template-columns: 1fr 160px auto auto;
    gap: 8px;
    align-items: center;
    font-size: 12px;
    padding: 2px 0;
  }
  .qacts {
    display: inline-flex;
    gap: 2px;
  }
  .qname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .qstat {
    opacity: 0.75;
    text-align: right;
    min-width: 120px;
  }
  .qrow.failed .qstat {
    color: tomato;
  }
  .qcancel {
    font-size: 11px;
    padding: 0 5px;
  }
</style>
