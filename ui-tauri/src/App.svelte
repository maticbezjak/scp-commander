<script>
  import { invoke, listen, joinPath } from "./lib/api.js";
  import Pane from "./lib/Pane.svelte";
  import TransferQueue from "./lib/TransferQueue.svelte";

  const PROTOS = ["sftp", "ftp", "ftps", "s3"];

  let form = $state({
    protocol: "sftp",
    host: "",
    port: 22,
    username: "",
    password: "",
    auth_mode: "password",
    key_path: "",
    bucket: "",
    region: "",
    path: "/",
  });

  let connected = $state(false);
  let status = $state("Not connected");
  let busy = $state(false);
  let hostKey = $state(null);

  let local = $state({ path: "/", entries: [] });
  let remote = $state({ path: "/", entries: [] });
  let localSel = $state([]);
  let remoteSel = $state([]);
  let localAnchor = -1;
  let remoteAnchor = -1;

  let queue = $state([]);

  const isSftp = $derived(form.protocol === "sftp");
  const isS3 = $derived(form.protocol === "s3");

  // --- transfer events from the backend worker ---
  $effect(() => {
    const un = listen("xfer", (e) => onXfer(e.payload));
    return () => un.then((f) => f());
  });
  function onXfer(p) {
    const t = queue.find((x) => x.id === p.id);
    switch (p.event) {
      case "started":
        if (!t)
          queue.push({ id: p.id, name: p.name, upload: p.upload, done: 0, total: p.total, state: "active" });
        break;
      case "progress":
        if (t) { t.done = p.done; t.total = p.total; }
        break;
      case "done":
        if (t) { t.state = "done"; t.done = t.total || t.done; }
        if (p.upload) loadRemote(remote.path);
        else loadLocal(local.path);
        break;
      case "failed":
        if (t) { t.state = "failed"; t.error = p.message; }
        break;
      case "cancelled":
        if (t) t.state = "cancelled";
        break;
    }
  }

  // --- local pane ---
  let started = false;
  $effect(() => {
    if (started) return;
    started = true;
    invoke("home_local").then(loadLocal);
  });
  async function loadLocal(path) {
    try {
      const entries = await invoke("list_local", { path });
      local = { path, entries };
      localSel = [];
    } catch (e) {
      status = `Local: ${e}`;
    }
  }
  async function localUp() {
    loadLocal(await invoke("parent_local", { path: local.path }));
  }

  // --- remote pane ---
  async function loadRemote(path) {
    busy = true;
    try {
      const entries = await invoke("list_remote", { path });
      remote = { path, entries };
      remoteSel = [];
      status = `${path} — ${entries.length} item(s)`;
    } catch (e) {
      status = `Error: ${e}`;
    } finally {
      busy = false;
    }
  }
  const remoteUp = () => loadRemote(joinPath(remote.path, ".."));

  // --- selection (click / cmd-click toggle / shift-range) ---
  function sortedNames(entries) {
    return [...entries]
      .sort((a, b) => Number(b.is_dir) - Number(a.is_dir) || a.name.localeCompare(b.name))
      .map((e) => e.name);
  }
  function rowClick(isLocal, entry, index, ev) {
    const names = sortedNames(isLocal ? local.entries : remote.entries);
    let sel = isLocal ? localSel : remoteSel;
    if (ev.metaKey || ev.ctrlKey) {
      sel = sel.includes(entry.name) ? sel.filter((n) => n !== entry.name) : [...sel, entry.name];
      if (isLocal) localAnchor = index; else remoteAnchor = index;
    } else if (ev.shiftKey) {
      const anchor = isLocal ? localAnchor : remoteAnchor;
      const [a, b] = anchor < 0 ? [index, index] : [Math.min(anchor, index), Math.max(anchor, index)];
      sel = names.slice(a, b + 1);
    } else {
      sel = [entry.name];
      if (isLocal) localAnchor = index; else remoteAnchor = index;
    }
    if (isLocal) localSel = sel; else remoteSel = sel;
  }

  // --- transfers ---
  async function transfer(entries, upload) {
    if (!connected) return;
    for (const e of entries) {
      await invoke("enqueue", {
        upload,
        isDir: e.is_dir,
        name: e.name,
        local: joinPath(local.path, e.name, "/"),
        remote: joinPath(remote.path, e.name),
      });
    }
  }
  function transferSelected(fromLocal) {
    const src = fromLocal ? local : remote;
    const sel = fromLocal ? localSel : remoteSel;
    transfer(src.entries.filter((e) => sel.includes(e.name)), fromLocal);
  }
  async function cancelTransfer(id) {
    await invoke("cancel_transfer", { id });
  }
  function clearFinished() {
    queue = queue.filter((t) => t.state === "active");
  }

  // F5 transfers the focused pane's selection.
  function onKey(ev) {
    if (ev.key === "F5" && connected) {
      const inField = ["INPUT", "SELECT", "TEXTAREA"].includes(document.activeElement?.tagName);
      if (inField) return;
      ev.preventDefault();
      transferSelected(focusLocal);
    }
  }
  let focusLocal = $state(true);

  // --- connect ---
  const defaultPort = (p) => (p === "sftp" ? 22 : p === "s3" ? 443 : 21);
  async function connect(trustFingerprint) {
    busy = true;
    status = "Connecting…";
    try {
      const res = await invoke("connect_session", {
        form: { ...form, port: Number(form.port) },
        trustFingerprint: trustFingerprint ?? null,
      });
      switch (res.status) {
        case "connected":
          connected = true;
          hostKey = null;
          remote = { path: res.path, entries: res.entries };
          remoteSel = [];
          status = `Connected — ${res.entries.length} item(s)`;
          break;
        case "unknown_host_key":
          hostKey = { fingerprint: res.fingerprint, mismatch: false };
          status = "Unknown host key — confirm to continue";
          break;
        case "host_key_mismatch":
          hostKey = { fingerprint: res.fingerprint, mismatch: true };
          status = "HOST KEY MISMATCH";
          break;
        case "error":
          status = `Error: ${res.message}`;
          break;
      }
    } finally {
      busy = false;
    }
  }
  async function disconnect() {
    await invoke("disconnect");
    connected = false;
    remote = { path: "/", entries: [] };
    status = "Disconnected";
  }
