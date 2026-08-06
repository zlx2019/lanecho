import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
// @ts-expect-error node
import { readFileSync } from "node:fs";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

const DEV_PORT = 1420;

function tauriDevCsp(): Plugin {
  return {
    name: "lanecho-dev-csp",
    apply: "serve",
    transformIndexHtml() {
      const toml: string = readFileSync(
        new URL("./src-tauri/Tauri.toml", import.meta.url),
        "utf8",
      );
      const found = toml.match(/^csp = "(.+)"$/m);
      if (!found) throw new Error("CSP configuration not found in Tauri.toml");
      const csp = relaxCsp(found[1], {
        "script-src": "'unsafe-inline'",
        "connect-src": host ? `ws://${host}:1421` : `ws://localhost:${DEV_PORT}`,
      });
      return [
        {
          tag: "meta",
          attrs: { "http-equiv": "Content-Security-Policy", content: csp },
          injectTo: "head-prepend" as const,
        },
      ];
    },
  };
}

function relaxCsp(csp: string, extra: Record<string, string>): string {
  const directives = new Map<string, string>();
  for (const part of csp.split(";")) {
    const [name, ...sources] = part.trim().split(/\s+/);
    if (name) directives.set(name, sources.join(" "));
  }
  const fallback = directives.get("default-src") ?? "";
  for (const [name, sources] of Object.entries(extra)) {
    directives.set(
      name,
      `${directives.get(name) ?? fallback} ${sources}`.trim(),
    );
  }
  return [...directives]
    .map(([name, sources]) => (sources ? `${name} ${sources}` : name))
    .join("; ");
}

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react(), tailwindcss(), tauriDevCsp()],
  build: {
    target:
      process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13",
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: DEV_PORT,
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
