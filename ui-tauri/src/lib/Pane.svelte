<script>
  import { humanSize } from "./api.js";

  let {
    kind = "local",
    title,
    path = "/",
    entries = [],
    busy = false,
    selected = [],
    showHidden = true,
    showRights = false,
    showOwnerGroup = false,
    focused = false,
    canTransfer = false,
    transferLabel = "",
    recents = [],
    canBack = false,
    canForward = false,
    onFocus = () => {},
    onUp,
    onHome,
    onBack,
    onForward,
    onRefresh,
    onNavigate,
    onOpen,
    onTransferOne,
    onTransfer,
    onRowClick,
    onContext,
    onNewFolder,
    onDelete,
    onProperties,
    onRowPointerDown = () => {},
    dropActive = false,
    dropName = null,
    flashName = null, // name of a just-arrived file to briefly highlight
    pending = [], // files mid-transfer into this dir: dimmed placeholder rows

    onView = () => {}, // report the current visible row order (for keyboard nav)
    focusPathReq = 0, // bump to request focus on the path bar (⌘L)
    onContextEmpty = () => {}, // right-click on empty pane area
  } = $props();

  let pathInput = $state("");
  let rowsEl; // scroll container, for scroll-into-view
  let pathEl; // path bar input, for ⌘L focus
  $effect(() => {
    pathInput = path;
  });

  let filterText = $state("");
  let sortKey = $state("name"); // name | size | type | mtime
  let ascending = $state(true);

  // Per-pane bookmarks (persisted in localStorage by kind).
  let bmOpen = $state(false);
  let bookmarks = $state(loadBookmarks());
  function loadBookmarks() {
    try { return JSON.parse(localStorage.getItem(`bm.${kind}`) || "[]"); } catch { return []; }
  }
  const isBookmarked = $derived(bookmarks.includes(path));
  function toggleBookmark() {
    bookmarks = isBookmarked ? bookmarks.filter((b) => b !== path) : [...bookmarks, path];
    localStorage.setItem(`bm.${kind}`, JSON.stringify(bookmarks));
  }
  function goBookmark(b) {
    bmOpen = false;
    onNavigate(b);
  }

  // Resizable column widths, persisted per pane in localStorage.
  const DEFAULTS = { size: 64, type: 92, changed: 118, owner: 48, group: 48, rights: 88 };
  function loadWidths() {
    try {
      return { ...DEFAULTS, ...JSON.parse(localStorage.getItem(`colw.${kind}`) || "{}") };
    } catch {
      return { ...DEFAULTS };
    }
  }
  let widths = $state(loadWidths());
  const colWidth = (k) => widths[k] ?? DEFAULTS[k];

  let resizing = null;
  function startResize(key, e) {
    e.preventDefault();
    e.stopPropagation();
    resizing = { key, startX: e.clientX, startW: colWidth(key) };
    window.addEventListener("pointermove", onResize);
    window.addEventListener("pointerup", endResize);
  }
  function onResize(e) {
    if (!resizing) return;
    widths = { ...widths, [resizing.key]: Math.max(40, resizing.startW + (e.clientX - resizing.startX)) };
  }
  function endResize() {
    window.removeEventListener("pointermove", onResize);
    window.removeEventListener("pointerup", endResize);
    if (resizing) {
      localStorage.setItem(`colw.${kind}`, JSON.stringify(widths));
      resizing = null;
    }
  }

  const BASE = [
    { key: "size", label: "Size", sort: "size", align: "right" },
    { key: "type", label: "Type", sort: "type", align: "left" },
    { key: "changed", label: "Changed", sort: "mtime", align: "left" },
  ];
  const OWNER_GROUP = [
    { key: "owner", label: "Owner", align: "right" },
    { key: "group", label: "Group", align: "right" },
  ];
  const RIGHTS = { key: "rights", label: "Rights", align: "left", mono: true };
  let cols = $derived(
    showRights
      ? [...BASE, ...(showOwnerGroup ? OWNER_GROUP : []), RIGHTS]
      : BASE,
  );

  const EXT = {
    txt: "Text", md: "Markdown", log: "Log", json: "JSON", yml: "YAML", yaml: "YAML",
    xml: "XML", html: "HTML", css: "Stylesheet", js: "JavaScript", ts: "TypeScript",
    rs: "Rust source", py: "Python", sh: "Shell script", c: "C source", h: "C header",
    pdf: "PDF document", zip: "ZIP archive", gz: "Gzip archive", tar: "Tar archive",
    png: "PNG image", jpg: "JPEG image", jpeg: "JPEG image", gif: "GIF image", svg: "SVG image",
    mp3: "Audio", mp4: "Video", mov: "Video", doc: "Document", docx: "Document",
    xls: "Spreadsheet", xlsx: "Spreadsheet", conf: "Config", cfg: "Config", ini: "Config",
    key: "Key file", pem: "Key file", env: "Config",
  };
  function typeDesc(e) {
    if (e.is_symlink) return "Symbolic link";
    if (e.is_dir) return "Folder";
    const dot = e.name.lastIndexOf(".");
    if (dot <= 0) return "File";
    const ext = e.name.slice(dot + 1).toLowerCase();
    return EXT[ext] || `${ext.toUpperCase()} file`;
  }
  function fmtTime(m) {
    if (!m) return "";
    const d = new Date(m * 1000);
    const p = (n) => String(n).padStart(2, "0");
    return `${p(d.getDate())}.${p(d.getMonth() + 1)}.${d.getFullYear()} ${p(d.getHours())}:${p(d.getMinutes())}`;
  }

  let display = $derived.by(() => {
    let v = entries.filter((e) => showHidden || !e.name.startsWith("."));
    const f = filterText.trim().toLowerCase();
    if (f) v = v.filter((e) => e.name.toLowerCase().includes(f));
    const dir = ascending ? 1 : -1;
    return [...v].sort((a, b) => {
      if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
      let r;
      switch (sortKey) {
        case "size": r = a.size - b.size; break;
        case "type": r = typeDesc(a).localeCompare(typeDesc(b)); break;
        case "mtime": r = (a.mtime || 0) - (b.mtime || 0); break;
        default: r = 0;
      }
      return (r || a.name.localeCompare(b.name)) * dir;
    });
  });

  let selEntries = $derived(entries.filter((e) => selected.includes(e.name)));
  let selBytes = $derived(selEntries.filter((e) => !e.is_dir).reduce((s, e) => s + e.size, 0));

  // Report the visible order upward so the keyboard handler can navigate it.
  $effect(() => {
    onView(display.map((e) => e.name));
  });
  // Keep the selected row visible during keyboard navigation.
  $effect(() => {
    selected;
    queueMicrotask(() => rowsEl?.querySelector("tr.sel")?.scrollIntoView({ block: "nearest" }));
  });
  // Scroll a just-arrived (flashing) file into view so it's visible even in a
  // long listing — makes "the file landed here" obvious after a transfer.
  $effect(() => {
    if (!flashName) return;
    queueMicrotask(() => rowsEl?.querySelector("tr.flash")?.scrollIntoView({ block: "nearest" }));
  });
  // ⌘L: focus the path bar.
  let lastFocusReq = 0;
  $effect(() => {
    if (focusPathReq > lastFocusReq) {
      lastFocusReq = focusPathReq;
      pathEl?.focus();
      pathEl?.select();
    }
  });

  function setSort(key) {
    if (sortKey === key) ascending = !ascending;
    else { sortKey = key; ascending = true; }
  }
  function cell(e, key) {
    switch (key) {
      case "size": return e.is_dir ? "" : humanSize(e.size);
      case "type": return typeDesc(e);
      case "changed": return fmtTime(e.mtime);
      case "owner": return e.uid ?? "";
      case "group": return e.gid ?? "";
      case "rights": return e.perms ?? "";
      default: return "";
    }
  }
  function dbl(e) {
    if (e.is_dir) onOpen(e);
    else onTransferOne(e);
  }
