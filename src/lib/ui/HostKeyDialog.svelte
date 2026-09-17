<script lang="ts">
  import Dialog from "./Dialog.svelte";
  import DialogShell from "./DialogShell.svelte";
  import { forgetHostKey } from "$lib/connections";
  import { hostKeyPrompt } from "$lib/hostkey";
  import { lang, t } from "$lib/i18n";
  import { makeToast, toastError } from "$lib/toast";

  /** The single instance is mounted in `+layout.svelte`; every failure path
   *  opens it by setting `hostKeyPrompt` (已知坑 80). Re-trusting is the only
   *  way to clear a recorded fingerprint, and it is security-relevant, so it
   *  always goes through this confirmation — never a one-click toast action. */
  let busy = $state(false);
  let prompt = $derived($hostKeyPrompt);

  function close() {
    hostKeyPrompt.set(null);
  }

  async function retrust() {
    const target = prompt?.target;
    if (!target || busy) return;
    busy = true;
    try {
      await forgetHostKey(target);
      close();
      makeToast({
        kind: "success",
        message: t($lang, "hostkey.done", { target }),
      });
    } catch (err) {
      toastError(err);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog
  open={prompt !== null}
  onClose={close}
  z={95}
  panelClass="w-full max-w-md"
>
  <DialogShell
    title={t($lang, "hostkey.title")}
    confirmText={busy
      ? t($lang, "hostkey.retrusting")
      : t($lang, "hostkey.retrust")}
    danger
    onConfirm={retrust}
    onCancel={close}
  >
    <p class="mt-2 text-sm text-zinc-400">{t($lang, "hostkey.message")}</p>
    {#if prompt?.expected && prompt?.actual}
      <dl
        class="mt-3 space-y-2 rounded-md border border-zinc-700 bg-zinc-950 p-3 text-xs"
      >
        <div>
          <dt class="text-zinc-500">{t($lang, "hostkey.expected")}</dt>
          <dd class="mt-0.5 break-all font-mono text-zinc-300">
            {prompt.expected}
          </dd>
        </div>
        <div>
          <dt class="text-zinc-500">{t($lang, "hostkey.actual")}</dt>
          <dd class="mt-0.5 break-all font-mono text-amber-300">
            {prompt.actual}
          </dd>
        </div>
      </dl>
    {:else if prompt}
      <!-- A terminal / SFTP failure only carries text, and that text already
           names both fingerprints. -->
      <p
        class="mt-3 break-all rounded-md border border-zinc-700 bg-zinc-950 p-3 font-mono text-xs text-zinc-300"
      >
        {prompt.message}
      </p>
    {/if}
    <p class="mt-3 text-xs text-amber-400/90">
      {t($lang, "hostkey.warning")}
    </p>
  </DialogShell>
</Dialog>
