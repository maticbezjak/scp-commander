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
    const site = {
      ...rest,
      name,
      port: Number(form.port) || 22, // inputs yield strings; the backend wants u16
      jump_port: Number(form.jump_port) || 22,
    };
    try {
      await invoke("save_site", { site });
      await reloadSites();
      selectedSite = name;
      status = `Saved site “${name}”`;
    } catch (e) {
      status = `Save site failed: ${e}`;
    }
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
    show_owner_group: false,
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

  let status = $state("Not connected");
  let busy = $state(false);
  let hostKey = $state(null);

  // Sessions / tabs. Each tab is one backend session (id) with its own remote
  // pane state; the local pane is shared across all tabs.
  let tabs = $state([]); // [{ id, label }]
  let activeId = $state(null); // active session id, or null when none open
  const connected = $derived(activeId !== null);
  const activeLabel = $derived(tabs.find((t) => t.id === activeId)?.label ?? "");
  const tabRemote = {}; // id -> { remote, remoteSel, remoteNav, remoteRecents, remoteHome }

  let local = $state({ path: "", entries: [] });
  let remote = $state({ path: "", entries: [] });
  let localSel = $state([]);
  let remoteSel = $state([]);
  let localAnchor = -1;
  let remoteAnchor = -1;

  // Per-pane navigation history + recent locations.
  let localNav = $state({ back: [], fwd: [] });
  let remoteNav = $state({ back: [], fwd: [] });
  let localRecents = $state([]);
  let remoteRecents = $state([]);
  let remoteHome = $state("/");
  let showLogin = $state(true);

  let queue = $state([]);

  function pushRecent(isLocal, p) {
    const arr = isLocal ? localRecents : remoteRecents;
    const next = [p, ...arr.filter((x) => x !== p)].slice(0, 12);
    if (isLocal) localRecents = next;
    else remoteRecents = next;
  }
  function goBack(isLocal) {
    const nav = isLocal ? localNav : remoteNav;
    if (!nav.back.length) return;
    const target = nav.back[nav.back.length - 1];
    const cur = isLocal ? local.path : remote.path;
    nav.back = nav.back.slice(0, -1);
    nav.fwd = [cur, ...nav.fwd];
    isLocal ? loadLocal(target, false) : loadRemote(target, false);
  }
  function goForward(isLocal) {
    const nav = isLocal ? localNav : remoteNav;
    if (!nav.fwd.length) return;
    const target = nav.fwd[0];
    const cur = isLocal ? local.path : remote.path;
    nav.fwd = nav.fwd.slice(1);
    nav.back = [...nav.back, cur];
    isLocal ? loadLocal(target, false) : loadRemote(target, false);
  }
  function goHomeLocal() {
    invoke("home_local").then((h) => loadLocal(h));
  }
  function goHomeRemote() {
    loadRemote(remoteHome);
  }

  // --- tabs (sessions) ---
  function snapshotActive() {
    if (activeId == null) return;
    tabRemote[activeId] = { remote, remoteSel, remoteNav, remoteRecents, remoteHome };
  }
  function switchTab(id) {
    if (id === activeId) return;
    snapshotActive();
    const s = tabRemote[id];
    if (s) {
      remote = s.remote;
      remoteSel = s.remoteSel;
      remoteNav = s.remoteNav;
      remoteRecents = s.remoteRecents;
      remoteHome = s.remoteHome;
    }
    activeId = id;
    busy = false;
    status = `${remote.path} — ${remote.entries.length} item(s)`;
  }
  async function closeTab(id) {
    await invoke("disconnect", { sessionId: id });
    delete tabRemote[id];
    const idx = tabs.findIndex((t) => t.id === id);
    tabs = tabs.filter((t) => t.id !== id);
    if (activeId === id) {
      if (tabs.length) {
        const next = tabs[Math.min(idx, tabs.length - 1)];
        activeId = null; // force switchTab to load the snapshot
        switchTab(next.id);
      } else {
        activeId = null;
        remote = { path: "", entries: [] };
        remoteSel = [];
        status = "Disconnected";
      }
    }
  }
  function newSession() {
    selectedSite = "";
    hostKey = null;
    showLogin = true;
  }

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
    // Transfer ids are per-session, so match on both.
    const t = queue.find((x) => x.id === p.id && x.session === p.session);
    switch (p.event) {
      case "started":
        if (!t)
          queue.push({ id: p.id, session: p.session, name: p.name, upload: p.upload, done: 0, total: p.total, state: "active" });
        break;
      case "progress":
        if (t) { t.done = p.done; t.total = p.total; }
        break;
      case "done": {
        if (t) { t.state = "done"; t.done = t.total || t.done; }
        // Refresh the affected pane: remote only for the active session; local
        // is shared so always refresh on a download.
        if (p.upload) { if (p.session === activeId) loadRemote(remote.path, false); }
        else loadLocal(local.path, false);
        // Move: delete the source now that the copy landed.
        const mk = `${p.session}:${p.id}`;
        if (pendingMove.has(mk)) {
          const mv = pendingMove.get(mk);
          pendingMove.delete(mk);
          invoke(mv.isLocal ? "local_delete" : "remote_delete", {
            sessionId: p.session, path: mv.path, isDir: mv.is_dir,
          })
            .then(() => {
              if (mv.isLocal) loadLocal(local.path, false);
              else if (p.session === activeId) loadRemote(remote.path, false);
            })
            .catch((e) => (status = `Move cleanup failed: ${e}`));
        }
        break;
      }
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
  async function loadLocal(path, record = true) {
    try {
      const prev = local.path;
      const entries = await invoke("list_local", { path });
      if (record && prev && prev !== path) {
        localNav.back = [...localNav.back, prev];
        localNav.fwd = [];
      }
      local = { path, entries };
      localSel = [];
      pushRecent(true, path);
    } catch (e) {
      status = `Local: ${e}`;
    }
  }
  async function localUp() {
    loadLocal(await invoke("parent_local", { path: local.path }));
  }

  // --- remote pane ---
  async function loadRemote(path, record = true) {
    busy = true;
    try {
      const prev = remote.path;
      const entries = await invoke("list_remote", { sessionId: activeId, path });
      if (record && prev && prev !== path) {
        remoteNav.back = [...remoteNav.back, prev];
        remoteNav.fwd = [];
      }
      remote = { path, entries };
      remoteSel = [];
      pushRecent(false, path);
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
  // Transfers whose source should be deleted on success (F6 move).
  const pendingMove = new Map(); // `${session}:${id}` -> { isLocal, path, is_dir }
  function enqueueEntry(e, upload, policy, move = false) {
    const localPath = joinPath(local.path, e.name, "/");
    const remotePath = joinPath(remote.path, e.name);
    const p = invoke("enqueue", {
      sessionId: activeId,
      upload,
      isDir: e.is_dir,
      name: e.name,
      local: localPath,
      remote: remotePath,
      overwrite: policy,
    });
    if (move) {
      const sess = activeId;
      p.then((id) =>
        pendingMove.set(`${sess}:${id}`, {
          isLocal: upload, // source side: upload=local→remote, so source is local
          path: upload ? localPath : remotePath,
          is_dir: e.is_dir,
        }),
      );
    }
    return p;
  }
  function transfer(entries, upload, move = false) {
    if (!connected || !entries.length) return;
    const dest = upload ? remote : local;
    const destNames = new Set(dest.entries.map((e) => e.name));
    const collisions = entries.filter((e) => destNames.has(e.name));
    if (collisions.length && prefs.confirm_overwrite) {
      overwrite = { entries, upload, move, count: collisions.length };
    } else {
      for (const e of entries) enqueueEntry(e, upload, 0, move);
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
    const { entries, upload, move } = overwrite;
    overwrite = null;
    if (decision === "cancel") return;
    const dest = upload ? remote : local;
    const byName = new Map(dest.entries.map((e) => [e.name, e]));
    const policy = decision === "skip" ? 1 : decision === "newer" ? 2 : 0;
    for (const e of entries) {
      const d = byName.get(e.name);
      if (e.is_dir) {
        enqueueEntry(e, upload, policy, move); // backend applies the policy per-file
      } else if (!d) {
        enqueueEntry(e, upload, 0, move);
      } else if (decision === "skip") {
        continue;
      } else if (decision === "newer") {
        if (e.mtime && d.mtime && e.mtime > d.mtime) enqueueEntry(e, upload, 0, move);
      } else {
        enqueueEntry(e, upload, 0, move);
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
      !entry.is_dir && { label: "View (F3)", action: () => viewFile(isLocal, entry) },
      { label: "Rename… (F2)", action: () => (renameTarget = { isLocal, entry, value: entry.name }) },
      !isLocal && !entry.is_dir && {
        label: "Duplicate…",
        action: () => (dupTarget = { entry, value: entry.name }),
      },
      {
        label: `Delete${targets.length > 1 ? ` (${targets.length})` : ""}…`,
        danger: true,
        action: () =>
          prefs.confirm_delete
            ? (deleteTarget = { isLocal, entries: targets })
            : doDeleteEntries(isLocal, targets),
      },
      { label: "New folder…", action: () => (newFolder = { isLocal, value: "" }) },
      { label: "Copy path", action: () => copyToClipboard(fullPath(isLocal, entry.name)) },
      !isLocal && { label: "Copy URL", action: () => copyToClipboard(remoteUrl(entry.name)) },
      isLocal && {
        label: "Reveal in Finder",
        action: () => invoke("reveal_path", { path: fullPath(true, entry.name) }),
      },
      {
        label: "Properties…",
        action: () =>
          (propsTarget = { isLocal, entry, mode: octalPerms(entry.perms) }),
      },
    ].filter(Boolean);
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
      await invoke(isLocal ? "local_rename" : "remote_rename", { sessionId: activeId, from, to });
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
      await invoke(isLocal ? "local_mkdir" : "remote_mkdir", { sessionId: activeId, path: fullPath(isLocal, v) });
      refresh(isLocal);
    } catch (e) {
      status = `New folder failed: ${e}`;
    }
  }
  async function doDeleteEntries(isLocal, entries) {
    for (const e of entries) {
      try {
        await invoke(isLocal ? "local_delete" : "remote_delete", {
          sessionId: activeId,
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
  // Toolbar delete/properties — honor the confirm-delete preference.
  function requestDelete(isLocal, entries) {
    if (!entries || !entries.length) return;
    if (prefs.confirm_delete) deleteTarget = { isLocal, entries };
    else doDeleteEntries(isLocal, entries);
  }
  function openProps(isLocal, entry) {
    if (!entry) return;
    propsTarget = { isLocal, entry, mode: octalPerms(entry.perms) };
  }
  async function toggleHidden() {
    const next = { ...prefs, show_hidden: !prefs.show_hidden };
    prefs = next;
    await invoke("save_prefs", { prefs: next });
  }
  async function doChmod() {
    const { entry, mode } = propsTarget;
    const m = parseInt(mode, 8);
    propsTarget = null;
    if (Number.isNaN(m)) return;
    try {
      await invoke("remote_chmod", { sessionId: activeId, path: fullPath(false, entry.name), mode: m });
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
  async function cancelTransfer(item) {
    await invoke("cancel_transfer", { sessionId: item.session, id: item.id });
  }
  function clearFinished() {
    queue = queue.filter((t) => t.state === "active");
  }

  let focusLocal = $state(true);

  function selectedEntriesIn(isLocal) {
    const sel = isLocal ? localSel : remoteSel;
    return (isLocal ? local.entries : remote.entries).filter((e) => sel.includes(e.name));
  }

  // Keyboard commander: F5 copy · F6 move · F2 rename · F3 view · Del delete ·
  // Tab switch panes · Backspace up · Enter open · type-ahead row jump.
  let typeAhead = "";
  let typeAheadAt = 0;
  function anyModalOpen() {
    return (
      renameTarget || newFolder || deleteTarget || propsTarget || overwrite ||
      dupTarget || viewer || showLogin || showSync || showConsole ||
      showKnownHosts || showPrefs
    );
  }
  function onKey(ev) {
    if (anyModalOpen()) return;
    if (["INPUT", "SELECT", "TEXTAREA"].includes(document.activeElement?.tagName)) return;
    const isLocal = focusLocal;
    const remoteOk = isLocal || connected;
    switch (ev.key) {
      case "Tab":
        ev.preventDefault();
        focusLocal = !focusLocal;
        return;
      case "F5":
        if (connected) { ev.preventDefault(); transferSelected(isLocal); }
        return;
      case "F6":
        if (connected) { ev.preventDefault(); moveSelected(isLocal); }
        return;
      case "F2": {
        ev.preventDefault();
        const e = selectedEntriesIn(isLocal)[0];
        if (e) renameTarget = { isLocal, entry: e, value: e.name };
        return;
      }
      case "F3": {
        ev.preventDefault();
        const e = selectedEntriesIn(isLocal)[0];
        if (e && !e.is_dir) viewFile(isLocal, e);
        return;
      }
      case "Delete": {
        ev.preventDefault();
        const es = selectedEntriesIn(isLocal);
        if (es.length) requestDelete(isLocal, es);
        return;
      }
      case "Backspace":
        ev.preventDefault();
        if (isLocal) localUp();
        else if (connected) remoteUp();
        return;
      case "Enter": {
        ev.preventDefault();
        const e = selectedEntriesIn(isLocal)[0];
        if (e && e.is_dir && remoteOk) {
          if (isLocal) loadLocal(joinPath(local.path, e.name, "/"));
          else loadRemote(joinPath(remote.path, e.name));
        }
        return;
      }
    }
    // Type-ahead: a printable key jumps to the first matching row.
    if (ev.key.length === 1 && !ev.metaKey && !ev.ctrlKey && !ev.altKey && /[\w.\- ]/.test(ev.key)) {
      ev.preventDefault();
      const now = Date.now();
      typeAhead = now - typeAheadAt < 1000 ? typeAhead + ev.key.toLowerCase() : ev.key.toLowerCase();
      typeAheadAt = now;
      const names = sortedNames(isLocal ? local.entries : remote.entries);
      const hit = names.find((n) => n.toLowerCase().startsWith(typeAhead));
      if (hit) {
        if (isLocal) localSel = [hit];
        else remoteSel = [hit];
      }
    }
  }

  // F6 move = copy to the other side, then delete the source on success.
  function moveSelected(fromLocal) {
    const src = fromLocal ? local : remote;
    const sel = fromLocal ? localSel : remoteSel;
    transfer(src.entries.filter((e) => sel.includes(e.name)), fromLocal, true);
  }

  // Built-in file viewer (F3).
  let viewer = $state(null); // { name, text }
  async function viewFile(isLocal, e) {
    if (e.is_dir) return;
    if (e.size > 1048576) { status = "File too large to view (>1 MiB)"; return; }
    try {
      const text = await invoke(isLocal ? "local_read_text" : "remote_read_text", {
        sessionId: activeId,
        path: fullPath(isLocal, e.name),
      });
      viewer = { name: e.name, text };
    } catch (err) {
      status = `View failed: ${err}`;
    }
  }

  // Duplicate a remote file (server-side copy).
  let dupTarget = $state(null); // { entry, value }
  async function doDuplicate() {
    const { entry, value } = dupTarget;
    const v = value.trim();
    dupTarget = null;
    if (!v || v === entry.name) return;
    try {
      await invoke("remote_copy", {
        sessionId: activeId,
        src: fullPath(false, entry.name),
        dst: fullPath(false, v),
      });
      loadRemote(remote.path, false);
    } catch (e) {
      status = `Duplicate failed: ${e}`;
    }
  }

  async function copyToClipboard(text) {
    try {
      await navigator.clipboard.writeText(text);
      status = `Copied: ${text}`;
    } catch {
      status = "Copy failed";
    }
  }
  function remoteUrl(name) {
    const c = tabs.find((t) => t.id === activeId);
    if (!c) return "";
    const auth = c.user ? `${c.user}@` : "";
    return `${c.proto}://${auth}${c.host}:${c.port}${joinPath(remote.path, name)}`;
  }

  // --- drag and drop between panes ---
  let dragData = null; // { fromLocal, entries }
  function onDragStartRow(isLocal, entry) {
    const sel = isLocal ? localSel : remoteSel;
    const entries = (isLocal ? local.entries : remote.entries).filter((e) => sel.includes(e.name));
    dragData = { fromLocal: isLocal, entries: entries.length && sel.includes(entry.name) ? entries : [entry] };
  }
  function onDropPane(toLocal) {
    if (!dragData || dragData.fromLocal === toLocal) { dragData = null; return; }
    transfer(dragData.entries, !toLocal); // dropping onto remote (toLocal=false) = upload
    dragData = null;
  }

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
        case "connected": {
          hostKey = null;
          showLogin = false;
          // Save the current tab's pane state, then open the new session's tab.
          snapshotActive();
          const label =
            (form.username ? form.username + "@" : "") + (form.host || form.bucket || "session");
          tabs = [
            ...tabs,
            {
              id: res.session_id,
              label,
              proto: form.protocol,
              host: form.host,
              port: Number(form.port),
              user: form.username,
            },
          ];
          remote = { path: res.path, entries: res.entries };
          remoteSel = [];
          remoteHome = res.path;
          remoteNav = { back: [], fwd: [] };
          remoteRecents = [res.path];
          activeId = res.session_id;
          status = `Connected — ${res.entries.length} item(s)`;
          break;
        }
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
    if (activeId != null) await closeTab(activeId);
  }
</script>

<svelte:window onkeydown={onKey} />

<header class="topbar">
  <span class="brand"><span class="logodot"></span> SCP Commander</span>
  {#if connected}
    <span class="hostpill">{activeLabel}</span>
  {/if}
  <span class="grow"></span>
  {#if connected}
    <button class="act" onclick={() => (showSync = true)}>Synchronize</button>
    <button class="act" onclick={compareDirs}>Compare</button>
    <button class="act" onclick={() => (showConsole = true)}>Console</button>
    <span class="tvsep"></span>
  {/if}
  <button class="act" class:on={prefs.show_hidden} onclick={toggleHidden} title="Show hidden files">Hidden</button>
  <button class="act" onclick={() => (showKnownHosts = true)} title="Trusted host keys">Hosts</button>
  <button class="act" onclick={() => (showPrefs = true)} title="Preferences">Preferences</button>
  <span class="tvsep"></span>
  {#if connected}
    <button class="act" onclick={disconnect}>Disconnect</button>
  {:else}
    <button class="act primary" onclick={() => (showLogin = true)}>Connect…</button>
  {/if}
</header>

{#if tabs.length}
  <div class="tabstrip">
    {#each tabs as t (t.id)}
      <div class="tab" class:active={t.id === activeId} onclick={() => switchTab(t.id)} role="button" tabindex="0">
        <span class="tdot"></span>
        <span class="tlabel">{t.label}</span>
        <button class="tclose" title="Close session" onclick={(e) => (e.stopPropagation(), closeTab(t.id))}>×</button>
      </div>
    {/each}
    <button class="tnew" title="New session" onclick={newSession}>＋</button>
  </div>
{/if}

<div class="panes">
  <Pane
    kind="local"
    title="Local"
    path={local.path}
    entries={local.entries}
    showHidden={prefs.show_hidden}
    focused={focusLocal}
    selected={localSel}
    transferLabel="Upload"
    canTransfer={connected}
    recents={localRecents}
    canBack={localNav.back.length > 0}
    canForward={localNav.fwd.length > 0}
    onFocus={() => (focusLocal = true)}
    onUp={localUp}
    onHome={goHomeLocal}
    onBack={() => goBack(true)}
    onForward={() => goForward(true)}
    onRefresh={() => loadLocal(local.path, false)}
    onNavigate={(p) => loadLocal(p)}
    onOpen={(e) => loadLocal(joinPath(local.path, e.name, "/"))}
    onTransferOne={(e) => transfer([e], true)}
    onTransfer={() => transferSelected(true)}
    onRowClick={(e, i, ev) => rowClick(true, e, i, ev)}
    onContext={(e, i, ev) => openContext(true, e, i, ev)}
    onNewFolder={() => (newFolder = { isLocal: true, value: "" })}
    onDelete={(entries) => requestDelete(true, entries)}
    onProperties={(e) => openProps(true, e)}
    onDragStartRow={(e) => onDragStartRow(true, e)}
    onDropPane={() => onDropPane(true)}
  />
  {#if connected}
    <Pane
      kind="remote"
      title="Remote"
      path={remote.path}
      entries={remote.entries}
      showHidden={prefs.show_hidden}
      showRights={true}
      showOwnerGroup={prefs.show_owner_group}
      focused={!focusLocal}
      {busy}
      selected={remoteSel}
      transferLabel="Download"
      canTransfer={connected}
      recents={remoteRecents}
      canBack={remoteNav.back.length > 0}
      canForward={remoteNav.fwd.length > 0}
      onFocus={() => (focusLocal = false)}
      onUp={remoteUp}
      onHome={goHomeRemote}
      onBack={() => goBack(false)}
      onForward={() => goForward(false)}
      onRefresh={() => loadRemote(remote.path, false)}
      onNavigate={(p) => loadRemote(p)}
      onOpen={(e) => loadRemote(joinPath(remote.path, e.name))}
      onTransferOne={(e) => transfer([e], false)}
      onTransfer={() => transferSelected(false)}
      onRowClick={(e, i, ev) => rowClick(false, e, i, ev)}
      onContext={(e, i, ev) => openContext(false, e, i, ev)}
      onNewFolder={() => (newFolder = { isLocal: false, value: "" })}
      onDelete={(entries) => requestDelete(false, entries)}
      onProperties={(e) => openProps(false, e)}
      onDragStartRow={(e) => onDragStartRow(false, e)}
      onDropPane={() => onDropPane(false)}
    />
  {:else}
    <div class="placeholder">
      <div>
        <p>Not connected.</p>
        <button class="act primary" onclick={() => (showLogin = true)}>Connect…</button>
      </div>
    </div>
  {/if}
</div>

<div class="statusbar">
  <span class="dot" class:on={connected}></span>
  <span class="stxt">{status}</span>
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
  <Modal title="Save site" z={60} onClose={() => (saveSiteName = null)}>
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
  <SyncDialog sessionId={activeId} localPath={local.path} remotePath={remote.path} onClose={() => (showSync = false)} />
{/if}

{#if showConsole}
  <ConsoleDialog sessionId={activeId} remotePath={remote.path} selection={remoteSel} onClose={() => (showConsole = false)} />
{/if}

{#if showKnownHosts}
  <KnownHostsDialog onClose={() => (showKnownHosts = false)} />
{/if}

{#if showPrefs}
  <PrefsDialog {prefs} onSave={savePrefs} onClose={() => (showPrefs = false)} />
{/if}

{#if dupTarget}
  <Modal title="Duplicate" onClose={() => (dupTarget = null)}>
    <form onsubmit={(e) => (e.preventDefault(), doDuplicate())}>
      <input class="dlg-input" bind:value={dupTarget.value} autofocus />
      <div class="dlg-actions">
        <button type="button" onclick={() => (dupTarget = null)}>Cancel</button>
        <button type="submit">Duplicate</button>
      </div>
    </form>
  </Modal>
{/if}

{#if viewer}
  <Modal title={viewer.name} onClose={() => (viewer = null)}>
    <pre class="viewer">{viewer.text}</pre>
    <div class="dlg-actions"><button onclick={() => (viewer = null)}>Close</button></div>
  </Modal>
{/if}

{#if showLogin}
  <Modal title="Connect to server" onClose={() => (showLogin = false)}>
    <form class="login" onsubmit={(e) => (e.preventDefault(), connect())}>
      <div class="lrow">
        <label>Protocol</label>
        <select bind:value={form.protocol} onchange={() => (form.port = defaultPort(form.protocol))}>
          {#each PROTOS as p}<option value={p}>{p.toUpperCase()}</option>{/each}
        </select>
        <span class="grow"></span>
        <select class="sitepick" bind:value={selectedSite} onchange={applySite} title="Load a saved site">
          <option value="">— Saved sites —</option>
          {#each sites as s}<option value={s.name}>{s.name}</option>{/each}
        </select>
      </div>

      <div class="lrow">
        <label>{isS3 ? "Endpoint" : "Host"}</label>
        <input class="grow" placeholder={isS3 ? "blank = AWS" : "host"} bind:value={form.host} />
        <label class="lbl2">Port</label>
        <input class="port" bind:value={form.port} />
      </div>

      <div class="lrow">
        <label>{isS3 ? "Access key" : "User"}</label>
        <input class="grow" bind:value={form.username} />
        {#if isSftp}
          <select bind:value={form.auth_mode}>
            <option value="password">Password</option>
            <option value="key">Key file</option>
            <option value="agent">Agent</option>
          </select>
        {/if}
      </div>

      {#if form.auth_mode === "key" && isSftp}
        <div class="lrow"><label>Private key</label><input class="grow" placeholder="~/.ssh/id_ed25519" bind:value={form.key_path} /></div>
        <div class="lrow"><label>Passphrase</label><input class="grow" type="password" bind:value={form.password} /></div>
      {:else if !(isSftp && form.auth_mode === "agent")}
        <div class="lrow"><label>{isS3 ? "Secret key" : "Password"}</label><input class="grow" type="password" bind:value={form.password} /></div>
      {/if}

      {#if isS3}
        <div class="lrow">
          <label>Bucket</label><input class="grow" bind:value={form.bucket} />
          <label class="lbl2">Region</label><input class="port wide" placeholder="us-east-1" bind:value={form.region} />
        </div>
      {/if}

      {#if isSftp}
        <label class="jrow"><input type="checkbox" bind:checked={form.use_jump} /> Connect through a jump host (bastion)</label>
        {#if form.use_jump}
          <div class="lrow">
            <label>Jump host</label><input class="grow" bind:value={form.jump_host} />
            <label class="lbl2">Port</label><input class="port" bind:value={form.jump_port} />
          </div>
          <div class="lrow">
            <label>Jump user</label><input class="grow" bind:value={form.jump_user} />
            <select bind:value={form.jump_auth_mode}>
              <option value="password">Password</option>
              <option value="key">Key file</option>
              <option value="agent">Agent</option>
            </select>
          </div>
          {#if form.jump_auth_mode === "key"}
            <div class="lrow"><label>Jump key</label><input class="grow" placeholder="~/.ssh/id_ed25519" bind:value={form.jump_key_path} /></div>
          {:else if form.jump_auth_mode !== "agent"}
            <div class="lrow"><label>Jump password</label><input class="grow" type="password" bind:value={form.jump_password} /></div>
          {/if}
        {/if}
      {/if}

      {#if form.protocol === "ftp"}
        <p class="warn">⚠ Plain FTP sends credentials and data unencrypted — prefer SFTP or FTPS.</p>
      {/if}

      {#if hostKey}
        <div class="hostkey" class:mismatch={hostKey.mismatch}>
          {#if hostKey.mismatch}
            ⚠ Host key <code>{hostKey.fingerprint}</code> contradicts the stored one — connection refused.
          {:else}
            Unknown server key: <code>{hostKey.fingerprint}</code>
            <button type="button" class="primary" onclick={() => connect(hostKey.fingerprint)}>Trust &amp; Connect</button>
          {/if}
        </div>
      {/if}

      <div class="lactions">
        <button type="button" class="ghost" disabled={!form.host && !form.bucket} onclick={() => (saveSiteName = form.host || form.bucket)}>Save site…</button>
        <button type="button" class="ghost" disabled={!selectedSite} onclick={deleteSite}>Delete site</button>
        <span class="grow"></span>
        <button type="button" onclick={() => (showLogin = false)}>Close</button>
        <button type="submit" class="primary" disabled={busy}>{busy ? "Connecting…" : "Connect"}</button>
      </div>
    </form>
  </Modal>
{/if}

<style>
  /* Top bar */
  .topbar {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 7px 10px;
    background: var(--panel);
    border-bottom: 1px solid var(--border);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 7px;
    font-weight: 600;
    font-size: 13px;
  }
  .logodot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--accent);
  }
  .hostpill {
    font-family: var(--mono);
    font-size: 11.5px;
    color: var(--text-2);
    background: var(--panel-2);
    border: 1px solid var(--border);
    border-radius: 20px;
    padding: 2px 9px;
  }
  .grow {
    flex: 1;
  }
  .act {
    font-size: 12px;
    padding: 4px 10px;
    border-radius: 6px;
    color: var(--text);
  }
  .act.on {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--border));
    background: var(--accent-bg);
  }
  .act.primary {
    color: #fff;
    background: var(--accent);
    border-color: var(--accent);
  }
  .act.primary:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 88%, #000);
  }
  .tvsep {
    width: 1px;
    height: 18px;
    background: var(--border);
    margin: 0 3px;
  }

  /* Tab strip */
  .tabstrip {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 5px 10px 0;
    overflow-x: auto;
  }
  .tab {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 8px 4px 10px;
    border: 1px solid var(--border);
    border-bottom: none;
    border-radius: 7px 7px 0 0;
    background: var(--panel-2);
    color: var(--text-2);
    font-size: 12px;
    cursor: pointer;
    max-width: 220px;
    white-space: nowrap;
  }
  .tab.active {
    background: var(--panel);
    color: var(--text);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--border));
  }
  .tab .tdot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--ok);
    flex: none;
  }
  .tab .tlabel {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tclose {
    border: none;
    background: transparent;
    color: var(--text-3);
    padding: 0 2px;
    font-size: 13px;
    line-height: 1;
    border-radius: 4px;
  }
  .tclose:hover {
    background: var(--hover);
    color: var(--danger);
  }
  .tnew {
    border: none;
    background: transparent;
    color: var(--text-2);
    font-size: 14px;
    padding: 2px 8px;
    border-radius: 6px;
  }

  /* Panes */
  .panes {
    display: flex;
    gap: 8px;
    padding: 8px 10px;
    flex: 1;
    min-height: 0;
  }
  .placeholder {
    flex: 1 1 0;
    min-width: 0;
    display: grid;
    place-items: center;
    border: 1px dashed var(--border);
    border-radius: 8px;
    color: var(--text-2);
    font-size: 13px;
    text-align: center;
  }
  .placeholder p {
    margin: 0 0 10px;
  }

  /* Status bar */
  .statusbar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 12px;
    border-top: 1px solid var(--border);
    background: var(--panel);
    font-size: 12px;
    color: var(--text-2);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-3);
    flex: none;
  }
  .dot.on {
    background: var(--ok);
  }
  .stxt {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Login modal */
  .login {
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: 460px;
    max-width: 84vw;
  }
  .lrow {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .lrow > label {
    width: 86px;
    flex: none;
    font-size: 12px;
    color: var(--text-2);
  }
  .lrow .lbl2 {
    width: auto;
  }
  .lrow .grow {
    min-width: 0;
  }
  .lrow .port {
    width: 64px;
    flex: none;
  }
  .lrow .port.wide {
    width: 96px;
  }
  .sitepick {
    max-width: 180px;
  }
  .jrow {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12.5px;
    margin-top: 2px;
  }
  .warn {
    font-size: 12px;
    color: var(--warn);
    margin: 2px 0;
  }
  .lactions {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 6px;
    padding-top: 10px;
    border-top: 1px solid var(--border);
  }
  .lactions button {
    padding: 5px 12px;
  }
  .ghost {
    color: var(--text-2);
    font-size: 12px;
  }
  button.primary {
    color: #fff;
    background: var(--accent);
    border-color: var(--accent);
  }
  button.primary:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 88%, #000);
  }
  .hostkey {
    padding: 8px 10px;
    border-radius: 6px;
    background: color-mix(in srgb, var(--warn) 18%, var(--panel));
    font-size: 12.5px;
  }
  .hostkey.mismatch {
    background: color-mix(in srgb, var(--danger) 20%, var(--panel));
  }
  .hostkey code {
    font-family: var(--mono);
  }

  /* Shared dialog bits */
  .dlg-input {
    width: 100%;
    padding: 6px 8px;
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
    border-color: var(--danger);
    color: var(--danger);
  }
  .props {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 16px;
    font-size: 13px;
    margin-bottom: 12px;
  }
  .props span:nth-child(odd) {
    color: var(--text-2);
  }
  .mono {
    font-family: var(--mono);
  }
  .chmod {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 12px;
    font-size: 13px;
  }
  .hint {
    font-size: 12px;
    color: var(--text-2);
    margin: 0 0 12px;
  }
  .viewer {
    font-family: var(--mono);
    font-size: 12px;
    white-space: pre;
    overflow: auto;
    max-height: 60vh;
    max-width: 78vw;
    min-width: 380px;
    margin: 0 0 12px;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--panel-2);
  }
</style>
