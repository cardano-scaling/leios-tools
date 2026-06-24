import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "url";

const __dirname = fileURLToPath(new URL(".", import.meta.url));

const uiPort = Number(process.env.UI_PORT) || 3001;
const apiPort = Number(process.env.API_PORT) || 9100;

export default defineConfig({
  plugins: [react()],
  server: {
    port: uiPort,
    proxy: {
      "/api": `http://127.0.0.1:${apiPort}`,
    },
  },
  // `vite preview` (production build) needs the same /api proxy as the dev
  // server, otherwise the served bundle can't reach the aggregator. Use
  // `npm run build && npm run preview` for long-running monitoring (the dev
  // build leaks memory via React's User-Timing profiler — see README).
  preview: {
    port: uiPort,
    proxy: {
      "/api": `http://127.0.0.1:${apiPort}`,
    },
  },
  resolve: {
    alias: [{ find: "@", replacement: __dirname + "src" }],
  },
});
