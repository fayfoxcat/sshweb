<script lang="ts">
  import { page } from "$app/stores";

  import logotypeDark from "$lib/assets/logotype-dark.svg";
</script>

<main class="p-4 max-w-xl mx-auto my-6 md:my-12 lg:my-24">
  <img class="h-16 -mx-2" src={logotypeDark} alt="sshweb logo" />

  <div class="space-y-4 mt-6 mb-8 text-zinc-300">
    <p>
      {#if $page.status === 404}
        <b class="text-white">404 未找到。</b> 找不到此页面，很抱歉！
      {:else}
        <b class="text-white">错误 {$page.status}。</b> 发生意外错误。
      {/if}
    </p>
    {#if $page.status !== 404}
      <pre class="whitespace-pre-wrap break-all p-3 rounded bg-zinc-900">
{JSON.stringify($page.error, null, 2)}
</pre>
    {/if}
    <p>
      也许可以稍后再试？如果您有任何反馈，欢迎通过
      <a class="underline text-white" href="mailto:ekzhang1@gmail.com"
        >ekzhang1@gmail.com</a
      > 联系我们。
    </p>
  </div>

  <a
    href="/"
    class="inline-block font-medium px-6 py-2 rounded-full bg-indigo-900 hover:bg-indigo-700"
    >返回首页</a
  >
</main>
