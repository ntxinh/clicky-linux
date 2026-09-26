import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri dev: fixed port, no HMR host quirks inside the webview.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    target: "es2021",
    outDir: "dist",
  },
});
