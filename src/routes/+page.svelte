<svelte:options runes={true} />
<script lang="ts">
  import Session from "$lib/Session.svelte";

  // `/` 直接渲染终端页(认证由 +layout.svelte 的 AuthGate 处理)。WS 会话 key
  // 由前端存于 sessionStorage(sshweb-session-key)并在每次刷新后重连同一
  // 服务端会话,终端进程与标签在刷新后保留。
</script>

<svelte:head>
  <title>远程终端</title>

  <style>
    body {
      overscroll-behavior: none;
    }
  </style>
</svelte:head>

<Session />
