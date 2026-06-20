<script>
  import { invoke, listen, emit, joinPath, humanSize, updateProgress } from "./lib/api.js";
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
  let saveSitePw = $state(false); // "save password in keychain" checkbox
  async function reloadSites() {
    sites = await invoke("list_sites");
  }
  $effect(() => {
    reloadSites();
  });
  // Load a site into the form; fetches the keychain password if it has one.
  async function loadSiteForm(s) {
    selectedSite = s.name;
    let pw = "";
    if (s.save_password) {
      try { pw = (await invoke("secret_get", { account: s.name })) ?? ""; } catch {}
    }
    form = {
      ...form,
      protocol: s.protocol,
      host: s.host,
      port: s.port,
      username: s.username,
      password: pw,
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
    status = `Loaded site “${s.name}”`;
  }
  function applySiteByName(name) {
    const s = sites.find((x) => x.name === name);
    if (s) loadSiteForm(s);
  }
  async function connectSite(s) {
    await loadSiteForm(s);
    connect();
  }
  async function saveSite() {
    const name = saveSiteName.trim();
    const savePw = saveSitePw;
    saveSiteName = null;
    if (!name) return;
    const { password, jump_password, ...rest } = form;
    const site = {
      ...rest,
      name,
      port: Number(form.port) || 22, // inputs yield strings; the backend wants u16
      jump_port: Number(form.jump_port) || 22,
      save_password: savePw && !!password,
    };
    try {
      await invoke("save_site", { site });
      if (savePw && password) await invoke("secret_set", { account: name, password });
      else await invoke("secret_delete", { account: name }).catch(() => {});
      await reloadSites();
      selectedSite = name;
      status = `Saved site “${name}”`;
    } catch (e) {
      status = `Save site failed: ${e}`;
    }
  }
  async function deleteSiteByName(name) {
    await invoke("delete_site", { name });
    invoke("secret_delete", { account: name }).catch(() => {});
    if (selectedSite === name) selectedSite = "";
    await reloadSites();
  }
  function deleteSite() {
    if (selectedSite) deleteSiteByName(selectedSite);
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
  let syncBrowse = $state(false); // mirror cd between panes when on
  let showHelp = $state(false); // keyboard-shortcuts help modal
  let findState = $state(null); // { mask, results, busy } — Find dialog

  let status = $state("Not connected");
  let busy = $state(false);
  let hostKey = $state(null);

  // Sessions / tabs. Each tab is one backend session (id) with its own remote
  // pane state; the local pane is shared across all tabs.
  let tabs = $state([]); // [{ id, label }]
  let activeId = $state(null); // active session id, or null when none open
  const connected = $derived(activeId !== null);
  const activeLabel = $derived(tabs.find((t) => t.id === activeId)?.label ?? "");
  const activeConn = $derived(tabs.find((t) => t.id === activeId) ?? null);
  const tabRemote = {}; // id -> { remote, remoteSel, remoteNav, remoteRecents, remoteHome }

  let local = $state({ path: "", entries: [] });
  let remote = $state({ path: "", entries: [] });
  let localSel = $state([]);
  let remoteSel = $state([]);
  let localAnchor = -1;
  let remoteAnchor = -1;
  let localCursor = -1; // keyboard "current row" index into the visible order
  let remoteCursor = -1;
  let localView = []; // visible row names (from Pane, after sort/filter)
  let remoteView = [];
  let pathFocusReq = $state(0); // bumped on ⌘L to focus the active pane's path bar
  let maskSelect = $state(null); // { add } — the select/deselect-by-mask dialog
  let maskValue = $state("*");

  // Per-pane navigation history + recent locations.
  let localNav = $state({ back: [], fwd: [] });
  let remoteNav = $state({ back: [], fwd: [] });
  let localRecents = $state([]);
  let remoteRecents = $state([]);
  let remoteHome = $state("/");
  let showLogin = $state(true);

  let queue = $state([]);
  const activeXfers = $derived(queue.filter((t) => t.state === "active"));
  // Name of a just-completed file to briefly highlight in its destination pane,
  // so a finished transfer is visibly "landed" there (cleared after the glow).
  let localFlash = $state(null);
  let remoteFlash = $state(null);
  const flashTimers = {};
  function flashArrived(isLocal, name) {
    if (isLocal) localFlash = name;
    else remoteFlash = name;
    const k = isLocal ? "l" : "r";
    clearTimeout(flashTimers[k]);
    flashTimers[k] = setTimeout(() => {
      if (isLocal) localFlash = null;
      else remoteFlash = null;
    }, 2400);
  }

  // In-progress placeholders: a file being transferred shows up in its
  // destination pane right away as a dimmed "ghost" row (with live progress),
  // until the real listing refresh replaces it on completion. Only shown when
  // the pane is actually displaying the transfer's destination directory.
  function parentOf(p) {
    const s = String(p).replace(/[/\\]+$/, "");
    const i = Math.max(s.lastIndexOf("/"), s.lastIndexOf("\\"));
    return i <= 0 ? "/" : s.slice(0, i);
  }
  const stripTrail = (p) => String(p).replace(/[/\\]+$/, "") || "/";
  function pendingFor(upload) {
    const here = stripTrail(upload ? remote.path : local.path);
    const dest = upload ? remote : local;
    const have = new Set((dest.entries ?? []).map((e) => e.name));
    return queue
      .filter(
        (t) =>
          t.state === "active" &&
          t.upload === upload &&
          (upload ? t.session === activeId : true) &&
          stripTrail(parentOf(upload ? t.remote : t.local)) === here &&
          !have.has(t.name),
      )
      .map((t) => ({ name: t.name, is_dir: t.is_dir, upload, done: t.done, total: t.total, pending: true }));
  }
  const remotePending = $derived(connected ? pendingFor(true) : []);
  const localPending = $derived(pendingFor(false));
  const xferPct = $derived.by(() => {
    const done = activeXfers.reduce((s, t) => s + t.done, 0);
    const total = activeXfers.reduce((s, t) => s + t.total, 0);
    return total > 0 ? Math.round((done / total) * 100) : 0;
  });

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

  // --- navigation with optional synchronized browsing (mirror cd) ---
  function navOpenLocal(e) {
    loadLocal(joinPath(local.path, e.name, "/"));
    if (syncBrowse && connected && remote.entries.some((r) => r.name === e.name && r.is_dir))
      loadRemote(joinPath(remote.path, e.name));
  }
  function navOpenRemote(e) {
    loadRemote(joinPath(remote.path, e.name));
    if (syncBrowse && local.entries.some((r) => r.name === e.name && r.is_dir))
      loadLocal(joinPath(local.path, e.name, "/"));
  }
  function navUpLocal() {
    localUp();
    if (syncBrowse && connected) remoteUp();
  }
  function navUpRemote() {
    remoteUp();
    if (syncBrowse) localUp();
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
  // The separate Transfers window asks for the current queue when it opens.
  $effect(() => {
    const un = listen("request-xfer-snapshot", () =>
      emit("xfer-snapshot", queue.map((t) => ({ ...t }))),
    );
    return () => un.then((f) => f());
  });
  // Auto-upload results from the remote-file editor.
  $effect(() => {
    const un = listen("edit", (e) => {
      const p = e.payload;
      status = p.ok ? `Uploaded changes to ${p.name}` : `Edit upload failed (${p.name}): ${p.message}`;
    });
    return () => un.then((f) => f());
  });

  // Reflect the active connection in the OS window title.
  $effect(() => {
    const t = activeLabel ? `${activeLabel} — SCP Commander` : "SCP Commander";
    try {
      window.__TAURI__?.window?.getCurrentWindow?.().setTitle(t);
    } catch {}
  });

  // Notify when the queue drains (status line + best-effort desktop notification).
  let prevActive = 0;
  $effect(() => {
    const n = activeXfers.length;
    if (prevActive > 0 && n === 0) {
      status = "✓ All transfers complete";
      try {
        if (typeof Notification !== "undefined" && Notification.permission === "granted")
          new Notification("SCP Commander", { body: "All transfers complete" });
      } catch {}
    }
    prevActive = n;
  });

  // OS file drop (from Finder) → upload to the active remote directory.
  $effect(() => {
    const un = listen("tauri://drag-drop", (e) => {
      const paths = e.payload?.paths ?? [];
      if (!connected || !paths.length) return;
      // Only upload when dropped on the remote pane (position is physical px).
      const pos = e.payload?.position;
      if (pos) {
        const dpr = window.devicePixelRatio || 1;
        const el = document.elementFromPoint(pos.x / dpr, pos.y / dpr);
        if (el?.closest("[data-kind]")?.dataset.kind !== "remote") return;
      }
      uploadExternal(paths);
    });
    return () => un.then((f) => f());
  });
  async function uploadExternal(paths) {
    const sess = activeId;
    for (const p of paths) {
      const name = p.replace(/[/\\]+$/, "").split(/[/\\]/).pop();
      if (!name) continue;
      let isDir = false;
      try { isDir = await invoke("local_is_dir", { path: p }); } catch {}
      const remotePath = joinPath(remote.path, name);
      invoke("enqueue", {
        sessionId: sess,
        upload: true,
        isDir,
        name,
        local: p,
        remote: remotePath,
        overwrite: 0,
      })
        .then((id) => {
          if (!queue.find((t) => t.id === id && t.session === sess)) {
            queue.push({
              id, session: sess, name, upload: true,
              done: 0, total: 0, state: "active",
              local: p, remote: remotePath, is_dir: isDir, overwrite: 0,
              speed: 0, eta: null, lastAt: null, lastDone: 0,
            });
          }
        })
        .catch((err) => (status = `Could not start upload of ${name}: ${err}`));
    }
    status = `Uploading ${paths.length} dropped item(s)…`;
  }
  function onXfer(p) {
    // Transfer ids are per-session, so match on both.
    const t = queue.find((x) => x.id === p.id && x.session === p.session);
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
      case "done": {
        if (t) { t.state = "done"; t.done = t.total || t.done; }
        // Refresh the affected pane, then flash the arrived file (flash AFTER
        // the listing lands so the highlight lines up with the new row, and
        // the pane scrolls it into view). Remote refresh only for the active
        // session; local is shared so always refresh on a download.
        if (p.upload) {
          if (p.session === activeId)
            loadRemote(remote.path, false).then(() => flashArrived(false, p.name));
        } else {
          loadLocal(local.path, false).then(() => flashArrived(true, p.name));
        }
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
    try {
      if (typeof Notification !== "undefined" && Notification.permission === "default")
        Notification.requestPermission();
    } catch {}
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

  // --- selection / cursor helpers (operate on the pane's real visible order) ---
  const paneView = (isLocal) => (isLocal ? localView : remoteView);
  function setSel(isLocal, names) {
    if (isLocal) localSel = names;
    else remoteSel = names;
  }
  function setAnchor(isLocal, i) {
    if (isLocal) { localAnchor = i; localCursor = i; }
    else { remoteAnchor = i; remoteCursor = i; }
  }
  function curCursor(isLocal) {
    const c = isLocal ? localCursor : remoteCursor;
    if (c >= 0) return c;
    const view = paneView(isLocal);
    const sel = isLocal ? localSel : remoteSel;
    return sel.length ? view.indexOf(sel[sel.length - 1]) : -1;
  }
  // Move the keyboard cursor to row `target` (clamped); extend the range with Shift.
  function navTo(isLocal, target, extend) {
    const view = paneView(isLocal);
    if (!view.length) return;
    const idx = Math.max(0, Math.min(view.length - 1, target));
    if (extend) {
      const a = isLocal ? localAnchor : remoteAnchor;
      const base = a < 0 ? idx : a;
      if (isLocal) localAnchor = base; else remoteAnchor = base;
      setSel(isLocal, view.slice(Math.min(base, idx), Math.max(base, idx) + 1));
      if (isLocal) localCursor = idx; else remoteCursor = idx;
    } else {
      setSel(isLocal, [view[idx]]);
      setAnchor(isLocal, idx);
    }
  }
  function rowClick(isLocal, entry, index, ev) {
    const view = paneView(isLocal);
    let sel = isLocal ? localSel : remoteSel;
    if (ev.metaKey || ev.ctrlKey) {
      sel = sel.includes(entry.name) ? sel.filter((n) => n !== entry.name) : [...sel, entry.name];
      setAnchor(isLocal, index);
    } else if (ev.shiftKey) {
      const anchor = isLocal ? localAnchor : remoteAnchor;
      const [a, b] = anchor < 0 ? [index, index] : [Math.min(anchor, index), Math.max(anchor, index)];
      sel = view.slice(a, b + 1);
      if (isLocal) localCursor = index; else remoteCursor = index;
    } else {
      sel = [entry.name];
      setAnchor(isLocal, index);
    }
    setSel(isLocal, sel);
  }
  // Norton-style select/deselect by glob mask (* and ?).
  function maskToRegex(mask) {
    const esc = mask.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".");
    return new RegExp(`^${esc}$`, "i");
  }
  function applyMask(value) {
    const add = maskSelect.add;
    maskSelect = null;
    const v = value.trim();
    if (!v) return;
    const re = maskToRegex(v);
    const isLocal = focusLocal;
    const view = paneView(isLocal);
    const cur = new Set(isLocal ? localSel : remoteSel);
    for (const n of view) {
      if (re.test(n)) { if (add) cur.add(n); else cur.delete(n); }
    }
    setSel(isLocal, view.filter((n) => cur.has(n)));
  }

  // --- transfers (with overwrite prompt) ---
  // Transfers whose source should be deleted on success (F6 move).
  const pendingMove = new Map(); // `${session}:${id}` -> { isLocal, path, is_dir }
  // `destDir` overrides the destination directory (e.g. dropping onto a folder).
  function enqueueEntry(e, upload, policy, move = false, destDir = null) {
    const localBase = upload ? local.path : destDir ?? local.path;
    const remoteBase = upload ? destDir ?? remote.path : remote.path;
    const localPath = joinPath(localBase, e.name, "/");
    const remotePath = joinPath(remoteBase, e.name);
    const sess = activeId;
    const p = invoke("enqueue", {
      sessionId: sess,
      upload,
      isDir: e.is_dir,
      name: e.name,
      local: localPath,
      remote: remotePath,
      overwrite: policy,
    });
    p.then((id) => {
      // Show the item in the queue the moment it's accepted — don't wait for the
      // worker's first "started" event — so a drop gives instant feedback. The
      // backend events reconcile this same row by id (started only pushes if
      // absent; progress/done update it in place).
      if (!queue.find((t) => t.id === id && t.session === sess)) {
        queue.push({
          id, session: sess, name: e.name, upload,
          done: 0, total: e.is_dir ? 0 : e.size || 0, state: "active",
          local: localPath, remote: remotePath, is_dir: e.is_dir, overwrite: policy,
          speed: 0, eta: null, lastAt: null, lastDone: 0,
        });
      }
      if (move) {
        pendingMove.set(`${sess}:${id}`, {
          isLocal: upload, // source side: upload=local→remote, so source is local
          path: upload ? localPath : remotePath,
          is_dir: e.is_dir,
        });
      }
    }).catch((err) => {
      // Surface a silent enqueue failure instead of swallowing it.
      status = `Could not start ${upload ? "upload" : "download"} of ${e.name}: ${err}`;
    });
    return p;
  }
  function transfer(entries, upload, move = false, destDir = null) {
    if (!connected || !entries.length) return;
    // Skip the collision prompt when dropping into a subfolder we haven't listed.
    if (destDir == null) {
      const dest = upload ? remote : local;
      const destNames = new Set(dest.entries.map((e) => e.name));
      const collisions = entries.filter((e) => destNames.has(e.name));
      if (collisions.length && prefs.confirm_overwrite) {
        const byName = new Map(dest.entries.map((e) => [e.name, e]));
        const details = collisions
          .filter((e) => !e.is_dir)
          .map((e) => {
            const d = byName.get(e.name);
            return { name: e.name, srcSize: e.size, srcMtime: e.mtime, dstSize: d.size, dstMtime: d.mtime };
          });
        overwrite = { entries, upload, move, count: collisions.length, details };
        return;
      }
    }
    for (const e of entries) enqueueEntry(e, upload, 0, move, destDir);
    status = `${upload ? "Uploading" : "Downloading"} ${entries.length} item${entries.length === 1 ? "" : "s"}…`;
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
      !isLocal && !entry.is_dir && { label: "Edit (F4)", action: () => editFile(entry) },
      { label: "Rename… (F2)", action: () => (renameTarget = { isLocal, entry, value: entry.name }) },
      !isLocal && !entry.is_dir && {
        label: "Duplicate…",
        action: () => (dupTarget = { entry, value: entry.name }),
      },
      !isLocal && entry.is_dir && { label: "Calculate size", action: () => calcSize(entry) },
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
  function openContextEmpty(isLocal, ev) {
    const items = [
      { label: "Refresh", action: () => refresh(isLocal) },
      { label: "New folder…", action: () => (newFolder = { isLocal, value: "" }) },
      { label: "Select all", action: () => setSel(isLocal, [...paneView(isLocal)]) },
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
  function removeTransfer(item) {
    queue = queue.filter((x) => !(x.session === item.session && x.id === item.id));
  }
  function retryTransfer(item) {
    invoke("enqueue", {
      sessionId: item.session,
      upload: item.upload,
      isDir: item.is_dir,
      name: item.name,
      local: item.local,
      remote: item.remote,
      overwrite: item.overwrite ?? 0,
      resume: true, // continue the partial we already started
    }).catch((e) => (status = `Retry failed: ${e}`));
    removeTransfer(item);
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
      dupTarget || viewer || maskSelect || findState || showLogin || showSync || showConsole ||
      showKnownHosts || showPrefs || showHelp
    );
  }
  function onKey(ev) {
    if (anyModalOpen()) return;
    if (["INPUT", "SELECT", "TEXTAREA"].includes(document.activeElement?.tagName)) return;
    const isLocal = focusLocal;
    const remoteOk = isLocal || connected;
    // ⌘/Ctrl shortcuts: select-all, invert, focus path bar.
    if ((ev.metaKey || ev.ctrlKey) && !ev.altKey) {
      const k = ev.key.toLowerCase();
      if (k === "a") {
        ev.preventDefault();
        const v = paneView(isLocal);
        setSel(isLocal, [...v]);
        if (v.length) { if (isLocal) { localAnchor = 0; localCursor = v.length - 1; } else { remoteAnchor = 0; remoteCursor = v.length - 1; } }
        return;
      }
      if (k === "i") {
        ev.preventDefault();
        const v = paneView(isLocal);
        const cur = new Set(isLocal ? localSel : remoteSel);
        setSel(isLocal, v.filter((n) => !cur.has(n)));
        return;
      }
      if (k === "l") { ev.preventDefault(); pathFocusReq++; return; }
    }
    switch (ev.key) {
      case "Tab":
        ev.preventDefault();
        focusLocal = !focusLocal;
        return;
      case "ArrowDown": { ev.preventDefault(); const c = curCursor(isLocal); navTo(isLocal, c < 0 ? 0 : c + 1, ev.shiftKey); return; }
      case "ArrowUp": { ev.preventDefault(); const c = curCursor(isLocal); navTo(isLocal, c < 0 ? 0 : c - 1, ev.shiftKey); return; }
      case "Home": ev.preventDefault(); navTo(isLocal, 0, ev.shiftKey); return;
      case "End": ev.preventDefault(); navTo(isLocal, paneView(isLocal).length - 1, ev.shiftKey); return;
      case "PageDown": { ev.preventDefault(); const c = curCursor(isLocal); navTo(isLocal, (c < 0 ? 0 : c) + 14, ev.shiftKey); return; }
      case "PageUp": { ev.preventDefault(); const c = curCursor(isLocal); navTo(isLocal, (c < 0 ? 0 : c) - 14, ev.shiftKey); return; }
      case " ": {
        ev.preventDefault();
        const v = paneView(isLocal);
        const c = curCursor(isLocal);
        if (c >= 0 && v[c]) {
          const sel = isLocal ? localSel : remoteSel;
          const n = v[c];
          setSel(isLocal, sel.includes(n) ? sel.filter((x) => x !== n) : [...sel, n]);
        }
        return;
      }
      case "+": ev.preventDefault(); maskSelect = { add: true }; return;
      case "-": ev.preventDefault(); maskSelect = { add: false }; return;
      case "F5":
        if (connected) { ev.preventDefault(); transferSelected(isLocal); }
        return;
      case "F6":
        if (connected) { ev.preventDefault(); moveSelected(isLocal); }
        return;
      case "F4": {
        ev.preventDefault();
        if (!isLocal && connected) {
          const e = selectedEntriesIn(false)[0];
          if (e && !e.is_dir) editFile(e);
        }
        return;
      }
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
      const view = paneView(isLocal);
      const idx = view.findIndex((n) => n.toLowerCase().startsWith(typeAhead));
      if (idx >= 0) { setSel(isLocal, [view[idx]]); setAnchor(isLocal, idx); }
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

  // Edit a remote file: opens in the OS editor; saves auto-upload (F4).
  async function editFile(entry) {
    if (entry.is_dir) return;
    try {
      await invoke("edit_remote", { sessionId: activeId, path: fullPath(false, entry.name) });
      status = `Editing ${entry.name} — saves upload automatically`;
    } catch (e) {
      status = `Edit failed: ${e}`;
    }
  }

  // --- Find files (remote mask search) ---
  function openFind() {
    findState = { mask: "*", results: null, busy: false };
  }
  async function runFind() {
    if (!connected || !findState) return;
    findState.busy = true;
    try {
      findState.results = await invoke("find_remote", {
        sessionId: activeId,
        base: remote.path,
        mask: findState.mask || "*",
      });
    } catch (e) {
      status = `Find failed: ${e}`;
      findState.results = [];
    } finally {
      findState.busy = false;
    }
  }
  function openFindHit(hit) {
    const dir = hit.path.replace(/\/[^/]*$/, "") || "/";
    findState = null;
    loadRemote(dir);
  }

  // Calculate a remote directory's total size.
  async function calcSize(entry) {
    status = `Calculating size of ${entry.name}…`;
    try {
      const bytes = await invoke("dir_size", { sessionId: activeId, path: fullPath(false, entry.name) });
      status = `${entry.name}: ${humanSize(bytes)}`;
    } catch (e) {
      status = `Size failed: ${e}`;
    }
  }
  function openTerminal() {
    if (activeConn?.proto !== "sftp") return;
    invoke("open_ssh_terminal", { host: activeConn.host, port: activeConn.port, user: activeConn.user })
      .catch((e) => (status = `Terminal failed: ${e}`));
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

  // --- drag and drop between panes (pointer-based; HTML5 DnD is unreliable in
  // the WebKit webview, so we track pointer movement ourselves) ---
  let dragGhost = $state(null); // { x, y, label } while dragging
  let dropTarget = $state(null); // { kind, name } highlight while hovering a valid target
  let dragState = null; // { fromLocal, entries, startX, startY, active }
  // Native-feeling cursor while dragging: "grabbing" everywhere, "copy" (the
  // green +) when hovering a valid drop target. Toggled on <body> so it wins
  // over per-element cursors during the gesture.
  $effect(() => {
    const active = !!dragGhost;
    document.body.classList.toggle("dragging", active);
    document.body.classList.toggle("drop-ok", active && !!dropTarget);
  });
  function onRowPointerDown(isLocal, entry, ev) {
    if (ev.button !== 0) return;
    const sel = isLocal ? localSel : remoteSel;
    const entries = (isLocal ? local.entries : remote.entries).filter((e) => sel.includes(e.name));
    dragState = {
      fromLocal: isLocal,
      entries: entries.length && sel.includes(entry.name) ? entries : [entry],
      startX: ev.clientX,
      startY: ev.clientY,
      active: false,
    };
    window.addEventListener("pointermove", onDragMove);
    window.addEventListener("pointerup", onDragUp);
  }
  // Which pane (+ optional folder row) is under the cursor, if it's a valid drop.
  function dropInfoAt(ds, x, y) {
    const elt = document.elementFromPoint(x, y);
    const kind = elt?.closest("[data-kind]")?.dataset.kind;
    if (kind !== "local" && kind !== "remote") return null;
    if ((kind === "remote") !== ds.fromLocal) return null; // same pane it came from
    const rowName = elt?.closest("tr[data-name]")?.dataset.name;
    const entries = kind === "remote" ? remote.entries : local.entries;
    const onFolder = rowName && entries.find((e) => e.name === rowName)?.is_dir ? rowName : null;
    return { kind, name: onFolder };
  }
  function onDragMove(ev) {
    if (!dragState) return;
    if (!dragState.active) {
      const dx = ev.clientX - dragState.startX;
      const dy = ev.clientY - dragState.startY;
      if (dx * dx + dy * dy < 36) return; // 6px threshold before a drag begins
      dragState.active = true;
      // Drop any text-selection that may have formed before the drag engaged,
      // so the drag gesture stays clean and the drop hit-test is unambiguous.
      try { window.getSelection()?.removeAllRanges(); } catch {}
    }
    const n = dragState.entries.length;
    dragGhost = { x: ev.clientX, y: ev.clientY, label: n > 1 ? `${n} items` : dragState.entries[0].name };
    dropTarget = dropInfoAt(dragState, ev.clientX, ev.clientY);
  }
  function onDragUp(ev) {
    window.removeEventListener("pointermove", onDragMove);
    window.removeEventListener("pointerup", onDragUp);
    const ds = dragState;
    dragState = null;
    dragGhost = null;
    dropTarget = null;
    if (!ds || !ds.active || !connected) return;
    const info = dropInfoAt(ds, ev.clientX, ev.clientY);
    if (!info) return;
    const toRemote = info.kind === "remote";
    // Dropped on a folder row → drop into that folder; else the current dir.
    const destDir = info.name
      ? joinPath(toRemote ? remote.path : local.path, info.name)
      : null;
    transfer(ds.entries, toRemote, false, destDir);
    if (destDir) status = `${ds.entries.length} item(s) → ${info.name}/`;
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
  // Resolve a host-key mismatch: forget the stored key, then trust the new one.
  async function forgetHostKey() {
    try {
      await invoke("known_hosts_remove", { host: form.host });
      const fp = hostKey?.fingerprint;
      hostKey = null;
      connect(fp);
    } catch (e) {
      status = `Could not update host key: ${e}`;
    }
  }
</script>

<svelte:window onkeydown={onKey} />

{#if dragGhost}
  <div class="drag-ghost" style="left:{dragGhost.x + 14}px; top:{dragGhost.y + 10}px">
    {dragGhost.label}
  </div>
{/if}

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
    {#if activeConn?.proto === "sftp"}
      <button class="act" onclick={openTerminal} title="Open an SSH session in Terminal">Terminal</button>
    {/if}
    <button class="act" onclick={openFind} title="Find remote files by mask">Find</button>
    <button class="act" class:on={syncBrowse} onclick={() => (syncBrowse = !syncBrowse)} title="Synchronized browsing: mirror folder changes between panes">Sync browse</button>
    <button class="act" onclick={() => invoke("open_transfers_window")} title="Open transfers in a separate window">Transfers ⤢</button>
    <span class="tvsep"></span>
  {/if}
  <button class="act" class:on={prefs.show_hidden} onclick={toggleHidden} title="Show hidden files">Hidden</button>
  <button class="act" onclick={() => (showKnownHosts = true)} title="Trusted host keys">Hosts</button>
  <button class="act" onclick={() => (showPrefs = true)} title="Preferences">Preferences</button>
  <button class="act" onclick={() => (showHelp = true)} title="Keyboard shortcuts">?</button>
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
    onUp={navUpLocal}
    onHome={goHomeLocal}
    onBack={() => goBack(true)}
    onForward={() => goForward(true)}
    onRefresh={() => loadLocal(local.path, false)}
    onNavigate={(p) => loadLocal(p)}
    onOpen={navOpenLocal}
    onTransferOne={(e) => transfer([e], true)}
    onTransfer={() => transferSelected(true)}
    onRowClick={(e, i, ev) => rowClick(true, e, i, ev)}
    onContext={(e, i, ev) => openContext(true, e, i, ev)}
    onNewFolder={() => (newFolder = { isLocal: true, value: "" })}
    onDelete={(entries) => requestDelete(true, entries)}
    onProperties={(e) => openProps(true, e)}
    onRowPointerDown={(e, ev) => onRowPointerDown(true, e, ev)}
    dropActive={dropTarget?.kind === "local"}
    dropName={dropTarget?.kind === "local" ? dropTarget.name : null}
    flashName={localFlash}
    pending={localPending}
    onView={(names) => (localView = names)}
    focusPathReq={focusLocal ? pathFocusReq : 0}
    onContextEmpty={(ev) => openContextEmpty(true, ev)}
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
      onUp={navUpRemote}
      onHome={goHomeRemote}
      onBack={() => goBack(false)}
      onForward={() => goForward(false)}
      onRefresh={() => loadRemote(remote.path, false)}
      onNavigate={(p) => loadRemote(p)}
      onOpen={navOpenRemote}
      onTransferOne={(e) => transfer([e], false)}
      onTransfer={() => transferSelected(false)}
      onRowClick={(e, i, ev) => rowClick(false, e, i, ev)}
      onContext={(e, i, ev) => openContext(false, e, i, ev)}
      onNewFolder={() => (newFolder = { isLocal: false, value: "" })}
      onDelete={(entries) => requestDelete(false, entries)}
      onProperties={(e) => openProps(false, e)}
      onRowPointerDown={(e, ev) => onRowPointerDown(false, e, ev)}
      dropActive={dropTarget?.kind === "remote"}
      dropName={dropTarget?.kind === "remote" ? dropTarget.name : null}
      flashName={remoteFlash}
      pending={remotePending}
      onView={(names) => (remoteView = names)}
      focusPathReq={!focusLocal ? pathFocusReq : 0}
      onContextEmpty={(ev) => openContextEmpty(false, ev)}
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

<TransferQueue {queue} onCancel={cancelTransfer} onClear={clearFinished} onRetry={retryTransfer} onRemove={removeTransfer} />

<div class="statusbar">
  <span class="dot" class:on={connected}></span>
  <span class="stxt">{status}</span>
  {#if activeXfers.length}
    <span class="xfer-ind">
      <span class="spinner"></span>
      Transferring {activeXfers.length} {activeXfers.length === 1 ? "file" : "files"} — {xferPct}%
    </span>
  {/if}
</div>

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
      {#if form.password}
        <label class="pwsave"><input type="checkbox" bind:checked={saveSitePw} /> Save password in the keychain</label>
      {/if}
      <p class="hint">Connection settings are stored as JSON; the password (if saved) goes to the OS keychain.</p>
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
    {#if overwrite.details?.length}
      <table class="ow">
        <thead><tr><th>Name</th><th>Source</th><th>Target</th></tr></thead>
        <tbody>
          {#each overwrite.details.slice(0, 8) as d}
            {@const newer = d.srcMtime && d.dstMtime ? (d.srcMtime > d.dstMtime ? "newer" : d.srcMtime < d.dstMtime ? "older" : "same") : ""}
            <tr>
              <td class="ow-nm" title={d.name}>{d.name}</td>
              <td class="ow-m" class:hit={newer === "newer"}>{humanSize(d.srcSize)} · {fmtTime(d.srcMtime)}{#if newer} · {newer}{/if}</td>
              <td class="ow-m">{humanSize(d.dstSize)} · {fmtTime(d.dstMtime)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
      {#if overwrite.details.length > 8}<p class="hint">…and {overwrite.details.length - 8} more.</p>{/if}
    {/if}
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

{#if showHelp}
  <Modal title="Keyboard shortcuts" onClose={() => (showHelp = false)}>
    <div class="keys">
      <span class="k">F5</span><span>Copy (upload/download) selection</span>
      <span class="k">F6</span><span>Move selection (copy then delete)</span>
      <span class="k">F2</span><span>Rename</span>
      <span class="k">F3</span><span>View file</span>
      <span class="k">F4</span><span>Edit remote file (auto-upload on save)</span>
      <span class="k">Del</span><span>Delete selection</span>
      <span class="k">Enter</span><span>Open folder</span>
      <span class="k">Backspace</span><span>Parent directory</span>
      <span class="k">Tab</span><span>Switch panes</span>
      <span class="k">↑ ↓</span><span>Move selection (Shift extends)</span>
      <span class="k">Home / End</span><span>First / last item</span>
      <span class="k">⌘A / ⌘I</span><span>Select all / invert</span>
      <span class="k">Space</span><span>Toggle current row</span>
      <span class="k">+ / −</span><span>Select / deselect by mask</span>
      <span class="k">⌘L</span><span>Focus the path bar</span>
      <span class="k">type…</span><span>Jump to a row by name</span>
    </div>
    <p class="hint">Drag rows between panes to transfer; drop onto a folder to go into it.</p>
    <div class="dlg-actions"><button onclick={() => (showHelp = false)}>Close</button></div>
  </Modal>
{/if}

{#if findState}
  <Modal title="Find remote files" onClose={() => (findState = null)}>
    <form class="findbar" onsubmit={(e) => (e.preventDefault(), runFind())}>
      <span class="muted">under {remote.path}</span>
      <input class="dlg-input findmask" placeholder="mask, e.g. *.log" bind:value={findState.mask} autofocus />
      <button type="submit" class="primary" disabled={findState.busy}>{findState.busy ? "Searching…" : "Search"}</button>
    </form>
    {#if findState.results}
      <div class="findres">
        {#if findState.results.length}
          <div class="findcount">{findState.results.length} match(es){findState.results.length === 1000 ? " (capped)" : ""}</div>
          <ul>
            {#each findState.results as h (h.path)}
              <li>
                <span class="ftype">{h.is_dir ? "dir" : "file"}</span>
                <span class="fpath" title={h.path}>{h.path}</span>
                <button class="ghost" onclick={() => openFindHit(h)}>Open dir</button>
              </li>
            {/each}
          </ul>
        {:else}
          <div class="findcount">No matches.</div>
        {/if}
      </div>
    {/if}
    <div class="dlg-actions"><button onclick={() => (findState = null)}>Close</button></div>
  </Modal>
{/if}

{#if maskSelect}
  <Modal title={maskSelect.add ? "Select by mask" : "Deselect by mask"} onClose={() => (maskSelect = null)}>
    <form onsubmit={(e) => (e.preventDefault(), applyMask(maskValue))}>
      <input class="dlg-input" placeholder="*.log" bind:value={maskValue} autofocus />
      <p class="hint">Glob mask — <code>*</code> and <code>?</code> wildcards, case-insensitive.</p>
      <div class="dlg-actions">
        <button type="button" onclick={() => (maskSelect = null)}>Cancel</button>
        <button type="submit">{maskSelect.add ? "Select" : "Deselect"}</button>
      </div>
    </form>
  </Modal>
{/if}

{#if showLogin}
  <Modal title="Connect to server" onClose={() => (showLogin = false)}>
    <div class="login-layout">
      <aside class="sites-side">
        <div class="sites-head">Saved sites</div>
        <ul class="sites-list">
          {#each sites as s (s.name)}
            <li
              class:active={selectedSite === s.name}
              onclick={() => applySiteByName(s.name)}
              ondblclick={() => connectSite(s)}
              role="button"
              tabindex="0"
            >
              <span class="snm">{s.name}</span>
              <span class="spr">{s.protocol}</span>
              <button class="srm" title="Delete site" onclick={(e) => (e.stopPropagation(), deleteSiteByName(s.name))}>✕</button>
            </li>
          {/each}
          {#if !sites.length}<li class="sites-empty">No saved sites yet</li>{/if}
        </ul>
      </aside>
      <form class="login" onsubmit={(e) => (e.preventDefault(), connect())}>
        <div class="lrow">
          <label>Protocol</label>
          <select bind:value={form.protocol} onchange={() => (form.port = defaultPort(form.protocol))}>
            {#each PROTOS as p}<option value={p}>{p.toUpperCase()}</option>{/each}
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
            ⚠ Host key <code>{hostKey.fingerprint}</code> contradicts the one stored for this host.
            Only continue if you know it legitimately changed (e.g. the server was reinstalled) — otherwise this could be a man-in-the-middle.
            <button type="button" class="danger" onclick={forgetHostKey}>Forget old key &amp; connect</button>
          {:else}
            Unknown server key: <code>{hostKey.fingerprint}</code>
            <button type="button" class="primary" onclick={() => connect(hostKey.fingerprint)}>Trust &amp; Connect</button>
          {/if}
        </div>
      {/if}

      <div class="lactions">
        <button type="button" class="ghost" disabled={!form.host && !form.bucket} onclick={() => { saveSitePw = !!form.password; saveSiteName = form.host || form.bucket || ""; }}>Save site…</button>
        <button type="button" class="ghost" disabled={!selectedSite} onclick={deleteSite}>Delete site</button>
        <span class="grow"></span>
        <button type="button" onclick={() => (showLogin = false)}>Close</button>
        <button type="submit" class="primary" disabled={busy}>{busy ? "Connecting…" : "Connect"}</button>
      </div>
      </form>
    </div>
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
  .xfer-ind {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--accent);
    font-weight: 500;
    white-space: nowrap;
  }
  .spinner {
    width: 12px;
    height: 12px;
    border: 2px solid color-mix(in srgb, var(--accent) 30%, transparent);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  /* Login modal */
  .login-layout {
    display: flex;
    gap: 14px;
    width: 660px;
    max-width: 88vw;
  }
  .sites-side {
    width: 190px;
    flex: none;
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
    background: var(--panel-2);
  }
  .sites-head {
    font-size: 11px;
    font-weight: 600;
    color: var(--text-2);
    padding: 6px 10px;
    border-bottom: 1px solid var(--border);
  }
  .sites-list {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow: auto;
    max-height: 360px;
  }
  .sites-list li {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 5px 10px;
    cursor: pointer;
    border-bottom: 1px solid color-mix(in srgb, var(--border) 50%, transparent);
  }
  .sites-list li:hover {
    background: var(--hover);
  }
  .sites-list li.active {
    background: var(--sel);
  }
  .sites-list .snm {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
  }
  .sites-list .spr {
    font-size: 10px;
    text-transform: uppercase;
    color: var(--text-3);
  }
  .sites-list .srm {
    border: none;
    background: transparent;
    color: var(--text-3);
    padding: 0 3px;
    font-size: 12px;
    border-radius: 4px;
    opacity: 0;
  }
  .sites-list li:hover .srm {
    opacity: 1;
  }
  .sites-list .srm:hover {
    color: var(--danger);
  }
  .sites-empty {
    color: var(--text-3);
    font-size: 12px;
    padding: 12px 10px;
    cursor: default;
  }
  .pwsave {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    margin-bottom: 10px;
  }
  .login {
    display: flex;
    flex-direction: column;
    gap: 8px;
    flex: 1;
    min-width: 0;
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
  .findbar {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 520px;
    max-width: 80vw;
    margin-bottom: 8px;
  }
  .findbar .muted { font-size: 12px; color: var(--text-2); white-space: nowrap; }
  .findmask { flex: 1; margin: 0; }
  .findbar button { padding: 6px 12px; }
  .findres { max-height: 320px; overflow: auto; margin-bottom: 10px; }
  .findcount { font-size: 12px; color: var(--text-2); margin: 4px 0; }
  .findres ul { list-style: none; margin: 0; padding: 0; }
  .findres li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 3px 0;
    border-bottom: 1px solid color-mix(in srgb, var(--border) 50%, transparent);
    font-size: 12px;
  }
  .ftype {
    font-size: 10px;
    text-transform: uppercase;
    color: var(--text-3);
    width: 30px;
    flex: none;
  }
  .fpath {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    direction: rtl;
    text-align: left;
  }
  .findres .ghost { font-size: 11px; padding: 2px 8px; }
  .keys {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 16px;
    font-size: 13px;
    margin-bottom: 10px;
  }
  .keys .k {
    font-family: var(--mono);
    font-size: 12px;
    color: var(--text-2);
    white-space: nowrap;
  }
  .ow {
    width: 100%;
    border-collapse: collapse;
    font-size: 12px;
    margin: 6px 0 12px;
  }
  .ow th {
    text-align: left;
    color: var(--text-2);
    font-weight: 600;
    border-bottom: 1px solid var(--border);
    padding: 3px 6px;
  }
  .ow td {
    padding: 3px 6px;
    border-bottom: 1px solid color-mix(in srgb, var(--border) 50%, transparent);
  }
  .ow-nm {
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ow-m {
    font-family: var(--mono);
    font-size: 11px;
    color: var(--text-2);
    white-space: nowrap;
  }
  .ow-m.hit {
    color: var(--ok);
  }
  /* Cursor feedback during a pointer-drag (classes toggled on <body>). */
  :global(body.dragging),
  :global(body.dragging *) {
    cursor: grabbing !important;
    -webkit-user-select: none !important;
    user-select: none !important;
  }
  :global(body.drop-ok),
  :global(body.drop-ok *) {
    cursor: copy !important;
  }
  .drag-ghost {
    position: fixed;
    z-index: 100;
    pointer-events: none;
    padding: 3px 9px;
    font-size: 12px;
    border-radius: 6px;
    background: var(--accent);
    color: #fff;
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.3);
    white-space: nowrap;
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
