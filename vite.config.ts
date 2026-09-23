import { defineConfig } from "vite";
import { readFileSync } from "node:fs";
import react from "@vitejs/plugin-react";
// @ts-expect-error type error without @types/node package
import process from "node:process";

const host = process.env.TAURI_DEV_HOST;

// Read at build time from package.json. A version typed into a component is a
// version that goes stale the moment somebody bumps the real one — which it
// promptly did.
const APP_VERSION = JSON.parse(readFileSync("./package.json", "utf-8")).version;

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [react()],

  define: { __APP_VERSION__: JSON.stringify(APP_VERSION) },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
