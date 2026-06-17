<script>
  import { invoke } from "./api.js";
  import Modal from "./Modal.svelte";

  let { sessionId, remotePath, selection = [], onClose } = $props();

  let input = $state("");
  let log = $state([]); // { cmd, code, stdout, stderr, error }
  let busy = $state(false);
  let editing = $state(false);

  // Custom command templates persist in the webview's localStorage.
  // {label, template} — template supports {} (selected names) and {dir}.
  const STORE = "scp-commands";
  let commands = $state(load());
  function load() {
    try { return JSON.parse(localStorage.getItem(STORE) || "[]"); } catch { return []; }
  }
  function persist() {
    localStorage.setItem(STORE, JSON.stringify(commands));
  }

  // Shell-quote and join the current remote selection for {} substitution.
  function selArg() {
    return selection.map((n) => `'${n.replace(/'/g, "'\\''")}'`).join(" ");
  }
  function expand(tmpl) {
    return tmpl.replaceAll("{}", selArg()).replaceAll("{dir}", `'${remotePath}'`);
  }

  async function run(cmd) {
    const c = cmd.trim();
    if (!c || busy) return;
    busy = true;
    try {
      const r = await invoke("remote_exec", { sessionId, cmd: c });
      log = [...log, { cmd: c, code: r.exit_code, stdout: r.stdout, stderr: r.stderr }];
    } catch (e) {
      log = [...log, { cmd: c, error: String(e) }];
    } finally {
      busy = false;
    }
  }
  function runInput() {
    run(input);
    input = "";
  }

  // --- template management ---
  let newLabel = $state("");
  let newTemplate = $state("");
  function addCommand() {
    if (!newLabel.trim() || !newTemplate.trim()) return;
    commands = [...commands, { label: newLabel.trim(), template: newTemplate.trim() }];
    persist();
    newLabel = "";
    newTemplate = "";
  }
  function removeCommand(i) {
    commands = commands.filter((_, j) => j !== i);
    persist();
  }
</script>

<Modal title="Remote console" {onClose}>
  <div class="cmds">
    {#each commands as c, i}
      <span class="cmd-chip">
        <button class="run-chip" title={c.template} onclick={() => run(expand(c.template))}>{c.label}</button>
        {#if editing}<button class="x" onclick={() => removeCommand(i)}>×</button>{/if}
      </span>
    {/each}
    <button class="edit" onclick={() => (editing = !editing)}>{editing ? "Done" : "Edit ⚙"}</button>
  </div>

  {#if editing}
    <form class="add" onsubmit={(e) => (e.preventDefault(), addCommand())}>
      <input placeholder="label" bind:value={newLabel} />
      <input placeholder={"template — {} = selection, {dir} = remote dir"} bind:value={newTemplate} />
      <button type="submit">Add</button>
    </form>
  {/if}

  <div class="scroll">
    {#each log as e}
      <div class="entry">
        <div class="cmdline">$ {e.cmd}</div>
        {#if e.error}
          <pre class="se">{e.error}</pre>
        {:else}
          {#if e.stdout}<pre>{e.stdout}</pre>{/if}
          {#if e.stderr}<pre class="se">{e.stderr}</pre>{/if}
          {#if e.code !== 0}<div class="code">exit {e.code}</div>{/if}
        {/if}
      </div>
    {/each}
    {#if !log.length}<div class="empty">Run a command on the server. Selected remote items fill <code>{"{}"}</code>.</div>{/if}
  </div>

  <form class="inputline" onsubmit={(e) => (e.preventDefault(), runInput())}>
    <input class="mono" placeholder="command…" bind:value={input} disabled={busy} autofocus />
    <button type="submit" disabled={busy}>{busy ? "…" : "Run"}</button>
  </form>
</Modal>

<style>
  .cmds { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; margin-bottom: 8px; }
  .cmd-chip { display: inline-flex; align-items: center; }
  .run-chip { font-size: 12px; padding: 3px 8px; }
  .x { border: none; background: none; color: tomato; cursor: pointer; padding: 0 4px; }
  .edit { font-size: 12px; opacity: 0.7; margin-left: auto; }
  .add { display: flex; gap: 6px; margin-bottom: 8px; }
  .add input { flex: 1; font: inherit; padding: 4px 6px; }
  .add input:first-child { flex: 0 0 110px; }
  .scroll {
    border: 1px solid var(--border);
    border-radius: 6px;
    background: color-mix(in srgb, var(--panel) 70%, #000 6%);
    height: 260px;
    overflow: auto;
    padding: 6px 8px;
    font-size: 12px;
    margin-bottom: 8px;
  }
  .entry { margin-bottom: 8px; }
  .cmdline { font-family: ui-monospace, monospace; opacity: 0.75; }
  pre { margin: 2px 0; white-space: pre-wrap; word-break: break-word; font-family: ui-monospace, monospace; }
  pre.se { color: tomato; }
  .code { color: orange; font-size: 11px; }
  .empty { opacity: 0.5; }
  .inputline { display: flex; gap: 6px; }
  .inputline input { flex: 1; padding: 5px 7px; }
  .mono { font-family: ui-monospace, monospace; }
</style>