</script>

<svelte:window onkeydown={onKey} />

<header>
  <strong>SCP Commander</strong>
  <span class="muted">— Tauri</span>
  <span class="status">{status}</span>
</header>

<form class="login" onsubmit={(e) => (e.preventDefault(), connect())}>
  <select bind:value={form.protocol} onchange={() => (form.port = defaultPort(form.protocol))}>
    {#each PROTOS as p}<option value={p}>{p.toUpperCase()}</option>{/each}
  </select>
  <input class="host" placeholder={isS3 ? "endpoint (blank = AWS)" : "host"} bind:value={form.host} />
  <input class="port" bind:value={form.port} />
  <input placeholder={isS3 ? "access key" : "user"} bind:value={form.username} />
  {#if isSftp}
    <select bind:value={form.auth_mode}>
      <option value="password">Password</option>
      <option value="key">Key file</option>
      <option value="agent">Agent</option>
    </select>
  {/if}
  {#if form.auth_mode === "key" && isSftp}
    <input placeholder="~/.ssh/id_ed25519" bind:value={form.key_path} />
  {:else if !(isSftp && form.auth_mode === "agent")}
    <input type="password" placeholder={isS3 ? "secret key" : "password"} bind:value={form.password} />
  {/if}
  {#if isS3}<input placeholder="bucket" bind:value={form.bucket} />{/if}
  {#if !connected}
    <button type="submit" disabled={busy}>Connect</button>
  {:else}
    <button type="button" onclick={disconnect}>Disconnect</button>
  {/if}
</form>

{#if hostKey}
  <div class="hostkey" class:mismatch={hostKey.mismatch}>
    {#if hostKey.mismatch}
      ⚠ The server's host key <code>{hostKey.fingerprint}</code> contradicts the stored one —
      possible man-in-the-middle. Connection refused.
      <button onclick={() => (hostKey = null)}>Dismiss</button>
    {:else}
      Unknown server key: <code>{hostKey.fingerprint}</code>
      <button onclick={() => connect(hostKey.fingerprint)}>Trust & Connect</button>
      <button onclick={() => (hostKey = null)}>Cancel</button>
    {/if}
  </div>
{/if}

<div class="panes">
  <div class="panewrap" onfocusin={() => (focusLocal = true)} onpointerdown={() => (focusLocal = true)}>
    <Pane
      title="Local"
      path={local.path}
      entries={local.entries}
      selected={localSel}
      transferLabel="Upload →"
      canTransfer={connected}
      onUp={localUp}
      onNavigate={loadLocal}
      onOpen={(e) => loadLocal(joinPath(local.path, e.name, "/"))}
      onTransferOne={(e) => transfer([e], true)}
      onTransfer={() => transferSelected(true)}
      onRowClick={(e, i, ev) => rowClick(true, e, i, ev)}
    />
  </div>
  <div class="panewrap" onfocusin={() => (focusLocal = false)} onpointerdown={() => (focusLocal = false)}>
    {#if connected}
      <Pane
        title="Remote"
        path={remote.path}
        entries={remote.entries}
        {busy}
        selected={remoteSel}
        transferLabel="← Download"
        canTransfer={connected}
        onUp={remoteUp}
        onNavigate={loadRemote}
        onOpen={(e) => loadRemote(joinPath(remote.path, e.name))}
        onTransferOne={(e) => transfer([e], false)}
        onTransfer={() => transferSelected(false)}
        onRowClick={(e, i, ev) => rowClick(false, e, i, ev)}
      />
    {:else}
      <div class="placeholder">Connect to a server to browse the remote side.</div>
    {/if}
  </div>
</div>

<TransferQueue {queue} onCancel={cancelTransfer} onClear={clearFinished} />

<style>
  header {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 8px 10px 4px;
  }
  .muted { opacity: 0.5; }
  .status { margin-left: auto; font-size: 12px; opacity: 0.8; }
  .login {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 4px 10px 8px;
    align-items: center;
  }
  .login input, .login select, .login button { font: inherit; padding: 4px 6px; }
  .login .host { flex: 1; min-width: 140px; }
  .login .port { width: 64px; }
  .hostkey {
    margin: 0 10px 8px;
    padding: 8px 10px;
    border-radius: 6px;
    background: color-mix(in srgb, orange 18%, var(--panel));
    font-size: 13px;
  }
  .hostkey.mismatch { background: color-mix(in srgb, red 22%, var(--panel)); }
  .hostkey code { font-family: ui-monospace, monospace; }
  .panes {
    display: flex;
    gap: 8px;
    padding: 0 10px 8px;
    flex: 1;
    min-height: 0;
  }
  .panewrap { display: flex; flex: 1 1 0; min-width: 0; }
  .placeholder {
    flex: 1;
    display: grid;
    place-items: center;
    border: 1px dashed var(--border);
    border-radius: 6px;
    opacity: 0.6;
    font-size: 13px;
  }
</style>
