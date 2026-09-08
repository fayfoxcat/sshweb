<script lang="ts">
  import { onMount } from "svelte";

  /** Teleport children into a target element (default `<body>`), so overlays and
   *  toasts escape ancestor clipping / stacking. SSR-safe: renders in place
   *  during prerender and moves to `target` only once mounted on the client.
   *  `target` is a function so the DOM is never touched at module/SSR time. */
  export let target: () => HTMLElement = () => document.body;

  let host: HTMLElement;

  onMount(() => {
    const el = target();
    el.appendChild(host);
    return () => host.remove();
  });
</script>

<div bind:this={host}><slot /></div>
