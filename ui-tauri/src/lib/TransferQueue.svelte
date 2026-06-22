<script>
  import { humanSize, humanRate, fmtEta } from "./api.js";

  let { queue = [], paused = false, speedKbs = 0, onTogglePause, onSpeed, onCancel, onClear, onRetry, onRetryAll, onRemove } = $props();

  const SPEEDS = [
    { v: 0, label: "Unlimited" },
    { v: 256, label: "256 KB/s" },
    { v: 1024, label: "1 MB/s" },
    { v: 5120, label: "5 MB/s" },
    { v: 10240, label: "10 MB/s" },
  ];

  let active = $derived(queue.filter((t) => t.state === "active"));
  let failed = $derived(queue.filter((t) => t.state === "failed"));
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
    if (t.state === "done") return `done · ${humanSize(t.total || t.done)}`;
    if (t.state === "failed") return `failed: ${t.error ?? ""}`;
    if (t.state === "cancelled") return "cancelled";
    const size = `${humanSize(t.done)}${t.total ? " / " + humanSize(t.total) : ""}`;
    const rate = t.speed ? ` · ${humanRate(t.speed)}` : "";
    const eta = t.eta ? ` · ${fmtEta(t.eta)} left` : "";
    return `${size}${rate}${eta}`;
  }
  function glyph(t) {
    if (t.state === "done") return "✓";
    if (t.state === "failed") return "✕";
    if (t.state === "cancelled") return "⊘";
    return t.upload ? "↑" : "↓";
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
      <span class="qspace"></span>
      <button class="qctl" class:on={paused} onclick={onTogglePause} title={paused ? "Resume transfers" : "Pause transfers"}>
        {paused ? "▶ Resume" : "❚❚ Pause"}
      </button>
      <select class="qspeed" value={speedKbs} onchange={(e) => onSpeed(Number(e.target.value))} title="Transfer speed limit">
        {#each SPEEDS as sp}<option value={sp.v}>{sp.label}</option>{/each}
      </select>
      {#if failed.length}
        <button class="retry-all" onclick={onRetryAll} title="Resume all failed transfers">
          ↻ Retry all ({failed.length})
        </button>
      {/if}
      <button class="clear" onclick={onClear} disabled={!queue.some((t) => t.state !== "active")}>
        Clear finished
      </button>
    </div>
    <div class="qrows">
      {#each queue as t (t.id)}
        <div class="qrow {t.state}">
          <span class="qg" title={t.upload ? "upload" : "download"}>{glyph(t)}</span>
          <span class="qname" title={t.name}>{t.name}</span>
          <progress max="100" value={pctOf(t)}></progress>
          <span class="qpct">{pctOf(t)}%</span>
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
  .qspace {
    flex: 1;
  }
  .qctl {
    font-size: 12px;
    padding: 3px 9px;
  }
  .qctl.on {
    color: var(--warn);
    border-color: var(--warn);
  }
  .qspeed {
    font-size: 12px;
    padding: 3px 6px;
  }
  .retry-all {
    font-size: 12px;
    color: var(--accent);
  }
  .clear {
    font-size: 12px;
  }
  .qrows {
    overflow: auto;
    padding: 0 10px 6px;
  }
  .qrow {
    display: grid;
    grid-template-columns: 16px 1fr 150px 40px auto auto;
    gap: 8px;
    align-items: center;
    font-size: 12px;
    padding: 3px 0;
  }
  .qg {
    text-align: center;
    font-weight: 700;
    color: var(--accent);
  }
  .qrow.done .qg {
    color: var(--ok);
  }
  .qrow.failed .qg,
  .qrow.cancelled .qg {
    color: var(--danger);
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
  .qpct {
    text-align: right;
    font-variant-numeric: tabular-nums;
    opacity: 0.8;
  }
  .qstat {
    opacity: 0.75;
    text-align: right;
    min-width: 120px;
    white-space: nowrap;
  }
  .qrow.failed .qstat {
    color: var(--danger);
    opacity: 1;
  }
  /* Themed progress bars: accent while active, green when done, red on fail. */
  progress {
    -webkit-appearance: none;
    appearance: none;
    height: 8px;
    border: none;
    border-radius: 4px;
    overflow: hidden;
  }
  progress::-webkit-progress-bar {
    background: var(--panel-2);
    border-radius: 4px;
  }
  progress::-webkit-progress-value {
    background: var(--accent);
    border-radius: 4px;
  }
  .qrow.done progress::-webkit-progress-value {
    background: var(--ok);
  }
  .qrow.failed progress::-webkit-progress-value,
  .qrow.cancelled progress::-webkit-progress-value {
    background: var(--danger);
  }
  .qcancel {
    font-size: 11px;
    padding: 0 5px;
  }
</style>