</script>

{#snippet ic(name)}
  <svg class="ic" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"
       stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    {#if name === "back"}<path d="M10 3.5 5.5 8 10 12.5" />
    {:else if name === "fwd"}<path d="M6 3.5 10.5 8 6 12.5" />
    {:else if name === "up"}<path d="M8 13V3 M4 7 8 3 12 7" />
    {:else if name === "home"}<path d="M2.5 8 8 3 13.5 8 M4.5 7.5V13H11.5V7.5" />
    {:else if name === "refresh"}<path d="M12.6 6A4.6 4.6 0 1 0 13 9.3" /><path d="M12.8 3v3.2H9.6" />
    {:else if name === "upload"}<path d="M8 3v7 M5 6 8 3 11 6 M3.5 12.5h9" />
    {:else if name === "download"}<path d="M8 3v7 M5 7 8 10 11 7 M3.5 12.5h9" />
    {:else if name === "newfolder"}<path d="M1.8 4h4l1.5 1.5H14.2V12H1.8Z" /><path d="M11 7.4v3 M9.5 8.9h3" />
    {:else if name === "trash"}<path d="M3 4.5h10 M5.5 4.5V3.4h5V4.5 M4.6 4.5 5.1 13h5.8l.5-8.5" />
    {:else if name === "info"}<circle cx="8" cy="8" r="5.6" /><path d="M8 7.3v3.6 M8 5h.01" />
    {:else if name === "bookmark"}<path d="M5 2.5h6v11l-3-2.3-3 2.3Z" />
    {/if}
  </svg>
{/snippet}

{#snippet typeIcon(e)}
  {#if e.is_symlink}
    <svg class="ti link" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true">
      <path d="M4 2h6l2.5 2.5V14H4Z" /><path d="M6 10 10 6 M7 6h3v3" stroke-linecap="round" stroke-linejoin="round" />
    </svg>
  {:else if e.is_dir}
    <svg class="ti dir" viewBox="0 0 16 16" aria-hidden="true">
      <path d="M1.5 4h4.2l1.4 1.5h7.4V13H1.5Z" fill="currentColor" />
    </svg>
  {:else}
    <svg class="ti file" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" aria-hidden="true">
      <path d="M4 2h5.5L12 4.5V14H4Z" /><path d="M9.4 2v2.6H12" />
    </svg>
  {/if}
{/snippet}

<div class="pane" class:focused class:drop-active={dropActive && !dropName} data-kind={kind} onpointerdown={onFocus}>
  <!-- Row 1: title + navigation + file actions + filter -->
  <div class="ptool">
    <span class="title" class:focus={focused}>{title}</span>
    <button class="tb" disabled={!canBack} onclick={onBack} title="Back">{@render ic("back")}</button>
    <button class="tb" disabled={!canForward} onclick={onForward} title="Forward">{@render ic("fwd")}</button>
    <button class="tb" onclick={onUp} title="Parent directory">{@render ic("up")}</button>
    <button class="tb" onclick={onHome} title="Home">{@render ic("home")}</button>
    <button class="tb" onclick={onRefresh} title="Refresh">{@render ic("refresh")}</button>
    <span class="bm">
      <button class="tb" class:on={isBookmarked} onclick={() => (bmOpen = !bmOpen)} title="Bookmarks">{@render ic("bookmark")}</button>
      {#if bmOpen}
        <button class="bm-backdrop" onclick={() => (bmOpen = false)} aria-label="Close"></button>
        <div class="bm-menu">
          {#each bookmarks as b}
            <button class="bm-item" title={b} onclick={() => goBookmark(b)}>{b}</button>
          {/each}
          {#if !bookmarks.length}<div class="bm-empty">No bookmarks</div>{/if}
          <div class="bm-sep"></div>
          <button class="bm-item add" onclick={toggleBookmark}>
            {isBookmarked ? "Remove this directory" : "Bookmark this directory"}
          </button>
        </div>
      {/if}
    </span>
    <span class="vsep"></span>
    <button class="tb" disabled={!canTransfer || selEntries.length === 0} onclick={onTransfer}
            title="{transferLabel} (F5)">{@render ic(kind === "local" ? "upload" : "download")}</button>
    <button class="tb" onclick={onNewFolder} title="New folder">{@render ic("newfolder")}</button>
    <button class="tb" disabled={selEntries.length === 0} onclick={() => onDelete(selEntries)} title="Delete">{@render ic("trash")}</button>
    <button class="tb" disabled={selEntries.length === 0} onclick={() => onProperties(selEntries[0])} title="Properties">{@render ic("info")}</button>
    <span class="grow"></span>
    <input class="filter" placeholder="filter" bind:value={filterText} />
  </div>

  <!-- Row 2: address bar -->
  <div class="addr" class:focus={focused}>
    {@render typeIcon({ is_dir: true })}
    <input
      class="pathbar"
      bind:this={pathEl}
      bind:value={pathInput}
      onkeydown={(e) => e.key === "Enter" && onNavigate(pathInput)}
    />
    {#if recents.length}
      <select class="recents" title="Recent locations"
              onchange={(e) => { if (e.target.value) onNavigate(e.target.value); e.target.selectedIndex = 0; }}>
        <option value="">⌄</option>
        {#each recents as r}<option value={r}>{r}</option>{/each}
      </select>
    {/if}
  </div>

  <!-- Listing -->
  <div
    class="rows"
    class:busy
    bind:this={rowsEl}
    oncontextmenu={(ev) => { if (!ev.target.closest("tr")) { ev.preventDefault(); onContextEmpty(ev); } }}
  >
    <table>
      <colgroup>
        <col />
        {#each cols as c}<col style="width:{colWidth(c.key)}px" />{/each}
      </colgroup>
      <thead>
        <tr>
          <th class="name">
            <button class="hbtn" onclick={() => setSort("name")}>
              Name{#if sortKey === "name"}<span class="arr">{ascending ? "▲" : "▼"}</span>{/if}
            </button>
          </th>
          {#each cols as c}
            <th class={c.align}>
              <button class="hbtn" disabled={!c.sort} onclick={() => c.sort && setSort(c.sort)}>
                {c.label}{#if c.sort && sortKey === c.sort}<span class="arr">{ascending ? "▲" : "▼"}</span>{/if}
              </button>
              <span class="resizer" onpointerdown={(e) => startResize(c.key, e)}></span>
            </th>
          {/each}
        </tr>
      </thead>
      <tbody>
        <tr class="up" ondblclick={onUp}>
          <td class="name"><span class="updots">..</span></td>
          {#each cols as c}<td></td>{/each}
        </tr>
        {#each pending as p (p.name)}
          <tr class="pending" title="{p.upload ? 'Uploading' : 'Downloading'} — in progress">
            <td class="name">{@render typeIcon(p)}<span class="nm">{p.name}</span></td>
            <td colspan={cols.length}>
              <div class="ghost-cell">
                <span class="ghost-bar">
                  <span class="ghost-fill" style="width:{p.total ? Math.min(100, Math.round((100 * p.done) / p.total)) : 0}%"></span>
                </span>
                <span class="ghost-pct">{p.upload ? "↑" : "↓"} {p.total ? Math.min(100, Math.round((100 * p.done) / p.total)) : 0}%</span>
              </div>
            </td>
          </tr>
        {/each}
        {#each display as e, i (e.name)}
          <tr
            class:sel={selected.includes(e.name)}
            class:drop-row={dropName === e.name}
            class:flash={flashName === e.name}
            data-name={e.name}
            onpointerdown={(ev) => onRowPointerDown(e, ev)}
            onclick={(ev) => onRowClick(e, i, ev)}
            ondblclick={() => dbl(e)}
            oncontextmenu={(ev) => (ev.preventDefault(), ev.stopPropagation(), onContext(e, i, ev))}
          >
            <td class="name">{@render typeIcon(e)}<span class="nm">{e.name}</span></td>
            {#each cols as c}
              <td class={c.align} class:mono={c.mono || c.key === "size" || c.key === "changed"}>{cell(e, c.key)}</td>
            {/each}
          </tr>
        {/each}
        {#if !display.length && !pending.length}
          <tr class="empty-row"><td class="empty-cell" colspan={cols.length + 1}>{filterText ? "No matching items" : "Empty folder"}</td></tr>
        {/if}
      </tbody>
    </table>
  </div>

  <!-- Selection footer -->
  <div class="foot">
    {#if selEntries.length}
      {selEntries.length} of {display.length} selected{#if selBytes > 0} · {humanSize(selBytes)}{/if}
    {:else}
      {display.length} item{display.length === 1 ? "" : "s"}
    {/if}
  </div>
</div>

<style>
  .pane {
    display: flex;
    flex-direction: column;
    flex: 1 1 0;
    min-width: 0;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
    background: var(--panel);
    /* The file list is a click/drag surface, not a text document. Suppress
       native text-selection (prefixed — the macOS WKWebView needs -webkit-),
       otherwise a pointer-drag paints a stray selection across rows and the
       release reads as a same-pane drop instead of a real drag. */
    -webkit-user-select: none;
    user-select: none;
  }
  /* …but the filter and path fields are real inputs the user edits. */
  .pane :is(input, textarea) {
    -webkit-user-select: text;
    user-select: text;
  }
  .pane.focused {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--border));
  }
  .pane.drop-active {
    border-color: var(--accent);
    box-shadow: inset 0 0 0 1px var(--accent);
  }
  tbody tr.drop-row,
  tbody tr.drop-row:hover {
    background: color-mix(in srgb, var(--accent) 35%, transparent);
    outline: 1px solid var(--accent);
    outline-offset: -1px;
  }
  /* A file mid-transfer shows as a dimmed placeholder with a live mini
     progress bar, until the real listing replaces it on completion. */
  tbody tr.pending td {
    opacity: 0.6;
    font-style: italic;
    color: var(--text-2);
  }
  tbody tr.pending {
    background: color-mix(in srgb, var(--accent) 7%, transparent);
  }
  .ghost-cell {
    display: flex;
    align-items: center;
    gap: 8px;
    font-style: normal;
  }
  .ghost-bar {
    flex: 0 0 56px;
    height: 6px;
    border-radius: 3px;
    background: var(--panel-2);
    overflow: hidden;
  }
  .ghost-fill {
    display: block;
    height: 100%;
    background: var(--accent);
    border-radius: 3px;
    /* No width transition: track the real progress exactly so the fill
       reaches the end at completion instead of lagging behind. */
  }
  .ghost-pct {
    font-variant-numeric: tabular-nums;
    font-size: 11.5px;
  }
  /* A file that just finished transferring glows green, then fades — so a
     completed transfer is visibly "landed" in this pane. */
  tbody tr.flash,
  tbody tr.flash:hover {
    animation: arrived 2.2s ease-out;
  }
  @keyframes arrived {
    0%,
    30% {
      background: color-mix(in srgb, var(--ok) 60%, transparent);
    }
    100% {
      background: transparent;
    }
  }

  .ptool {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 4px 6px;
    border-bottom: 1px solid var(--border);
    background: var(--header);
  }
  .title {
    font-weight: 600;
    font-size: 12px;
    color: var(--text-2);
    padding: 0 6px 0 2px;
  }
  .title.focus {
    color: var(--accent);
  }
  .tb {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 22px;
    padding: 0;
    border: none;
    background: transparent;
    color: var(--text-2);
    border-radius: 5px;
  }
  .tb:hover:not(:disabled) {
    background: var(--hover);
    color: var(--text);
  }
  .tb.on {
    color: var(--accent);
  }
  .bm {
    position: relative;
    display: inline-flex;
  }
  .bm-backdrop {
    position: fixed;
    inset: 0;
    z-index: 19;
    border: none;
    background: transparent;
    cursor: default;
  }
  .bm-menu {
    position: absolute;
    top: 100%;
    left: 0;
    z-index: 20;
    min-width: 200px;
    max-width: 320px;
    max-height: 320px;
    overflow: auto;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    box-shadow: 0 6px 22px rgba(0, 0, 0, 0.28);
    padding: 4px;
  }
  .bm-item {
    display: block;
    width: 100%;
    text-align: left;
    border: none;
    background: transparent;
    color: var(--text);
    font-size: 12px;
    font-family: var(--mono);
    padding: 4px 8px;
    border-radius: 4px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .bm-item:hover {
    background: var(--hover);
  }
  .bm-item.add {
    font-family: inherit;
    color: var(--accent);
  }
  .bm-empty {
    color: var(--text-3);
    font-size: 12px;
    padding: 4px 8px;
  }
  .bm-sep {
    height: 1px;
    background: var(--border);
    margin: 4px 0;
  }
  .ic {
    width: 15px;
    height: 15px;
  }
  .vsep {
    width: 1px;
    height: 15px;
    background: var(--border);
    margin: 0 3px;
  }
  .grow {
    flex: 1;
  }
  .filter {
    width: 96px;
    font-size: 12px;
    padding: 3px 7px;
  }

  .addr {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 5px 6px;
    padding: 3px 7px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--panel-2);
  }
  .addr.focus {
    border-color: color-mix(in srgb, var(--accent) 45%, var(--border));
  }
  .addr .ti {
    color: var(--text-3);
  }
  .pathbar {
    flex: 1;
    min-width: 0;
    border: none;
    background: transparent;
    padding: 0;
    font-family: var(--mono);
    font-size: 12px;
  }
  .pathbar:focus {
    outline: none;
    box-shadow: none;
  }
  .recents {
    width: 26px;
    padding: 1px;
    border: none;
    background: transparent;
    color: var(--text-3);
    font-size: 11px;
  }

  .rows {
    overflow: auto;
    flex: 1;
  }
  .rows.busy {
    opacity: 0.55;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    table-layout: fixed;
    font-size: 12.5px;
  }
  thead th {
    position: sticky;
    top: 0;
    z-index: 1;
    background: var(--panel-2);
    border-bottom: 1px solid var(--border);
    padding: 0;
    text-align: left;
    font-weight: 600;
  }
  th.right .hbtn {
    justify-content: flex-end;
  }
  .hbtn {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 3px;
    border: none;
    background: transparent;
    color: var(--text-2);
    font-size: 11.5px;
    font-weight: 600;
    padding: 4px 8px;
  }
  .hbtn:hover:not(:disabled) {
    background: transparent;
    color: var(--text);
  }
  .hbtn:disabled {
    opacity: 1;
    cursor: default;
  }
  .arr {
    font-size: 8px;
    color: var(--accent);
  }
  th {
    position: relative;
  }
  .resizer {
    position: absolute;
    top: 0;
    right: -3px;
    width: 7px;
    height: 100%;
    cursor: col-resize;
    z-index: 2;
  }
  .resizer:hover {
    background: color-mix(in srgb, var(--accent) 40%, transparent);
  }

  td {
    padding: 2px 8px;
    border-bottom: 1px solid color-mix(in srgb, var(--border) 55%, transparent);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    user-select: none;
    color: var(--text-2);
  }
  td.name {
    color: var(--text);
  }
  td.right {
    text-align: right;
  }
  td.mono {
    font-family: var(--mono);
    font-size: 11.5px;
    font-variant-numeric: tabular-nums;
  }
  tbody tr {
    cursor: default;
  }
  tbody tr:nth-child(even) {
    background: color-mix(in srgb, var(--panel-2) 55%, transparent);
  }
  tbody tr:hover {
    background: var(--hover);
  }
  tbody tr.sel,
  tbody tr.sel:hover {
    background: var(--sel);
  }
  td.name {
    display: flex;
    align-items: center;
    gap: 7px;
  }
  .nm {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .ti {
    width: 15px;
    height: 15px;
    flex: none;
  }
  .ti.dir {
    color: var(--accent);
  }
  .ti.file {
    color: var(--text-3);
  }
  .ti.link {
    color: var(--warn);
  }
  .up .updots {
    font-weight: 700;
    color: var(--text-2);
    padding-left: 22px;
  }
  .empty-row td {
    color: var(--text-3);
    text-align: center;
    padding: 18px;
    font-style: italic;
  }
  .empty-row:hover {
    background: transparent;
  }

  .foot {
    padding: 3px 10px;
    border-top: 1px solid var(--border);
    background: var(--header);
    font-size: 11.5px;
    color: var(--text-2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
