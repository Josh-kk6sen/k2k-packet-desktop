import { defineConfig } from "vite";
import { readFileSync } from "node:fs";

// Single source of truth for the app version: package.json. Vite substitutes
// __APP_VERSION__ at build time, so no HTML/JS carries a hand-maintained
// version string. The Rust side reads CARGO_PKG_VERSION (tauri.conf.json has no
// version key, so it inherits), and the release bump writes both manifests.
const pkg = JSON.parse(
  readFileSync(new URL("./package.json", import.meta.url), "utf-8"),
);

export default defineConfig({
  clearScreen: false,
  define: {
    __APP_VERSION__: JSON.stringify(pkg.version),
  },
  server: {
    port: 1420,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: process.env.TAURI_PLATFORM === "windows" ? "chrome105" : "safari13",
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    sourcemap: true,
  },
});
