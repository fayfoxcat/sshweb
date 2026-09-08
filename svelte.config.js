import adapter from "@sveltejs/adapter-static";
import preprocess from "svelte-preprocess";

/** @type {import('@sveltejs/kit').Config} */
const config = {
  // Consult https://github.com/sveltejs/svelte-preprocess
  // for more information about preprocessors
  preprocess: [
    preprocess({
      postcss: true,
    }),
  ],

  // The app's interactive panels are custom DOM (terminal drop-zone, context
  // menus, row dbl-click, drag handles…) that were never ARIA-role annotated;
  // Svelte 5's a11y compiler diagnostics for those are filtered here (kept off
  // for the rest) — real keyboard/a11y remediation is tracked separately.
  compilerOptions: {
    warningFilter(warning) {
      return !warning.code.startsWith("a11y_");
    },
  },

  kit: {
    adapter: adapter({
      // `/` is prerendered to `index.html`; unknown-route SPA fallback is
      // served by the Rust embed layer (web/embed.rs::SPA_SHELL = index.html),
      // so the adapter needs no own `fallback` copy (avoids emitting a dead
      // `spa.html` that embed.rs never serves).
      precompress: true,
    }),
  },
};

export default config;
