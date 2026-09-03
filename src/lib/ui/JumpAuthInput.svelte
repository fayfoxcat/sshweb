<script lang="ts">
  import { ChevronDownIcon, KeyIcon } from "svelte-feather-icons";

  import { lang, t } from "$lib/i18n";
  import { noop } from "./a11y";
  import type { SshKey } from "$lib/keys";

  /** Saved keys selectable from the dropdown. */
  export let keys: SshKey[] = [];
  /** Password value for password authentication. */
  export let password = "";
  /** Selected key id (key authentication), or null/undefined for password
   *  mode. The private key itself never reaches the browser. */
  export let keyId: string | null | undefined = null;
  export let placeholder = "";

  let open = false;
  let inputEl: HTMLInputElement;

  $: selectedKey = keyId ? keys.find((k) => k.id === keyId) : undefined;
  /** Input display: the key name in key mode, the password otherwise. */
  $: displayValue = selectedKey ? selectedKey.name : keyId ? keyId : password;
  $: title = keyId
    ? t($lang, "servers.jumpKeyAuth", { name: selectedKey?.name ?? keyId })
    : t($lang, "servers.jumpPwdAuth");

  /** Manual input switches to password authentication. */
  function onInput(event: Event) {
    const value = (event.currentTarget as HTMLInputElement).value;
    keyId = null;
    password = value;
  }

  /** In key mode, select the key name on focus so typing replaces it (and
   *  switches back to password authentication) instead of appending. */
  function onFocus() {
    open = true;
    if (keyId) inputEl.select();
  }

  /** Close the dropdown only when focus leaves the whole widget — not when it
   *  moves between the input and a dropdown item (which would otherwise
   *  unmount the item before its `click` fires). */
  function onWrapperFocusOut(event: FocusEvent) {
    const next = event.relatedTarget;
    const wrapper = event.currentTarget as Node | null;
    if (next instanceof Node && wrapper?.contains(next)) return;
    open = false;
  }

  /** Dropdown selection switches to key authentication. */
  function selectKey(key: SshKey) {
    keyId = key.id;
    // The clicked item unmounts with the dropdown; return focus to the input
    // first — focusing reopens the dropdown via `onFocus`, so close after.
    inputEl?.focus();
    open = false;
  }
</script>

<div class="relative min-w-0 w-full" on:focusout={onWrapperFocusOut}>
  <input
    class="input-base pr-7"
    type={keyId ? "text" : "password"}
    value={displayValue}
    {placeholder}
    {title}
    bind:this={inputEl}
    on:input={onInput}
    on:focus={onFocus}
    on:click={() => (open = true)}
  />
  <ChevronDownIcon
    size="12"
    class="pointer-events-none absolute right-2.5 top-1/2 -translate-y-1/2 text-zinc-500"
  />
  {#if open}
    <div
      class="no-scrollbar absolute left-0 right-0 top-full z-20 mt-1 max-h-48 overflow-y-auto rounded-md border border-zinc-700 bg-zinc-900 py-1 shadow-lg"
      on:mousedown|stopPropagation
      on:click|stopPropagation
      on:keydown={noop}
    >
      {#if keys.length === 0}
        <p class="px-3 py-1.5 text-xs text-zinc-500">
          {t($lang, "servers.noKeys")}
        </p>
      {:else}
        {#each keys as key (key.id)}
          <button
            type="button"
            class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs text-zinc-300 transition-colors hover:bg-zinc-800"
            on:click={() => selectKey(key)}
          >
            <KeyIcon size="12" class="shrink-0 text-zinc-400" />
            <span class="min-w-0 flex-1 truncate">{key.name}</span>
            <span class="shrink-0 font-mono text-[10px] text-zinc-500">
              {key.fingerprint}
            </span>
          </button>
        {/each}
      {/if}
    </div>
  {/if}
</div>
