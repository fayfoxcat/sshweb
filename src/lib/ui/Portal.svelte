<script lang="ts">
  import { onMount } from "svelte";
  import type { Snippet } from "svelte";

  /** Teleport children into a target element (default `<body>`), so overlays and
   *  toasts escape ancestor clipping / stacking. SSR-safe: renders in place
   *  during prerender and moves to `target` only once mounted on the client.
   *  `target` is a function so the DOM is never touched at module/SSR time. */
  interface Props {
    target?: () => HTMLElement;
    children?: Snippet;
  }

  let { target = () => document.body, children }: Props = $props();

  let host: HTMLElement;

  onMount(() => {
    const el = target();
    el.appendChild(host);
    return () => host.remove();
  });
</script>

<div bind:this={host}>{@render children?.()}</div>
