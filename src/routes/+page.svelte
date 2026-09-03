<script lang="ts">
  import Session from "$lib/Session.svelte";

  // `/` 直接渲染终端页(认证由 layout 的 AuthGate 处理),地址栏不再出现
  // `/s/terminal` 后缀。WS 会话 key 由前端存于 sessionStorage 并在每次刷新后
  // 重连同一服务端会话,终端进程与标签在刷新后保留。
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
