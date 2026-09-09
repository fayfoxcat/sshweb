<script lang="ts">
  import type { Snippet } from "svelte";

  import "@fontsource-variable/inter";

  import "sshx-xterm/css/xterm.css";
  import "../app.css";

  import ToastContainer from "$lib/ui/ToastContainer.svelte";
  import AuthGate from "$lib/ui/AuthGate.svelte";

  let { children }: { children?: Snippet } = $props();
</script>

<ToastContainer />

<AuthGate>
  {@render children?.()}
</AuthGate>
