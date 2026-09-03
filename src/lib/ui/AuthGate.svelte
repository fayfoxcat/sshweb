<script lang="ts">
  import { onMount } from "svelte";

  import {
    authStatus,
    changeAccessPassword,
    fetchAuthStatus,
    login,
  } from "$lib/auth";
  import { lang, t } from "$lib/i18n";

  let ready = false;
  let busy = false;
  let password = "";
  let confirmation = "";
  let error = "";

  onMount(async () => {
    try {
      await fetchAuthStatus();
      ready = true;
    } catch (cause) {
      error =
        cause instanceof Error ? cause.message : t($lang, "auth.errConnect");
      authStatus.update((status) => ({ ...status, loading: false }));
    }
  });

  /** Login (normal or setup-key) — single password/key field. */
  async function submitLogin() {
    if (!password) {
      error = t($lang, "auth.empty");
      return;
    }
    busy = true;
    error = "";
    try {
      await login(password);
      password = "";
      ready = true;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : "error";
    } finally {
      busy = false;
    }
  }

  /** Forced first-password change after a setup-key login. */
  async function submitForceChange() {
    if (!password) {
      error = t($lang, "auth.empty");
      return;
    }
    if (password !== confirmation) {
      error = t($lang, "auth.mismatch");
      return;
    }
    busy = true;
    error = "";
    try {
      await changeAccessPassword("", password, confirmation);
      password = "";
      confirmation = "";
      ready = true;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : "error";
    } finally {
      busy = false;
    }
  }
</script>

{#if ready && $authStatus.authenticated && !$authStatus.pendingChange}
  <slot />
{:else}
  <main
    class="flex min-h-screen items-center justify-center bg-zinc-950 px-4 text-zinc-100"
  >
    <section
      class="w-full max-w-md rounded-xl border border-zinc-800 bg-zinc-900 p-6 shadow-2xl"
    >
      {#if $authStatus.loading}
        <p class="text-center text-sm text-zinc-400">
          {t($lang, "auth.loading")}
        </p>
      {:else}
        {#if $authStatus.authenticated && $authStatus.pendingChange}
          <!-- Forced first-password change after a setup-key login -->
          <div class="mb-6 text-center">
            <h1 class="text-xl font-medium">{t($lang, "auth.forceTitle")}</h1>
            <p class="mt-2 text-sm text-zinc-400">
              {t($lang, "auth.forceHint")}
            </p>
          </div>
          <form
            class="flex flex-col gap-4"
            on:submit|preventDefault={submitForceChange}
          >
            <label class="field">
              <span>{t($lang, "auth.password")}</span>
              <input
                class="input-auth"
                type="password"
                autocomplete="new-password"
                bind:value={password}
              />
            </label>
            <label class="field">
              <span>{t($lang, "auth.confirm")}</span>
              <input
                class="input-auth"
                type="password"
                autocomplete="new-password"
                bind:value={confirmation}
              />
            </label>
            <p class="text-xs text-zinc-500">{t($lang, "auth.minLength")}</p>
            {#if error}
              <p
                class="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300"
              >
                {error}
              </p>
            {/if}
            <button class="btn-primary" type="submit" disabled={busy}>
              {busy
                ? t($lang, "auth.processing")
                : t($lang, "auth.forceBtn")}
            </button>
          </form>
        {:else}
          <!-- Login: setup key on a fresh install, access password otherwise -->
          <div class="mb-6 text-center">
            <h1 class="text-xl font-medium">
              {$authStatus.setup
                ? t($lang, "auth.loginTitle")
                : t($lang, "auth.setupKeyTitle")}
            </h1>
            <p class="mt-2 text-sm text-zinc-400">
              {$authStatus.setup
                ? t($lang, "auth.loginHint")
                : t($lang, "auth.setupKeyHint")}
            </p>
          </div>
          <form
            class="flex flex-col gap-4"
            on:submit|preventDefault={submitLogin}
          >
            <!-- Hidden username field: Chromium warns about password-only forms
                 with no username input for accessibility. -->
            <input
              class="hidden"
              type="text"
              name="username"
              autocomplete="username"
              tabindex="-1"
              aria-hidden="true"
            />
            <label class="field">
              <span>{$authStatus.setup
                ? t($lang, "auth.password")
                : t($lang, "auth.setupKey")}</span>
              <input
                class="input-auth"
                type="password"
                autocomplete={$authStatus.setup
                  ? "current-password"
                  : "off"}
                bind:value={password}
              />
            </label>
            {#if error}
              <p
                class="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300"
              >
                {error}
              </p>
            {/if}
            <button class="btn-primary" type="submit" disabled={busy}>
              {busy
                ? t($lang, "auth.processing")
                : $authStatus.setup
                ? t($lang, "auth.login")
                : t($lang, "auth.setupKeyBtn")}
            </button>
          </form>
        {/if}
      {/if}
    </section>
  </main>
{/if}

<style lang="postcss">
  .field {
    @apply flex flex-col gap-1 text-sm text-zinc-300;
  }

  /* Login / setup inputs: solid dark background on the centered auth card
     (distinct from the shared `.input-base` used in the app panels). */
  .input-auth {
    @apply w-full rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 outline-none transition-colors;
    @apply focus:ring-2 focus:ring-indigo-500/50;
  }

  .btn-primary {
    @apply rounded-md bg-indigo-700 px-3 py-2 text-sm font-medium text-white transition-colors hover:bg-indigo-600 disabled:cursor-not-allowed disabled:opacity-50;
  }
</style>
