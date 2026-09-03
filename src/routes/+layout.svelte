<script lang="ts">
  import "@fontsource-variable/inter";

  import "sshx-xterm/css/xterm.css";
  import "../app.css";

  import ToastContainer from "$lib/ui/ToastContainer.svelte";
  import AuthGate from "$lib/ui/AuthGate.svelte";
</script>

<ToastContainer />

<AuthGate>
  <slot />
</AuthGate>
