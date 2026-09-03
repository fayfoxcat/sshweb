<script lang="ts">
  import { changeAccessPassword } from "$lib/auth";
  import { lang, setLang, t, type Lang } from "$lib/i18n";
  import { settings, updateSettings } from "$lib/settings";
  import { makeToast, toastError } from "$lib/toast";
  import { enterEscape } from "./shortcuts";
  import OverlayMenu from "./OverlayMenu.svelte";
  import themes, { type ThemeName } from "./themes";
  import { EditIcon, PlusIcon, TrashIcon } from "svelte-feather-icons";
  import {
    createKey,
    deleteKey,
    keys,
    loadKeys,
    renameKey,
    type SshKey,
  } from "$lib/keys";

  export let open: boolean;

  let inputTheme: ThemeName;
  let inputScrollback: number;
  let inputLang: Lang = $lang;
  $: inputLang = $lang;

  let initialized = false;
  $: open, (initialized = false);
  $: if (!initialized) {
    initialized = true;
    inputTheme = $settings.theme;
    inputScrollback = $settings.scrollback;
    inputLang = $lang;
  }

  // ---- Change access password -------------------------------------------
  let curPassword = "";
  let newPassword = "";
  let confirmNewPassword = "";
  let changeBusy = false;
  let changeError = "";

  async function submitPasswordChange() {
    if (!curPassword) {
      changeError = t($lang, "settings.pwdEmpty");
      return;
    }
    if (newPassword.length < 6) {
      changeError = t($lang, "settings.pwdShort");
      return;
    }
    if (newPassword !== confirmNewPassword) {
      changeError = t($lang, "settings.pwdMismatch");
      return;
    }
    changeBusy = true;
    changeError = "";
    try {
      await changeAccessPassword(curPassword, newPassword, confirmNewPassword);
      curPassword = "";
      newPassword = "";
      confirmNewPassword = "";
      makeToast({ kind: "success", message: t($lang, "settings.pwdOk") });
    } catch (cause) {
      changeError = cause instanceof Error ? cause.message : "error";
    } finally {
      changeBusy = false;
    }
  }

  // ---- SSH key management -------------------------------------------------
  /** Draft name for a new key (optional; blank generates an `ssh-…` name). */
  let newKeyName = "";
  /** Key id currently being renamed inline (its name shown in an input). */
  let renameKeyId: string | null = null;
  let renameDraft = "";
  /** Key id armed for inline delete confirmation (second click deletes). */
  let deleteArmedId: string | null = null;
  let keysBusy = false;

  // Refresh the saved-key list each time the settings dialog opens, so keys
  // generated elsewhere (the server form) are reflected here.
  $: if (open) void loadKeys();

  function startRename(key: SshKey) {
    deleteArmedId = null;
    renameKeyId = key.id;
    renameDraft = key.name;
  }

  function cancelRename() {
    renameKeyId = null;
  }

  async function submitRename(key: SshKey) {
    const name = renameDraft.trim();
    if (!name || name === key.name) {
      cancelRename();
      return;
    }
    renameKeyId = null;
    try {
      await renameKey(key.id, name);
      makeToast({
        kind: "success",
        message: t($lang, "settings.keyRenamed", { name }),
      });
    } catch (cause) {
      toastError(cause);
    }
  }

  /** First click arms the row's delete button; a second click deletes. */
  function armDelete(key: SshKey) {
    deleteArmedId = deleteArmedId === key.id ? null : key.id;
  }

  async function confirmDelete(key: SshKey) {
    if (deleteArmedId !== key.id) {
      armDelete(key);
      return;
    }
    deleteArmedId = null;
    try {
      await deleteKey(key.id);
      makeToast({
        kind: "success",
        message: t($lang, "settings.keyDeleted", { name: key.name }),
      });
    } catch (cause) {
      toastError(cause);
    }
  }

  async function generateKey() {
    keysBusy = true;
    try {
      const key = await createKey(newKeyName.trim());
      newKeyName = "";
      makeToast({
        kind: "success",
        message: t($lang, "settings.keyCreated", { name: key.name }),
      });
    } catch (cause) {
      toastError(cause);
    } finally {
      keysBusy = false;
    }
  }
