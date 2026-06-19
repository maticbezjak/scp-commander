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
