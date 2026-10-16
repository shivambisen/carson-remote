import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

// The public landing page (glenley.in): download glenley, or open the web app to connect.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  build: { outDir: "dist" },
});
