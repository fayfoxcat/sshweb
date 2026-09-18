import { execSync } from "node:child_process";

import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";

const commitHash = (() => {
  try {
    return execSync("git rev-parse --short HEAD").toString().trim();
  } catch {
    return "dev";
  }
})();

export default defineConfig({
  define: {
    __APP_VERSION__: JSON.stringify("0.5.0-" + commitHash),
  },

  plugins: [sveltekit()],

  // `lucide-svelte` banners every icon module with its ISC notice, and the
  // default `inline` mode keeps a full copy in each chunk: 48 icons = 172 KB,
  // ~8.4% of the JS bundle (and 172 KB of the binary too, since `build/` is
  // embedded verbatim). This is a private internal app whose bundle is only
  // ever served to its own users, so drop the per-chunk banners.
  esbuild: { legalComments: "none" },

  server: {
    proxy: {
      "/api": {
        target: "http://[::1]:2223",
        changeOrigin: true,
        ws: true,
      },
    },
  },
});
