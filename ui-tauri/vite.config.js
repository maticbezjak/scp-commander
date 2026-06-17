import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Strip the `crossorigin` attribute Vite puts on the entry <script>/<link>.
// Tauri serves the bundle from the `tauri://localhost` custom protocol on
// macOS/Linux, where a crossorigin module triggers a CORS check the protocol
// doesn't satisfy — the script is blocked and the window renders blank.
// (Windows uses https://tauri.localhost and is unaffected.)
const stripCrossorigin = {
  name: "strip-crossorigin",
  transformIndexHtml(html) {
    return html.replace(/\s+crossorigin(?==|\s|>)/g, "");
  },
};

// Vite root is this dir; build output goes to ./dist, which Tauri serves as
// `frontendDist` (../dist relative to src-tauri).
export default defineConfig({
  plugins: [svelte(), stripCrossorigin],
  clearScreen: false,
  build: { outDir: "dist", emptyOutDir: true, target: "es2021" },
  // `cargo tauri dev` runs this dev server; the port is referenced by devUrl.
  server: { port: 1420, strictPort: true },
});
