// Thin wrappers over the Tauri bridge (withGlobalTauri = true, so no npm dep).
const tauri = window.__TAURI__;
export const invoke = tauri.core.invoke;
export const listen = tauri.event.listen;
export const emit = tauri.event.emit;

export function humanSize(n) {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${n} B` : `${v.toFixed(1)} ${u[i]}`;
}

export function humanRate(bps) {
  return `${humanSize(Math.round(bps))}/s`;
}

// Compact ETA: "12s", "3m 05s", "1h 04m".
export function fmtEta(sec) {
  if (sec == null || !isFinite(sec)) return "";
  sec = Math.round(sec);
  if (sec < 60) return `${sec}s`;
  const m = Math.floor(sec / 60);
  if (m < 60) return `${m}m ${String(sec % 60).padStart(2, "0")}s`;
  return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, "0")}m`;
}

// Update a queue item with a progress sample, deriving a smoothed speed + ETA.
export function updateProgress(t, done, total) {
  const now = Date.now();
  if (t.lastAt != null) {
    const dt = (now - t.lastAt) / 1000;
    if (dt > 0) {
      const inst = (done - t.lastDone) / dt;
      t.speed = t.speed ? t.speed * 0.7 + inst * 0.3 : inst;
    }
  }
  t.lastAt = now;
  t.lastDone = done;
  t.done = done;
  t.total = total;
  t.eta = t.speed > 0 && total > done ? (total - done) / t.speed : null;
}

// Join a path with a child, or go to the parent when child === "..".
// `sep` is "/" for remote (POSIX) and the platform separator for local.
export function joinPath(base, child, sep = "/") {
  if (child === "..") {
    const trimmed = base.replace(/[/\\]+$/, "");
    const idx = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
    if (idx <= 0) return sep === "/" ? "/" : trimmed.slice(0, idx + 1) || sep;
    return trimmed.slice(0, idx);
  }
  return base.endsWith(sep) ? base + child : base + sep + child;
}
