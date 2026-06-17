import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Vite root is this dir; build output goes to ./dist, which Tauri serves as
// `frontendDist` (../dist relative to src-tauri).
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  build: { outDir: "dist", emptyOutDir: true, target: "es2021" },
  // `cargo tauri dev` runs this dev server; the port is referenced by devUrl.
  server: { port: 1420, strictPort: true },
});