</script>

<OverlayMenu
  title={t($lang, "settings.title")}
  description={t($lang, "settings.description")}
  showCloseButton
  {open}
  on:close
>
  <div class="flex flex-col gap-4">
    <div class="item">
      <div>
        <p class="item-title">{t($lang, "settings.language")}</p>
        <p class="item-subtitle">{t($lang, "settings.languageHint")}</p>
      </div>
      <div class="relative">
        <select
          class="input-base w-52"
          bind:value={inputLang}
          on:change={() => setLang(inputLang)}
        >
          <option value="zh-CN">简体中文</option>
          <option value="en">English</option>
        </select>
      </div>
    </div>
    <div class="item">
      <div>
        <p class="item-title">{t($lang, "settings.theme")}</p>
        <p class="item-subtitle">{t($lang, "settings.themeHint")}</p>
      </div>
      <div class="relative">
        <select
          class="input-base w-52"
          bind:value={inputTheme}
          on:change={() => updateSettings({ theme: inputTheme })}
        >
          {#each Object.keys(themes) as themeName (themeName)}
            <option value={themeName}>{themeName}</option>
          {/each}
        </select>
      </div>
    </div>
    <div class="item">
      <div>
        <p class="item-title">{t($lang, "settings.scrollback")}</p>
        <p class="item-subtitle">{t($lang, "settings.scrollbackHint")}</p>
      </div>
      <div>
        <input
          type="number"
          class="input-base w-52"
          bind:value={inputScrollback}
          on:input={() => {
            if (inputScrollback >= 0) {
              updateSettings({ scrollback: inputScrollback });
            }
          }}
          step="100"
        />
      </div>
    </div>
    <div class="item">
      <div>
        <p class="item-title">{t($lang, "settings.chgPwd")}</p>
        <p class="item-subtitle">{t($lang, "settings.chgPwdHint")}</p>
      </div>
      <div class="flex w-full flex-col gap-2 sm:w-64">
        <input
          type="password"
          class="input-base w-52"
          placeholder={t($lang, "settings.curPwd")}
          autocomplete="current-password"
          bind:value={curPassword}
          use:enterEscape={{
            onEnter: submitPasswordChange,
            preventDefault: true,
          }}
        />
        <input
          type="password"
          class="input-base w-52"
          placeholder={t($lang, "settings.newPwd")}
          autocomplete="new-password"
          bind:value={newPassword}
          use:enterEscape={{
            onEnter: submitPasswordChange,
            preventDefault: true,
          }}
        />
        <input
          type="password"
          class="input-base w-52"
          placeholder={t($lang, "settings.newPwdConfirm")}
          autocomplete="new-password"
          bind:value={confirmNewPassword}
          use:enterEscape={{
            onEnter: submitPasswordChange,
            preventDefault: true,
          }}
        />
        {#if changeError}
          <p class="text-xs text-red-400">{changeError}</p>
        {/if}
        <button
          class="pw-btn"
          on:click={submitPasswordChange}
          disabled={changeBusy}
          title={t($lang, "settings.chgPwd")}
        >
          {changeBusy
            ? t($lang, "settings.changing")
            : t($lang, "settings.changeBtn")}
        </button>
      </div>
    </div>
    <div class="item">
      <div>
        <p class="item-title">{t($lang, "settings.keys")}</p>
        <p class="item-subtitle">{t($lang, "settings.keysHint")}</p>
      </div>
      <div class="flex w-full flex-col gap-2 sm:w-96">
        {#each $keys as key (key.id)}
          <div
            class="flex items-center gap-2 rounded-md border border-zinc-700 bg-zinc-800/50 px-2 py-1.5"
          >
            {#if renameKeyId === key.id}
              <input
                class="input-base min-w-0 flex-1 font-mono text-xs"
                bind:value={renameDraft}
                use:enterEscape={{
                  onEnter: () => submitRename(key),
                  onEscape: cancelRename,
                  preventDefault: true,
                }}
                on:blur={() => {
                  if (renameKeyId === key.id) submitRename(key);
                }}
              />
              <button
                class="icon-btn shrink-0"
                title={t($lang, "common.ok")}
                on:mousedown|stopPropagation
                on:click={() => submitRename(key)}
              >
                <EditIcon size="14" />
              </button>
            {:else}
              <div class="min-w-0 flex-1">
                <p class="truncate text-sm text-zinc-200" title={key.name}>
                  {key.name}
                </p>
                <p class="truncate font-mono text-[10px] text-zinc-500">
                  {key.fingerprint}
                </p>
              </div>
            {/if}
            {#if deleteArmedId === key.id}
              <button
                class="shrink-0 rounded px-1.5 py-0.5 text-xs font-medium text-red-300 transition-colors hover:bg-red-900/50"
                on:click={() => confirmDelete(key)}
              >
                {t($lang, "settings.confirmDeleteKey")}
              </button>
              <button
                class="shrink-0 rounded px-1.5 py-0.5 text-xs text-zinc-400 transition-colors hover:bg-zinc-700"
                on:click={() => (deleteArmedId = null)}
              >
                {t($lang, "common.cancel")}
              </button>
            {:else if renameKeyId !== key.id}
              <button
                class="icon-btn shrink-0"
                title={t($lang, "settings.renameKey")}
                on:click={() => startRename(key)}
              >
                <EditIcon size="14" />
              </button>
              <button
                class="icon-btn shrink-0 hover:!text-red-400"
                title={t($lang, "settings.deleteKey")}
                on:click={() => armDelete(key)}
              >
                <TrashIcon size="14" />
              </button>
            {/if}
          </div>
        {:else}
          <p class="text-xs text-zinc-500">{t($lang, "settings.noKeys")}</p>
        {/each}
        <div class="flex items-center gap-2">
          <input
            class="input-base min-w-0 flex-1 font-mono text-xs"
            placeholder={t($lang, "settings.newKeyPlaceholder")}
            bind:value={newKeyName}
            use:enterEscape={{
              onEnter: generateKey,
              preventDefault: true,
            }}
          />
          <button
            class="shrink-0 rounded-md bg-indigo-700 px-2.5 py-1.5 text-xs font-medium text-white transition-colors hover:bg-indigo-600 disabled:cursor-not-allowed disabled:opacity-50"
            on:click={generateKey}
            disabled={keysBusy}
          >
            <PlusIcon size="13" class="inline -mt-0.5" />
            {t($lang, "settings.genKey")}
          </button>
        </div>
      </div>
    </div>
  </div>

  <!-- svelte-ignore missing-declaration -->
  <p class="mt-6 text-sm text-right text-zinc-400">
    <a target="_blank" rel="noreferrer" href="https://github.com/ekzhang/sshx"
      >sshweb v{__APP_VERSION__}</a
    >
  </p>
</OverlayMenu>

<style lang="postcss">
  .item {
    @apply bg-zinc-800/25 rounded-lg p-4 flex gap-4 flex-col sm:flex-row items-start;
  }

  .item > div:first-child {
    @apply flex-1;
  }

  .item-title {
    @apply font-medium text-zinc-200 mb-1;
  }

  .item-subtitle {
    @apply text-sm text-zinc-400;
  }

  .pw-btn {
    @apply w-full rounded-md bg-indigo-700 px-3 py-2 text-sm font-medium text-white transition-colors hover:bg-indigo-600 disabled:cursor-not-allowed disabled:opacity-50;
  }

  :global(select option) {
    background: #1c1c1c;
    color: #e4e4e7;
  }
</style>
