<script lang="ts">
  import type { Snippet } from "svelte";

  // The Inter `@font-face` blocks live in `app.css` (latin + latin-ext only);
  // importing `@fontsource-variable/inter` here would ship all seven subsets.
  import "sshx-xterm/css/xterm.css";
  import "../app.css";

  import ToastContainer from "$lib/ui/ToastContainer.svelte";
  import HostKeyDialog from "$lib/ui/HostKeyDialog.svelte";
  import AuthGate from "$lib/ui/AuthGate.svelte";

  let { children }: { children?: Snippet } = $props();
</script>

<ToastContainer />

<!-- One instance for the whole app: any failure path opens it by setting
     `hostKeyPrompt` (已知坑 80). -->
<HostKeyDialog />

<AuthGate>
  {@render children?.()}
</AuthGate>
