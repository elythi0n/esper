import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { fileURLToPath, URL } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { BEFORE_PAINT } from "./src/lib/first-paint";

/**
 * The Esper home, the way every other part of the product resolves it:
 * `ESPER_HOME` decides, and otherwise it sits beside the home directory.
 */
function esperHome(): string {
  const named = process.env.ESPER_HOME?.trim();
  return named ? named : join(homedir(), ".esper");
}

/**
 * The API token, read from the 0600 file at dev-server start.
 *
 * This is the development half of the seam the desktop shell owns: the token
 * belongs to a process that can read a file, and the page gets a proxy rather
 * than a credential. Absent is not an error — the proxy then has no header to
 * add, the API answers 401, and the screens say the control plane refused,
 * which is exactly what somebody who has not run `esper api` should see.
 */
function apiToken(): string | null {
  const path = join(esperHome(), "api.token");
  if (!existsSync(path)) return null;
  try {
    return readFileSync(path, "utf8").trim() || null;
  } catch {
    return null;
  }
}

const version = readFileSync(fileURLToPath(new URL("../VERSION", import.meta.url)), "utf8").trim();
const upstream = process.env.ESPER_API ?? "http://127.0.0.1:8687";

export default defineConfig(() => {
  const token = apiToken();
  return {
    plugins: [
      react(),
      tailwindcss(),
      {
        /*
          The theme, applied before the first pixel.

          A window that mounts dark and corrects itself to light is a flash
          somebody sees every launch. So the rule runs inline in `<head>` —
          and it is **injected from the module that also implements it**
          rather than written out here, because two copies of a rule drift and
          the one nobody is looking at is the one that is wrong. See
          `src/lib/first-paint.ts`.
        */
        name: "esper-first-paint",
        transformIndexHtml() {
          return [
            {
              tag: "script",
              injectTo: "head",
              children: BEFORE_PAINT,
            },
          ];
        },
      },
    ],
    // The `@/` alias tsconfig declares, so the bundler and the typechecker agree.
    resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
    // The version the repo already holds, so the sidebar's foot cannot drift
    // from what `esper version` says.
    define: { __ESPER_VERSION__: JSON.stringify(version) },
    server: {
      port: 5178,
      strictPort: true,
      proxy: {
        "/api": {
          target: upstream,
          changeOrigin: false,
          rewrite: (path: string) => path.replace(/^\/api/, ""),
          configure: (proxy: {
            on: (event: string, cb: (proxyReq: { setHeader: (k: string, v: string) => void }) => void) => void;
          }) => {
            proxy.on("proxyReq", (proxyReq) => {
              // The one place the token is handled, and it never goes further
              // than this process.
              if (token) proxyReq.setHeader("Authorization", `Bearer ${token}`);
            });
          },
        },
      },
    },
    build: { target: "es2022", sourcemap: false },
  };
});
