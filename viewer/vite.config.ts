import path from "node:path";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

// In dev, the viewer runs on Vite (5173) and proxies API + signaling to carson-server (8080).
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": path.resolve(import.meta.dirname, "src") } },
  server: {
    port: 5173,
    proxy: {
      "/v1": { target: "http://localhost:8080", ws: true },
      "/auth": "http://localhost:8080",
      "/healthz": "http://localhost:8080",
    },
  },
  build: { outDir: "dist", sourcemap: true },
});
