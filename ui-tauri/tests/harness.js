// Test harness: stubs the Tauri bridge so the Svelte frontend can be driven in
// a plain browser. The app talks to the backend through `window.__TAURI__`
// (withGlobalTauri), so replacing that object is enough — no Rust involved.
//
// Exposed in the page by installStub():
//   window.__calls  – [{cmd, args}] every invoke() the app made (see calls())
//   window.__fire(name, payload) – deliver a backend event to listen(name)
//
// Gotchas these helpers encode:
//   * list_local / list_remote return an ARRAY of entries (not {path,entries}).
//   * connect_session returns {status:"connected", session_id, path, entries}.
//   * A synthetic element.click() does NOT fire pointerdown, so the pane's
//     onFocus never runs and keyboard commands act on the *other* pane. Use
//     clickRow() (real pointer events) whenever focus matters.

/** A file entry as the backend's EntryDto serializes. */
export const FILE = (name, extra = {}) => ({
  name,
  is_dir: false,
  size: 1024,
  mtime: 1700000000,
  mode: 0o644,
  uid: 0,
  gid: 0,
  is_symlink: false,
  perms: "rw-r--r--",
  ...extra,
});

/** A directory entry. */
export const DIR = (name, extra = {}) => FILE(name, { is_dir: true, size: 0, perms: "rwxr-xr-x", ...extra });

/** Generate n numbered files (f0000.dat …) — for virtualization tests. */
export const manyFiles = (n) =>
  Array.from({ length: n }, (_, i) => FILE(`f${String(i).padStart(4, "0")}.dat`, { size: 1000 + i }));

export const DEFAULT_PREFS = {
  show_hidden: true,
  confirm_delete: true,
  confirm_overwrite: true,
  atomic_uploads: false,
  max_parallel: 2,
  show_owner_group: false,
};

/**
 * Install the Tauri stub. Must be called before page.goto().
 * @param page Playwright page
 * @param opts {local, remote, remotePath, prefs, theme, localPath}
 */
export async function installStub(page, opts = {}) {
  const cfg = {
    local: opts.local ?? [FILE("a.txt"), FILE("b.txt"), DIR("sub")],
    remote: opts.remote ?? [DIR("drop"), FILE("config.xml")],
    remotePath: opts.remotePath ?? "/data",
    localPath: opts.localPath ?? "/home/user",
    prefs: { ...DEFAULT_PREFS, ...(opts.prefs ?? {}) },
    theme: opts.theme ?? null,
  };
  await page.addInitScript((c) => {
    if (c.theme) localStorage.setItem("theme", c.theme);
    window.__calls = [];
    const listeners = {};
    let nextId = 0;
    window.__fire = (name, payload) => (listeners[name] || []).forEach((cb) => cb({ payload }));
    window.__TAURI__ = {
      core: {
        invoke: async (cmd, args) => {
          window.__calls.push({ cmd, args });
          switch (cmd) {
            case "load_prefs":
              return c.prefs;
            // The app seeds the local pane with home_local() on startup; without
            // it local.path stays null and any path join (delete/transfer) blows
            // up inside a swallowing try/catch.
            case "home_local":
              return c.localPath;
            case "list_local":
              return c.local;
            case "list_remote":
              return c.remote;
            case "connect_session":
              return { status: "connected", session_id: 1, path: c.remotePath, entries: c.remote };
            case "sites_list":
              return [];
            case "secret_get":
              return null;
            case "local_is_dir":
              return false;
            case "parent_local":
              return "/";
            case "enqueue":
              return ++nextId;
            case "known_hosts_list":
              return [];
            default:
              return null;
          }
        },
      },
      event: {
        listen: async (name, cb) => {
          (listeners[name] ||= []).push(cb);
          return () => {};
        },
        emit: async () => {},
      },
      window: {
        getCurrentWindow: () => ({ setTitle() {}, listen: async () => () => {} }),
      },
    };
  }, cfg);
}

/** Open the app with the stub installed and wait for first render. */
export async function open(page, opts = {}) {
  await installStub(page, opts);
  await page.goto("/");
  await page.waitForSelector(".pane[data-kind=local]");
  return page;
}

/** Dismiss the login modal without connecting (local pane only). */
export async function closeLogin(page) {
  await page.getByRole("button", { name: "Close", exact: true }).click();
  await page.waitForSelector(".login", { state: "detached" }).catch(() => {});
}

/** Fill the connect dialog and connect; resolves once the remote pane exists. */
export async function connect(page) {
  const host = page.locator(".login input[type=text], .login input:not([type])").first();
  await host.fill("192.168.0.1");
  await page.getByRole("button", { name: "Connect", exact: true }).click();
  await page.waitForSelector(".pane[data-kind=remote]");
}

/** Deliver an "xfer" event (started/progress/done/failed/cancelled). */
export const fireXfer = (page, payload) => page.evaluate((p) => window.__fire("xfer", p), payload);

/** Args of every invoke() of `cmd`, in order. */
export const calls = (page, cmd) =>
  page.evaluate((c) => window.__calls.filter((x) => x.cmd === c).map((x) => x.args), cmd);

/** Row locator by name within a pane ("local" | "remote"). */
export const row = (page, kind, name) => page.locator(`.pane[data-kind=${kind}] tbody tr[data-name="${name}"]`);

/** Click a row with REAL pointer events so the pane takes focus (needed before
 *  keyboard commands). Pass {modifiers:["Meta"]} for multi-select. */
export const clickRow = (page, kind, name, opts = {}) => row(page, kind, name).click(opts);

/** Names of the currently rendered rows in a pane (virtualized: visible only). */
export const renderedNames = (page, kind) =>
  page.locator(`.pane[data-kind=${kind}] tbody tr[data-name]`).evaluateAll((els) => els.map((e) => e.dataset.name));

/** Names of selected rows in a pane. */
export const selectedNames = (page, kind) =>
  page.locator(`.pane[data-kind=${kind}] tbody tr.sel`).evaluateAll((els) => els.map((e) => e.dataset.name));
