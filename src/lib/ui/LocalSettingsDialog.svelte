<script lang="ts">
  import { createEventDispatcher } from "svelte";

  import {
    servers,
    splitSocks5Tunnel,
    updateLocalSettings,
    type LocalSettings,
  } from "$lib/connections";
  import { TERMINAL_ENCODINGS } from "$lib/encoding";
  import { lang, t } from "$lib/i18n";
  import { makeToast, toastError } from "$lib/toast";
  import OverlayMenu from "./OverlayMenu.svelte";
  import StartupSnippet from "./StartupSnippet.svelte";

  /** 「本机」设置对话框（已知坑 81）。
   *
   *  只暴露四项——本机没有主机 / 端口 / 用户名 / 认证方式可配，所以**不复用**
   *  服务器表单（它的必填校验与提交路径都绑在 `{servers}` 数组上）。启动命令与
   *  编码的语义与服务器表单同名设置完全一致，所以直接复用 `StartupSnippet` 与
   *  `TERMINAL_ENCODINGS` 的标签。 */

  const dispatch = createEventDispatcher<{ close: void }>();

  interface Props {
    open: boolean;
  }

  let { open }: Props = $props();

  let startup = $state("");
  let encoding = $state("utf-8");
  let home = $state("");
  // SOCKS5 直连代理（入站；不走 SSH）。
  let socks5Enabled = $state(false);
  let socks5Port = $state(0);
  let socks5User = $state("");
  let socks5Pass = $state("");
  let saving = $state(false);

  // Reset the draft from the live settings each time the dialog opens (same
  // pattern as Settings.svelte; without the guard the user's edits would be
  // clobbered by any store write while the dialog is open).
  let initDone = false;
  $effect(() => {
    if (open && !initDone) {
      initDone = true;
      const local = $servers.local;
      startup = local.startup ?? "";
      encoding = local.encoding || "utf-8";
      home = local.home ?? "";
      const flat = splitSocks5Tunnel(local.socks5Tunnel);
      socks5Enabled = flat.socks5Enabled;
      socks5User = flat.socks5User;
      socks5Pass = flat.socks5Pass;
      // 端口 0 = 服务端从 10801 起自动分配。**故意**不用 `joinSocks5Tunnel`
      // 的「空 → 1080」兜底：本机直连代理最常见的冲突对象正是另一条远程隧道
      // 占着的 1080，自动分配比一个固定默认值更不容易撞车。
      socks5Port = local.socks5Tunnel?.port ?? 0;
    } else if (!open) {
      initDone = false;
    }
  });

  /** Port input: `oninput` + explicit conversion, never `bind:value` (已知坑 43 ④). */
  function portValue(event: Event): number {
    const raw = (event.currentTarget as HTMLInputElement).value;
    return raw === "" ? 0 : Number(raw) || 0;
  }

  async function save() {
    const next: LocalSettings = {
      startup,
      encoding: encoding || "utf-8",
      home: home.trim(),
      socks5Tunnel: socks5Enabled
        ? {
            port: socks5Port,
            username: socks5User,
            password: socks5Pass,
          }
        : undefined,
    };
    saving = true;
    try {
      await updateLocalSettings(next);
      makeToast({ kind: "success", message: t($lang, "servers.localSaved") });
      dispatch("close");
    } catch (cause) {
      toastError(cause);
    } finally {
      saving = false;
    }
  }
</script>

<OverlayMenu
  title={t($lang, "servers.localTitle")}
  description={t($lang, "servers.localTitleDesc")}
  showCloseButton
  {open}
  on:close={() => dispatch("close")}
>
  <form
    class="flex flex-col gap-4"
    onsubmit={(event) => {
      event.preventDefault();
      void save();
    }}
  >
    <div class="section">
      <StartupSnippet bind:value={startup} />
    </div>

    <div class="section">
      <p class="section-title">{t($lang, "servers.labelEncoding")}</p>
      <select class="input-base" bind:value={encoding}>
        {#each TERMINAL_ENCODINGS as enc (enc)}
          <option value={enc}>{enc}</option>
        {/each}
      </select>
      <p class="section-desc">{t($lang, "servers.localEncodingHint")}</p>
    </div>

    <div class="section">
      <p class="section-title">{t($lang, "servers.localHome")}</p>
      <input
        class="input-base font-mono text-xs"
        placeholder={t($lang, "servers.localHomePlaceholder")}
        bind:value={home}
      />
      <p class="section-desc">{t($lang, "servers.localHomeDesc")}</p>
    </div>

    <!-- SOCKS5 直连代理(入站;不走 SSH) -->
    <div class="section">
      <label class="flex items-center gap-2">
        <input
          type="checkbox"
          class="accent-indigo-500"
          bind:checked={socks5Enabled}
        />
        <p class="section-title">{t($lang, "servers.localSocks5")}</p>
      </label>
      <p class="section-desc">{t($lang, "servers.localSocks5Hint")}</p>
      {#if socks5Enabled}
        <div class="grid grid-cols-3 gap-3">
          <label class="field">
            <span>{t($lang, "servers.socks5Port")}</span>
            <input
              class="input-base"
              type="number"
              min="0"
              max="65535"
              value={socks5Port}
              oninput={(event) => (socks5Port = portValue(event))}
              placeholder={t($lang, "servers.socks5AutoPort")}
            />
          </label>
          <label class="field">
            <span>{t($lang, "servers.socks5User")}</span>
            <input
              class="input-base"
              autocomplete="off"
              bind:value={socks5User}
              placeholder={t($lang, "servers.socks5UserPlaceholder")}
            />
          </label>
          <label class="field">
            <span>{t($lang, "servers.socks5Pass")}</span>
            <input
              class="input-base"
              type="password"
              autocomplete="off"
              bind:value={socks5Pass}
            />
          </label>
        </div>
        {#if !socks5User}
          <p class="section-warn">{t($lang, "servers.localSocks5Warn")}</p>
        {/if}
      {/if}
    </div>

    <div class="flex justify-end gap-2">
      <button
        type="button"
        class="btn-secondary"
        onclick={() => dispatch("close")}>{t($lang, "common.cancel")}</button
      >
      <button type="submit" class="btn-primary" disabled={saving}>
        {t($lang, "common.save")}
      </button>
    </div>
  </form>
</OverlayMenu>

<style lang="postcss">
  .section {
    @apply rounded-lg border border-zinc-800 bg-zinc-800/20 p-3 flex flex-col gap-2;
  }

  .section-title {
    @apply text-sm font-medium text-zinc-200;
  }

  .section-desc {
    @apply text-xs text-zinc-500;
  }

  .section-warn {
    @apply text-xs text-amber-400/90;
  }

  .btn-secondary {
    @apply rounded-md px-3 py-1.5 text-sm text-zinc-300 transition-colors hover:bg-zinc-700;
  }

  :global(select option) {
    background: #1c1c1c;
    color: #e4e4e7;
  }
</style>
