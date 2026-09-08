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
