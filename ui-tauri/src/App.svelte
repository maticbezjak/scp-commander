<script>
  import { invoke, listen, joinPath, humanSize } from "./lib/api.js";
  import Pane from "./lib/Pane.svelte";
  import TransferQueue from "./lib/TransferQueue.svelte";
  import Modal from "./lib/Modal.svelte";
  import ContextMenu from "./lib/ContextMenu.svelte";
  import SyncDialog from "./lib/SyncDialog.svelte";
  import ConsoleDialog from "./lib/ConsoleDialog.svelte";
  import KnownHostsDialog from "./lib/KnownHostsDialog.svelte";
  import PrefsDialog from "./lib/PrefsDialog.svelte";

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
    use_jump: false,
    jump_host: "",
    jump_port: 22,
    jump_user: "",
    jump_password: "",
    jump_auth_mode: "password",
    jump_key_path: "",
  });

  // Saved sites
  let sites = $state([]);
  let selectedSite = $state("");
  let saveSiteName = $state(null); // non-null = the Save-site dialog value
  async function reloadSites() {
    sites = await invoke("list_sites");
  }
  $effect(() => {
    reloadSites();
  });
  function applySite() {
    const s = sites.find((x) => x.name === selectedSite);
    if (!s) return;
    form = {
      ...form,
      protocol: s.protocol,
      host: s.host,
      port: s.port,
      username: s.username,
      password: "",
      auth_mode: s.auth_mode || "password",
      key_path: s.key_path || "",
      bucket: s.bucket || "",
      region: s.region || "",
      path: s.path || "/",
      use_jump: s.use_jump || false,
      jump_host: s.jump_host || "",
      jump_port: s.jump_port || 22,
      jump_user: s.jump_user || "",
      jump_password: "",
      jump_auth_mode: s.jump_auth_mode || "password",
      jump_key_path: s.jump_key_path || "",
    };
    status = `Loaded site “${s.name}” — enter password and Connect`;
  }
  async function saveSite() {
    const name = saveSiteName.trim();
    saveSiteName = null;
    if (!name) return;
    const { password, jump_password, ...rest } = form;
    await invoke("save_site", { site: { ...rest, name, port: Number(form.port) } });
    await reloadSites();
    selectedSite = name;
    status = `Saved site “${name}”`;
  }
  async function deleteSite() {
    if (!selectedSite) return;
    await invoke("delete_site", { name: selectedSite });
    selectedSite = "";
    await reloadSites();
  }

  // Preferences (loaded from backend on startup).
  let prefs = $state({
    show_hidden: false,
    confirm_delete: true,
    confirm_overwrite: true,
    atomic_uploads: true,
    max_parallel: 2,
  });
  $effect(() => {
    invoke("load_prefs").then((p) => {
      prefs = p;
      invoke("set_max_parallel", { n: prefs.max_parallel });
    });
  });
  async function savePrefs(next) {
    prefs = next;
    showPrefs = false;
    await invoke("save_prefs", { prefs: next });
    await invoke("set_max_parallel", { n: next.max_parallel });
  }

  // Phase 5 dialogs
  let showSync = $state(false);
  let showConsole = $state(false);
  let showKnownHosts = $state(false);
  let showPrefs = $state(false);

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

  // Phase 3: context menu + dialogs
  let ctx = $state(null); // { x, y, items }
  let renameTarget = $state(null); // { isLocal, entry, value }
  let newFolder = $state(null); // { isLocal, value }
  let deleteTarget = $state(null); // { isLocal, entries }
  let propsTarget = $state(null); // { isLocal, entry, mode }
  let overwrite = $state(null); // { entries, upload, count }

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
  // Mirror Pane's visible ordering so shift-range matches what's on screen.
  function sortedNames(entries) {
    return [...entries]
      .filter((e) => prefs.show_hidden || !e.name.startsWith("."))
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

  // --- transfers (with overwrite prompt) ---
  function enqueueEntry(e, upload, policy) {
    return invoke("enqueue", {
      upload,
      isDir: e.is_dir,
      name: e.name,
      local: joinPath(local.path, e.name, "/"),
      remote: joinPath(remote.path, e.name),
      overwrite: policy,
    });
  }
  function transfer(entries, upload) {
    if (!connected || !entries.length) return;
    const dest = upload ? remote : local;
    const destNames = new Set(dest.entries.map((e) => e.name));
    const collisions = entries.filter((e) => destNames.has(e.name));
    if (collisions.length && prefs.confirm_overwrite) {
      overwrite = { entries, upload, count: collisions.length };
    } else {
      for (const e of entries) enqueueEntry(e, upload, 0);
    }
  }

  // Compare the two panes: select entries that are missing on the other side
  // or differ in size. Folders are compared by presence only.
  function compareDirs() {
    if (!connected) return;
    const rByName = new Map(remote.entries.map((e) => [e.name, e]));
    const lByName = new Map(local.entries.map((e) => [e.name, e]));
    const differs = (a, b) => !b || (!a.is_dir && !b.is_dir && a.size !== b.size);
    localSel = local.entries.filter((e) => differs(e, rByName.get(e.name))).map((e) => e.name);
    remoteSel = remote.entries.filter((e) => differs(e, lByName.get(e.name))).map((e) => e.name);
    status = `Compare — ${localSel.length} local, ${remoteSel.length} remote differing`;
  }
  function resolveOverwrite(decision) {
    const { entries, upload } = overwrite;
    overwrite = null;
    if (decision === "cancel") return;
    const dest = upload ? remote : local;
    const byName = new Map(dest.entries.map((e) => [e.name, e]));
    const policy = decision === "skip" ? 1 : decision === "newer" ? 2 : 0;
    for (const e of entries) {
      const d = byName.get(e.name);
      if (e.is_dir) {
        enqueueEntry(e, upload, policy); // backend applies the policy per-file
      } else if (!d) {
        enqueueEntry(e, upload, 0);
      } else if (decision === "skip") {
        continue;
      } else if (decision === "newer") {
        if (e.mtime && d.mtime && e.mtime > d.mtime) enqueueEntry(e, upload, 0);
      } else {
        enqueueEntry(e, upload, 0);
      }
    }
  }
  function transferSelected(fromLocal) {
    const src = fromLocal ? local : remote;
    const sel = fromLocal ? localSel : remoteSel;
    transfer(src.entries.filter((e) => sel.includes(e.name)), fromLocal);
  }

  // --- file operations (context menu + dialogs) ---
  function fullPath(isLocal, name) {
    return isLocal ? joinPath(local.path, name, "/") : joinPath(remote.path, name);
  }
  function refresh(isLocal) {
    if (isLocal) loadLocal(local.path);
    else loadRemote(remote.path);
  }
  function openContext(isLocal, entry, index, ev) {
    rowClick(isLocal, entry, index, ev); // select the row under the cursor
    const sel = isLocal ? localSel : remoteSel;
    const entries = (isLocal ? local.entries : remote.entries).filter((e) =>
      sel.includes(e.name),
    );
    const targets = entries.length && sel.includes(entry.name) ? entries : [entry];
    const items = [
      {
        label: isLocal ? "Upload →" : "← Download",
        action: () => transfer(targets, isLocal),
      },
      { label: "Rename…", action: () => (renameTarget = { isLocal, entry, value: entry.name }) },
      {
        label: `Delete${targets.length > 1 ? ` (${targets.length})` : ""}…`,
        danger: true,
        action: () =>
          prefs.confirm_delete
            ? (deleteTarget = { isLocal, entries: targets })
            : doDeleteEntries(isLocal, targets),
      },
      { label: "New folder…", action: () => (newFolder = { isLocal, value: "" }) },
      {
        label: "Properties…",
        action: () =>
          (propsTarget = { isLocal, entry, mode: octalPerms(entry.perms) }),
      },
    ];
    ctx = { x: ev.clientX, y: ev.clientY, items };
  }
  async function doRename() {
    const { isLocal, entry, value } = renameTarget;
    const v = value.trim();
    renameTarget = null;
    if (!v || v === entry.name) return;
    const from = fullPath(isLocal, entry.name);
    const to = fullPath(isLocal, v);
    try {
      await invoke(isLocal ? "local_rename" : "remote_rename", { from, to });
      refresh(isLocal);
    } catch (e) {
      status = `Rename failed: ${e}`;
    }
  }
  async function doNewFolder() {
    const { isLocal, value } = newFolder;
    const v = value.trim();
    newFolder = null;
    if (!v) return;
    try {
      await invoke(isLocal ? "local_mkdir" : "remote_mkdir", { path: fullPath(isLocal, v) });
      refresh(isLocal);
    } catch (e) {
      status = `New folder failed: ${e}`;
    }
  }
  async function doDeleteEntries(isLocal, entries) {
    for (const e of entries) {
      try {
        await invoke(isLocal ? "local_delete" : "remote_delete", {
          path: fullPath(isLocal, e.name),
          isDir: e.is_dir,
        });
      } catch (err) {
        status = `Delete failed: ${err}`;
      }
    }
    refresh(isLocal);
  }
  async function doDelete() {
    const { isLocal, entries } = deleteTarget;
    deleteTarget = null;
    await doDeleteEntries(isLocal, entries);
  }
  async function doChmod() {
    const { entry, mode } = propsTarget;
    const m = parseInt(mode, 8);
    propsTarget = null;
    if (Number.isNaN(m)) return;
    try {
      await invoke("remote_chmod", { path: fullPath(false, entry.name), mode: m });
      refresh(false);
    } catch (e) {
      status = `chmod failed: ${e}`;
    }
  }
  // Extract the octal mode (e.g. "755") from a perms string like "-rwxr-xr-x".
  function octalPerms(perms) {
    if (!perms || perms.length < 10) return "644";
    const tri = (s) =>
      (s[0] === "r" ? 4 : 0) + (s[1] === "w" ? 2 : 0) + (s[2] === "x" ? 1 : 0);
    return `${tri(perms.slice(1, 4))}${tri(perms.slice(4, 7))}${tri(perms.slice(7, 10))}`;
  }
  function fmtTime(mtime) {
    return mtime ? new Date(mtime * 1000).toLocaleString() : "—";
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
  {#if isSftp}
    <label class="jump-toggle" title="Connect via a bastion / jump host">
      <input type="checkbox" bind:checked={form.use_jump} /> Jump
    </label>
  {/if}
  <span class="sites">
    <select bind:value={selectedSite} onchange={applySite} title="Saved sites">
      <option value="">— Sites —</option>
      {#each sites as s}<option value={s.name}>{s.name}</option>{/each}
    </select>
    <button type="button" title="Save current as a site" onclick={() => (saveSiteName = form.host)}>
      Save
    </button>
    <button type="button" title="Delete selected site" disabled={!selectedSite} onclick={deleteSite}>
      🗑
    </button>
  </span>
  {#if !connected}
    <button type="submit" disabled={busy}>Connect</button>
  {:else}
    <button type="button" onclick={disconnect}>Disconnect</button>
  {/if}
</form>

{#if isSftp && form.use_jump}
  <div class="jumprow">
    <span class="jump-label">via</span>
    <input class="host" placeholder="jump host" bind:value={form.jump_host} />
    <input class="port" placeholder="22" bind:value={form.jump_port} />
    <input placeholder="jump user" bind:value={form.jump_user} />
    <select bind:value={form.jump_auth_mode}>
      <option value="password">Password</option>
      <option value="key">Key file</option>
      <option value="agent">Agent</option>
    </select>
    {#if form.jump_auth_mode === "key"}
      <input placeholder="~/.ssh/id_ed25519" bind:value={form.jump_key_path} />
    {:else if form.jump_auth_mode !== "agent"}
      <input type="password" placeholder="jump password" bind:value={form.jump_password} />
    {/if}
  </div>
{/if}

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

<div class="toolbar">
  <button disabled={!connected} onclick={() => (showSync = true)} title="Synchronize directories">⇄ Sync</button>
  <button disabled={!connected} onclick={compareDirs} title="Select differing files">⇌ Compare</button>
  <button disabled={!connected} onclick={() => (showConsole = true)} title="Run remote commands">⌘ Console</button>
  <span class="sep"></span>
  <button onclick={() => (showKnownHosts = true)} title="Manage trusted host keys">🔑 Hosts</button>
  <button onclick={() => (showPrefs = true)} title="Preferences">⚙ Preferences</button>
</div>

<div class="panes">
  <div class="panewrap" onfocusin={() => (focusLocal = true)} onpointerdown={() => (focusLocal = true)}>
    <Pane
      title="Local"
      path={local.path}
      entries={local.entries}
      showHidden={prefs.show_hidden}
      selected={localSel}
      transferLabel="Upload →"
      canTransfer={connected}
      onUp={localUp}
      onNavigate={loadLocal}
      onOpen={(e) => loadLocal(joinPath(local.path, e.name, "/"))}
      onTransferOne={(e) => transfer([e], true)}
      onTransfer={() => transferSelected(true)}
      onRowClick={(e, i, ev) => rowClick(true, e, i, ev)}
      onContext={(e, i, ev) => openContext(true, e, i, ev)}
      onNewFolder={() => (newFolder = { isLocal: true, value: "" })}
      onRefresh={() => loadLocal(local.path)}
    />
  </div>
  <div class="panewrap" onfocusin={() => (focusLocal = false)} onpointerdown={() => (focusLocal = false)}>
    {#if connected}
      <Pane
        title="Remote"
        path={remote.path}
        entries={remote.entries}
        showHidden={prefs.show_hidden}
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
        onContext={(e, i, ev) => openContext(false, e, i, ev)}
        onNewFolder={() => (newFolder = { isLocal: false, value: "" })}
        onRefresh={() => loadRemote(remote.path)}
      />
    {:else}
      <div class="placeholder">Connect to a server to browse the remote side.</div>
    {/if}
  </div>
</div>

<TransferQueue {queue} onCancel={cancelTransfer} onClear={clearFinished} />

{#if ctx}
  <ContextMenu x={ctx.x} y={ctx.y} items={ctx.items} onClose={() => (ctx = null)} />
{/if}

{#if renameTarget}
  <Modal title="Rename" onClose={() => (renameTarget = null)}>
    <form onsubmit={(e) => (e.preventDefault(), doRename())}>
      <input class="dlg-input" bind:value={renameTarget.value} autofocus />
      <div class="dlg-actions">
        <button type="button" onclick={() => (renameTarget = null)}>Cancel</button>
        <button type="submit">Rename</button>
      </div>
    </form>
  </Modal>
{/if}

{#if newFolder}
  <Modal title="New folder" onClose={() => (newFolder = null)}>
    <form onsubmit={(e) => (e.preventDefault(), doNewFolder())}>
      <input class="dlg-input" placeholder="folder name" bind:value={newFolder.value} autofocus />
      <div class="dlg-actions">
        <button type="button" onclick={() => (newFolder = null)}>Cancel</button>
        <button type="submit">Create</button>
      </div>
    </form>
  </Modal>
{/if}

{#if deleteTarget}
  <Modal title="Delete" onClose={() => (deleteTarget = null)}>
    <p>
      Delete {deleteTarget.entries.length === 1
        ? `“${deleteTarget.entries[0].name}”`
        : `${deleteTarget.entries.length} items`}?
      {#if deleteTarget.entries.some((e) => e.is_dir)}<br /><small>Folders are removed recursively.</small>{/if}
    </p>
    <div class="dlg-actions">
      <button onclick={() => (deleteTarget = null)}>Cancel</button>
      <button class="danger" onclick={doDelete}>Delete</button>
    </div>
  </Modal>
{/if}

{#if propsTarget}
  <Modal title={propsTarget.entry.name} onClose={() => (propsTarget = null)}>
    <div class="props">
      <span>Type</span><span>{propsTarget.entry.is_dir ? "Folder" : propsTarget.entry.is_symlink ? "Symlink" : "File"}</span>
      <span>Size</span><span>{humanSize(propsTarget.entry.size)}</span>
      <span>Modified</span><span>{fmtTime(propsTarget.entry.mtime)}</span>
      {#if propsTarget.entry.perms}<span>Perms</span><span class="mono">{propsTarget.entry.perms}</span>{/if}
    </div>
    {#if !propsTarget.isLocal}
      <form class="chmod" onsubmit={(e) => (e.preventDefault(), doChmod())}>
        <label>Permissions (octal) <input class="mono" size="4" bind:value={propsTarget.mode} /></label>
        <button type="submit">Apply</button>
      </form>
    {/if}
    <div class="dlg-actions">
      <button onclick={() => (propsTarget = null)}>Close</button>
    </div>
  </Modal>
{/if}

{#if saveSiteName !== null}
  <Modal title="Save site" onClose={() => (saveSiteName = null)}>
    <form onsubmit={(e) => (e.preventDefault(), saveSite())}>
      <input class="dlg-input" placeholder="site name" bind:value={saveSiteName} autofocus />
      <p class="hint">Stores connection settings (no password).</p>
      <div class="dlg-actions">
        <button type="button" onclick={() => (saveSiteName = null)}>Cancel</button>
        <button type="submit">Save</button>
      </div>
    </form>
  </Modal>
{/if}

{#if overwrite}
  <Modal title="Files already exist" onClose={() => (overwrite = null)}>
    <p>{overwrite.count} item(s) already exist at the destination. What should happen?</p>
    <div class="dlg-actions wrap">
      <button class="danger" onclick={() => resolveOverwrite("overwrite")}>Overwrite</button>
      <button onclick={() => resolveOverwrite("newer")}>Only newer</button>
      <button onclick={() => resolveOverwrite("skip")}>Skip existing</button>
      <button onclick={() => resolveOverwrite("cancel")}>Cancel</button>
    </div>
  </Modal>
{/if}

{#if showSync}
  <SyncDialog localPath={local.path} remotePath={remote.path} onClose={() => (showSync = false)} />
{/if}

{#if showConsole}
  <ConsoleDialog remotePath={remote.path} selection={remoteSel} onClose={() => (showConsole = false)} />
{/if}

{#if showKnownHosts}
  <KnownHostsDialog onClose={() => (showKnownHosts = false)} />
{/if}

{#if showPrefs}
  <PrefsDialog {prefs} onSave={savePrefs} onClose={() => (showPrefs = false)} />
{/if}

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
  .jump-toggle {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 12px;
    white-space: nowrap;
  }
  .sites {
    display: flex;
    gap: 4px;
    align-items: center;
    margin-left: auto;
  }
  .jumprow {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    padding: 0 10px 8px;
  }
  .jumprow input, .jumprow select { font: inherit; padding: 4px 6px; }
  .jumprow .host { flex: 1; min-width: 120px; }
  .jumprow .port { width: 64px; }
  .jump-label {
    font-size: 12px;
    opacity: 0.6;
    padding-left: 2px;
  }
  .hint { font-size: 12px; opacity: 0.6; margin: 0 0 12px; }
  .hostkey {
    margin: 0 10px 8px;
    padding: 8px 10px;
    border-radius: 6px;
    background: color-mix(in srgb, orange 18%, var(--panel));
    font-size: 13px;
  }
  .hostkey.mismatch { background: color-mix(in srgb, red 22%, var(--panel)); }
  .hostkey code { font-family: ui-monospace, monospace; }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 10px 8px;
  }
  .toolbar button {
    font: inherit;
    font-size: 12px;
    padding: 4px 8px;
  }
  .toolbar .sep {
    flex: 1;
  }
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
  .dlg-input {
    width: 100%;
    font: inherit;
    padding: 5px 7px;
    margin-bottom: 12px;
  }
  .dlg-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .dlg-actions.wrap {
    flex-wrap: wrap;
  }
  .dlg-actions button {
    padding: 5px 12px;
  }
  button.danger {
    border-color: tomato;
    color: tomato;
  }
  .props {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 16px;
    font-size: 13px;
    margin-bottom: 12px;
  }
  .props span:nth-child(odd) {
    opacity: 0.6;
  }
  .mono {
    font-family: ui-monospace, monospace;
  }
  .chmod {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 12px;
    font-size: 13px;
  }
</style>
