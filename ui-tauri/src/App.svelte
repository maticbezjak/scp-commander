<script>
  import { invoke, joinPath } from "./lib/api.js";
  import Pane from "./lib/Pane.svelte";

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
  let hostKey = $state(null); // { fingerprint, mismatch }

  let local = $state({ path: "/", entries: [] });
  let remote = $state({ path: "/", entries: [] });

  const isSftp = $derived(form.protocol === "sftp");
  const isS3 = $derived(form.protocol === "s3");

  // Load the local pane at the user's home directory on startup.
  $effect(() => {
    init();
  });
  let started = false;
  async function init() {
    if (started) return;
    started = true;
    const home = await invoke("home_local");
    await loadLocal(home);
  }

  async function loadLocal(path) {
    try {
      const entries = await invoke("list_local", { path });
      local = { path, entries };
    } catch (e) {
      status = `Local: ${e}`;
    }
  }
  async function localUp() {
    const parent = await invoke("parent_local", { path: local.path });
    loadLocal(parent);
  }

  async function loadRemote(path) {
    busy = true;
    try {
      const entries = await invoke("list_remote", { path });
      remote = { path, entries };
      status = `${path} — ${entries.length} item(s)`;
    } catch (e) {
      status = `Error: ${e}`;
    } finally {
      busy = false;
    }
  }
  function remoteUp() {
    loadRemote(joinPath(remote.path, ".."));
  }

  function defaultPort(p) {
    return p === "sftp" ? 22 : p === "s3" ? 443 : 21;
  }

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

<header>
  <strong>SCP Commander</strong>
  <span class="muted">— Tauri</span>
  <span class="status">{status}</span>
</header>

<form class="login" onsubmit={(e) => (e.preventDefault(), connect())}>
  <select
    bind:value={form.protocol}
    onchange={() => (form.port = defaultPort(form.protocol))}
  >
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
  {#if isS3}
    <input placeholder="bucket" bind:value={form.bucket} />
  {/if}
  {#if !connected}
    <button type="submit" disabled={busy}>Connect</button>
  {:else}
    <button type="button" onclick={disconnect}>Disconnect</button>
  {/if}
</form>

{#if hostKey}
  <div class="hostkey" class:mismatch={hostKey.mismatch}>
    {#if hostKey.mismatch}
      ⚠ The server's host key <code>{hostKey.fingerprint}</code> contradicts the
      stored one — possible man-in-the-middle. Connection refused.
      <button onclick={() => (hostKey = null)}>Dismiss</button>
    {:else}
      Unknown server key: <code>{hostKey.fingerprint}</code>
      <button onclick={() => connect(hostKey.fingerprint)}>Trust & Connect</button>
      <button onclick={() => (hostKey = null)}>Cancel</button>
    {/if}
  </div>
{/if}

<div class="panes">
  <Pane
    title="Local"
    path={local.path}
    entries={local.entries}
    onUp={localUp}
    onOpen={(e) => loadLocal(joinPath(local.path, e.name, "/"))}
    onNavigate={loadLocal}
  />
  {#if connected}
    <Pane
      title="Remote"
      path={remote.path}
      entries={remote.entries}
      {busy}
      onUp={remoteUp}
      onOpen={(e) => loadRemote(joinPath(remote.path, e.name))}
      onNavigate={loadRemote}
    />
  {:else}
    <div class="placeholder">Connect to a server to browse the remote side.</div>
  {/if}
</div>

<style>
  header {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 8px 10px 4px;
  }
  .muted {
    opacity: 0.5;
  }
  .status {
    margin-left: auto;
    font-size: 12px;
    opacity: 0.8;
  }
  .login {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 4px 10px 8px;
    align-items: center;
  }
  .login input,
  .login select,
  .login button {
    font: inherit;
    padding: 4px 6px;
  }
  .login .host {
    flex: 1;
    min-width: 140px;
  }
  .login .port {
    width: 64px;
  }
  .hostkey {
    margin: 0 10px 8px;
    padding: 8px 10px;
    border-radius: 6px;
    background: color-mix(in srgb, orange 18%, var(--panel));
    font-size: 13px;
  }
  .hostkey.mismatch {
    background: color-mix(in srgb, red 22%, var(--panel));
  }
  .hostkey code {
    font-family: ui-monospace, monospace;
  }
  .panes {
    display: flex;
    gap: 8px;
    padding: 0 10px 10px;
    flex: 1;
    min-height: 0;
  }
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
