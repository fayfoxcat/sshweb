<script lang="ts">
  import { createEventDispatcher, onMount } from "svelte";
  import {
    ChevronDownIcon,
    CopyIcon,
    EditIcon,
    FolderIcon,
    HardDriveIcon,
    PlusIcon,
    ServerIcon,
    TerminalIcon,
    TrashIcon,
    ZapIcon,
  } from "svelte-feather-icons";

  import type { WsServerConfig } from "$lib/protocol";
  import { DEFAULT_SSH_PORT, DEFAULT_SOCKS_PORT } from "$lib/constants";
  import {
    addServer,
    deleteServer,
    duplicateServer,
    effectivePassword,
    joinProxy,
    joinSocks5Tunnel,
    moveServer,
    servers,
    serverTargetKey,
    splitProxy,
    splitSocks5Tunnel,
    testServerConnection,
    toWsServerConfig,
    updateServer,
    type ServerConfig,
    type ServerInput,
    type ProxyFormFields,
    type Socks5FormFields,
  } from "$lib/connections";
  import { TERMINAL_ENCODINGS } from "$lib/encoding";
  import { lang, t } from "$lib/i18n";
  import { createKey, installKey, keys } from "$lib/keys";
  import { makeToast, toastError } from "$lib/toast";
  import { copyText } from "$lib/clipboard";
  import {
    loadProxies,
    proxies,
    startProxy,
    stopProxy,
    type ProxyStatus,
  } from "$lib/proxies";
  import { sidebarResize } from "./dragResize";
  import { createReorderDnd, draggable, droppable } from "./dnd";
  import OverlayMenu from "./OverlayMenu.svelte";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import PromptDialog from "./PromptDialog.svelte";
  import JumpAuthInput from "./JumpAuthInput.svelte";
  import StartupSnippet from "./StartupSnippet.svelte";
  import Sidebar from "./Sidebar.svelte";

  const dispatch = createEventDispatcher<{
    connect: WsServerConfig;
    openSftp: string;
    connectLocal: void;
    openLocalSftp: void;
  }>();
  /** Available MAC (message authentication code) algorithms. */
  const MAC_ALGORITHMS = [
    "hmac-sha2-512-etm",
    "hmac-sha2-256-etm",
    "hmac-sha2-512",
    "hmac-sha2-256",
    "hmac-sha1",
    "hmac-sha1-96",
    "hmac-md5",
  ];

  /** Available terminal encodings. */
  const ENCODINGS = TERMINAL_ENCODINGS;

  /** 密钥下拉框可用宽度(经 `bind:clientWidth` 绑定),用于按宽度截断指纹。 */
  let keySelectWidth = 0;
  /** 按下拉框宽度动态计算指纹可显示字符数(扣除内边距与下拉箭头空间)。 */
  $: keyFpMax = Math.max(8, Math.floor((keySelectWidth - 48) / 7));

  /** Compact fingerprint for the key dropdown. A native `<select>` sizes its
   *  open list to the widest option, so a full `SHA256:…` fingerprint would
   *  widen the popup far past the box. Truncate to the space the closed box
   *  actually has(`max` 来自 `keyFpMax`,按宽度测量)。 */
  function shortFingerprint(fp: string, max: number): string {
    return fp.length > max ? `${fp.slice(0, max)}…` : fp;
  }

  // Form dialog state for adding/editing a server.
  let editing: ServerConfig | null = null;
  let formOpen = false;
  /** The add/edit form. Reuses `ServerInput` for the shared connection
   *  fields (so a new persisted field cannot drift); the proxy is split into
   *  an enabled flag plus one field per input and reassembled by
   *  `buildInput()`. The `Record<string, any>` intersection lets the
   *  descriptor-driven field grids index `form[spec.key]`. */
  let form: ServerForm & Record<string, any> = blankForm();
  let macMenuOpen = false;
  let keyNameOpen = false;
  let installPwdOpen = false;
  let testing = false;

  /** Add/edit form state. The proxy / SOCKS5 tunnel objects are split into
   *  per-input fields (`ProxyFormFields` / `Socks5FormFields`) and reassembled
   *  by `buildInput()` via `joinProxy` / `joinSocks5Tunnel`. */
  type ServerForm = Omit<ServerInput, "proxy" | "socks5Tunnel"> &
    ProxyFormFields &
    Socks5FormFields;

  /** Descriptor for a plain add/edit form field, rendered by the `{#each}`
   *  grids below. `key` binds `form[key]` (text/number/password inputs);
   *  `select` picks one of the fixed-option selects. */
  type FieldSpec = {
    key?: string;
    labelKey: string;
    type?: "text" | "number" | "password";
    placeholder?: string;
    placeholderKey?: string;
    min?: number;
    max?: number;
    select?: "auth" | "encoding" | "proxyKind";
  };

  /** Read an input's current value for the descriptor-driven field grids
   *  (replaces `bind:value`, which cannot combine with a dynamic `type`).
   *  Number fields keep a numeric value (Svelte's native `bind:value` coerces
   *  `<input type="number">` to a number; without this the serialized
   *  `port`/`proxyPort`/`socks5Port` would be strings and the server's
   *  `u16` deserialization would reject the payload with 400). */
  function inputValue(event: Event, spec: FieldSpec): string | number {
    const value = (event.currentTarget as HTMLInputElement).value;
    return spec.type === "number" ? Number(value) : value;
  }

  /** Basic connection fields (name/host/port + user/authMethod/encoding),
   *  laid out as two 3-column rows. */
  const BASIC_FIELDS: FieldSpec[] = [
    {
      key: "name",
      labelKey: "servers.labelName",
      placeholderKey: "servers.phName",
    },
    { key: "host", labelKey: "servers.labelHost", placeholder: "192.168.1.10" },
    {
      key: "port",
      labelKey: "servers.labelPort",
      type: "number",
      min: 1,
      max: 65535,
    },
    { key: "username", labelKey: "servers.labelUser", placeholder: "root" },
    { select: "auth", labelKey: "servers.authMethod" },
    { select: "encoding", labelKey: "servers.labelEncoding" },
  ];

  /** 连接代理 (出站) 字段:类型/主机/端口 + 用户名/密码。 */
  const PROXY_FIELDS: FieldSpec[] = [
    { select: "proxyKind", labelKey: "servers.proxyType" },
    {
      key: "proxyHost",
      labelKey: "servers.labelHost",
      placeholder: "127.0.0.1",
    },
    {
      key: "proxyPort",
      labelKey: "servers.labelPort",
      type: "number",
      min: 1,
      max: 65535,
    },
    { key: "proxyUser", labelKey: "servers.proxyUser" },
    { key: "proxyPass", labelKey: "servers.proxyPass", type: "password" },
  ];

  /** SOCKS5 隧道 (入站) 字段:端口/用户名/密码。 */
  const SOCKS5_FIELDS: FieldSpec[] = [
    {
      key: "socks5Port",
      labelKey: "servers.socks5Port",
      type: "number",
      min: 1,
      max: 65535,
      placeholder: String(DEFAULT_SOCKS_PORT),
    },
    {
      key: "socks5User",
      labelKey: "servers.socks5User",
      placeholderKey: "servers.socks5UserPlaceholder",
    },
    { key: "socks5Pass", labelKey: "servers.socks5Pass", type: "password" },
  ];

  /** Blank form values for adding a new server. */
  function blankForm(): ServerForm {
    return {
      name: "",
      host: "",
      port: DEFAULT_SSH_PORT,
      username: "",
      password: "",
      encoding: "utf-8",
      hosts: [],
      ...splitProxy(null),
      ...splitSocks5Tunnel(undefined),
      macs: [...MAC_ALGORITHMS],
      startup: "",
      authMethod: "password",
      keyId: "",
    };
  }

  function startAdd() {
    editing = null;
    form = blankForm();
    formOpen = true;
  }

  /** Validate the form's required connection fields; returns an error toast
   *  message or `null` when valid. Shared by install / test / submit so the
   *  host-user and key-required checks stay in one place. */
  function formError(): string | null {
    if (!form.host || !form.username) {
      return t($lang, "servers.errHostUser");
    }
    if (form.authMethod === "key" && !form.keyId) {
      return t($lang, "servers.errKeyRequired");
    }
    return null;
  }

  function startEdit(server: ServerConfig) {
    editing = server;
    form = {
      ...blankForm(),
      name: server.name,
      host: server.host,
      port: server.port,
      username: server.username,
      password: server.password,
      encoding: server.encoding || "utf-8",
      hosts: server.hosts ?? [],
      ...splitProxy(server.proxy),
      ...splitSocks5Tunnel(server.socks5Tunnel),
      macs: (server.macs ?? []).length > 0 ? server.macs : [...MAC_ALGORITHMS],
      startup: server.startup ?? "",
      authMethod: (server.authMethod as "password" | "key") || "password",
      keyId: server.keyId ?? "",
    };
    formOpen = true;
  }

  function cancelForm() {
    formOpen = false;
    editing = null;
  }

  /** Toggle a MAC algorithm in the selection, preserving order. */
  function toggleMac(algo: string) {
    if (form.macs.includes(algo)) {
      form.macs = form.macs.filter((m) => m !== algo);
    } else {
      form.macs = [...form.macs, algo];
    }
  }

  /** Collect the current form values into a server input (also used as the
   *  wire format for the one-click install). */
  function buildInput(): ServerInput {
    return {
      name: form.name,
      host: form.host,
      port: form.port,
      username: form.username,
      password: form.password,
      encoding: form.encoding,
      hosts: form.hosts,
      proxy: joinProxy(form),
      socks5Tunnel: joinSocks5Tunnel(form),
      macs: form.macs,
      startup: form.startup,
      authMethod: form.authMethod,
      keyId: form.keyId,
    };
  }

  /** Copy the selected key's public part to the clipboard. */
  async function copyPublicKey() {
    const key = $keys.find((k) => k.id === form.keyId);
    if (!key) {
      makeToast({ kind: "error", message: t($lang, "servers.errKeyRequired") });
      return;
    }
    const ok = await copyText(key.publicKey);
    makeToast({
      kind: ok ? "success" : "error",
      message: ok
        ? t($lang, "servers.keyCopyOk")
        : t($lang, "servers.keyCopyFail"),
    });
  }

  /** Generate a new server-side key and select it. */
  async function doGenerateKey(raw: string) {
    keyNameOpen = false;
    try {
      const key = await createKey(raw.trim());
      form.keyId = key.id;
      makeToast({
        kind: "success",
        message: t($lang, "servers.keyGenOk", { name: key.name }),
      });
    } catch (err) {
      makeToast({
        kind: "error",
        message: t($lang, "servers.keyGenFail", {
          error: (err as Error).message,
        }),
      });
    }
  }

  /** Install the selected key's public part onto the target server. The
   *  connection reuses the form's CURRENT authentication — the selected saved
   *  key (key mode) or the password — so any credential that can reach the
   *  server works, regardless of which key is being installed. For password
   *  mode `password` is merged in (the saved server password, or the one typed
   *  in the prompt); key mode connects with the form's `keyId` directly. */
  async function doInstall(password: string) {
    installPwdOpen = false;
    try {
      const input = buildInput();
      if (input.authMethod !== "key" && password) {
        input.password = password;
      }
      const message = await installKey(input, form.keyId);
      makeToast({ kind: "success", message });
    } catch (err) {
      toastError(err);
    }
  }

  /** Toast the form's validation error and report whether it is invalid.
   *  Shared by install / test / submit so the guard stays in one place. */
  function guardForm(): boolean {
    const error = formError();
    if (error) {
      makeToast({ kind: "error", message: error });
      return true;
    }
    return false;
  }

  /** The effective SSH password: the form's value, falling back to the saved
   *  server's when editing (a blank form password preserves the saved one).
   *  Delegates to the shared rule in `connections.ts` (same as `updateServer`). */
  function resolvedPassword(): string {
    return effectivePassword(form.password, editing?.password ?? "");
  }

  /** Toast "SSH 密码必填" and report whether a password-mode form is missing
   *  one. Only used where a password is truly required (new-server submit and
   *  connection tests); an edit may leave the field blank to keep the saved
   *  password. */
  function guardPassword(): boolean {
    if (form.authMethod === "password" && !resolvedPassword()) {
      makeToast({ kind: "error", message: t($lang, "servers.errPassword") });
      return true;
    }
    return false;
  }

  function startInstall() {
    if (guardForm()) return;
    // Key mode: connect with the currently selected saved key — no password
    // needed to bootstrap the install.
    if (form.authMethod === "key") {
      doInstall("");
      return;
    }
    const saved = resolvedPassword();
    if (saved) {
      doInstall(saved);
    } else {
      installPwdOpen = true;
    }
  }

  /** Test the current unsaved form values against the target server. When
   *  editing and the password field is left blank, falls back to the saved
   *  server's password (matching what a save would persist). */
  async function testConnection() {
    if (guardForm()) return;
    const cfg = buildInput();
    if (form.authMethod === "password") {
      if (guardPassword()) return;
      cfg.password = resolvedPassword();
    }
    testing = true;
    try {
      const message = await testServerConnection(cfg);
      makeToast({ kind: "success", message });
    } catch (err) {
      toastError(err);
    } finally {
      testing = false;
    }
  }

  async function submitForm() {
    if (guardForm()) return;
    if (!editing && guardPassword()) return;
    const input = buildInput();
    if (editing) {
      await updateServer(editing.id, input);
      makeToast({
        kind: "success",
        message: t($lang, "servers.updated", { name: form.name }),
      });
    } else {
      await addServer(input);
      makeToast({
        kind: "success",
        message: t($lang, "servers.added", { name: form.name }),
      });
    }
    cancelForm();
  }

  function connectTo(server: ServerConfig) {
    dispatch("connect", toWsServerConfig(server));
  }

  function browseSftp(server: ServerConfig) {
    dispatch("openSftp", server.id);
  }

  let deleteTarget: ServerConfig | null = null;

  function requestDelete(server: ServerConfig) {
    deleteTarget = server;
  }

  async function confirmDelete() {
    if (!deleteTarget) return;
    const target = deleteTarget;
    await deleteServer(target.id);
    makeToast({
      kind: "info",
      message: t($lang, "servers.deleted", { name: target.name }),
    });
    deleteTarget = null;
  }

  /** Duplicate the server, creating a copy with a "副本" suffix. */
  async function copyServer(server: ServerConfig) {
    const copy = await duplicateServer(server.id);
    if (copy) {
      makeToast({
        kind: "success",
        message: t($lang, "servers.duplicated", { name: copy.name }),
      });
    }
  }

  // ---- SOCKS5 隧道开关(列表快捷启停,运行时仅内存) ------------------------
  /** 该服务器的隧道状态(运行中返回其端口)。 */
  function proxyOf(server: ServerConfig): ProxyStatus | undefined {
    return $proxies.find((p) => p.serverKey === serverTargetKey(server));
  }

  /** 未配置过隧道端口(无 `socks5Tunnel`)的服务器:⚡ 按钮禁用。 */
  function proxyDisabled(server: ServerConfig): boolean {
    return !server.socks5Tunnel;
  }

  /** 切换该服务器的 SOCKS5 隧道:已开启则停止,否则启动(配置页端口或自动分配)。 */
  async function toggleProxy(server: ServerConfig) {
    if (proxyDisabled(server)) return;
    const status = proxyOf(server);
    if (status) {
      try {
        await stopProxy(status.serverKey);
        makeToast({
          kind: "info",
          message: t($lang, "servers.socks5Stopped", { name: server.name }),
        });
      } catch (err) {
        toastError(err);
      }
      return;
    }
    try {
      const started = await startProxy(
        toWsServerConfig(server),
        // 配置页端口偏好(默认 1080);未配置则自动分配。
        server.socks5Tunnel?.port ?? DEFAULT_SOCKS_PORT,
      );
      makeToast({
        kind: "success",
        message: t($lang, "servers.socks5Started", {
          name: server.name,
          port: started.port,
        }),
      });
    } catch (err) {
      toastError(err);
    }
  }

  // 面板每次打开时刷新隧道运行状态(开启/关闭只由列表 ⚡ 开关控制)。
  onMount(() => {
    void loadProxies().catch(() => {});
  });

  // ---- Drag-to-reorder ---------------------------------------------------
  const {
    over: serversOver,
    start: serversDragStart,
    end: serversDragEnd,
    overTarget: serversDragOver,
    leave: serversDragLeave,
    drop: serversDragDrop,
  } = createReorderDnd<string>();

  async function onRowDrop(server: ServerConfig) {
    const from = serversDragDrop();
    if (from !== null && from !== server.id) {
      await moveServer(from, server.id);
    }
  }
</script>

<Sidebar resize={sidebarResize} open showHandle={true}>
  <!-- Header -->
  <div class="flex items-center gap-2 border-b border-zinc-800 px-3 py-2">
    <ServerIcon size="16" class="text-zinc-400" />
    <span class="flex-1 text-sm font-medium text-zinc-200"
      >{t($lang, "servers.title")}</span
    >
    <button
      class="icon-btn"
      on:click={startAdd}
      title={t($lang, "servers.addTitle")}
    >
      <PlusIcon size="16" />
    </button>
  </div>

  <!-- Server list: first entry is always the machine sshweb-server runs on.
       It cannot be edited, copied or removed. -->
  <div class="no-scrollbar flex-1 overflow-y-auto p-2">
    <div class="flex flex-col gap-2">
      <div
        class="flex items-center gap-2 rounded-md border border-emerald-900/40 bg-zinc-900 px-2.5 py-2"
        title={t($lang, "servers.localDesc")}
        on:dblclick={() => dispatch("connectLocal")}
      >
        <HardDriveIcon size="16" class="shrink-0 text-zinc-400" />
        <div class="min-w-0 flex-1">
          <p class="truncate text-sm text-zinc-200">
            {t($lang, "servers.local")}
          </p>
          <p class="truncate text-xs text-zinc-500">
            {t($lang, "servers.localDesc")}
          </p>
        </div>
        <div
          class="flex shrink-0 items-center gap-0.5"
          on:dblclick|stopPropagation
        >
          <button
            class="icon-btn-sm text-sky-400 hover:bg-sky-900/40"
            title={t($lang, "servers.newLocal")}
            on:click={() => dispatch("connectLocal")}
          >
            <TerminalIcon size="14" />
          </button>
          <button
            class="icon-btn-sm text-yellow-400 hover:bg-yellow-900/40"
            title={t($lang, "servers.openLocal")}
            on:click={() => dispatch("openLocalSftp")}
          >
            <FolderIcon size="14" />
          </button>
        </div>
      </div>

      {#if $servers.servers.length === 0}
        <div class="p-4 text-center text-sm text-zinc-500">
          {t($lang, "servers.empty")}
        </div>
      {:else}
        {#each $servers.servers as server (server.id)}
          {@const proxy = proxyOf(server)}
          <div
            class="group flex items-center gap-2 rounded-md border border-zinc-800 bg-zinc-900 px-2.5 py-2 transition-colors hover:bg-zinc-800 {$serversOver ===
            server.id
              ? 'border-indigo-500/70 shadow-[inset_2px_0_0_0_#6366f1]'
              : ''}"
            use:droppable={{
              onDragOver: () => serversDragOver(server.id),
              onDrop: () => {
                void onRowDrop(server);
              },
              onDragLeave: serversDragLeave,
            }}
            on:dblclick={() => connectTo(server)}
          >
            <span
              draggable="true"
              class="cursor-grab text-zinc-500 transition-colors hover:text-indigo-300 active:cursor-grabbing"
              title={t($lang, "servers.sortHint")}
              use:draggable={{
                key: server.id,
                onStart: () => serversDragStart(server.id),
                onEnd: serversDragEnd,
              }}
            >
              <ServerIcon size="16" class="shrink-0" />
            </span>
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-1.5">
                <p class="truncate text-sm text-zinc-200">{server.name}</p>
              </div>
              <p class="truncate text-xs text-zinc-500">
                {server.username}@{server.host}:{server.port}
              </p>
            </div>
            <div
              class="flex shrink-0 items-center gap-0.5"
              on:dblclick|stopPropagation
            >
              <button
                class="icon-btn-sm text-sky-400 hover:bg-sky-900/40"
                title={t($lang, "servers.connect")}
                on:click={() => connectTo(server)}
              >
                <TerminalIcon size="14" />
              </button>
              <button
                class="icon-btn-sm text-yellow-400 hover:bg-yellow-900/40"
                title={t($lang, "servers.ftp")}
                on:click={() => browseSftp(server)}
              >
                <FolderIcon size="14" />
              </button>
              <button
                class="icon-btn-sm"
                title={t($lang, "servers.edit")}
                on:click={() => startEdit(server)}
              >
                <EditIcon size="14" />
              </button>
              <button
                class="icon-btn-sm"
                title={t($lang, "servers.copy")}
                on:click={() => copyServer(server)}
              >
                <CopyIcon size="14" />
              </button>
              <!-- ⚡ 隧道开关:外层 span 保证 disabled 时悬停提示仍可见(浏览器
                   不向 disabled 元素派发鼠标事件,title 不会显示)。 -->
              <span
                title={proxy
                  ? t($lang, "servers.socks5StopAt", { port: proxy.port })
                  : proxyDisabled(server)
                  ? t($lang, "servers.socks5NeedConfig")
                  : t($lang, "servers.socks5Start")}
                class="inline-flex"
              >
                <button
                  class="icon-btn-sm {proxyDisabled(server)
                    ? 'cursor-not-allowed opacity-40'
                    : proxy
                    ? '!text-emerald-300 !bg-emerald-900/40 hover:!bg-emerald-900/60'
                    : ''}"
                  disabled={proxyDisabled(server)}
                  on:click={() => toggleProxy(server)}
                >
                  <ZapIcon size="14" />
                </button>
              </span>
              <button
                class="icon-btn-sm text-red-400 hover:bg-red-900/40"
                title={t($lang, "servers.delete")}
                on:click={() => requestDelete(server)}
              >
                <TrashIcon size="14" />
              </button>
            </div>
          </div>
        {/each}
      {/if}
    </div>
  </div>
</Sidebar>

<!-- Add / edit server form dialog -->
<OverlayMenu
  title={editing ? t($lang, "servers.editTitle") : t($lang, "servers.addTitle")}
  description={t($lang, "servers.formDesc")}
  showCloseButton
  maxWidth={672}
  open={formOpen}
  on:close={cancelForm}
>
  <div class="flex flex-col gap-4">
    <!-- 基础连接信息(名称 ~ 密码/密钥) -->
    <div class="section">
      <!-- 名称 / 主机 / 端口 + 用户名 / 认证方式 / 编码(两个 3 列行) -->
      <div class="grid grid-cols-3 gap-3">
        {#each BASIC_FIELDS as f (f.labelKey)}
          {@const key = f.key}
          <label class="field">
            <span>{t($lang, f.labelKey)}</span>
            {#if f.select === "auth"}
              <select class="input-base" bind:value={form.authMethod}>
                <option value="password"
                  >{t($lang, "servers.authPassword")}</option
                >
                <option value="key">{t($lang, "servers.authKey")}</option>
              </select>
            {:else if f.select === "encoding"}
              <select class="input-base" bind:value={form.encoding}>
                {#each ENCODINGS as enc}
                  <option value={enc}>{enc}</option>
                {/each}
              </select>
            {:else if key}
              <input
                class="input-base"
                type={f.type ?? "text"}
                value={form[key]}
                on:input={(e) => (form[key] = inputValue(e, f))}
                min={f.min}
                max={f.max}
                placeholder={f.placeholder ??
                  (f.placeholderKey ? t($lang, f.placeholderKey) : undefined)}
              />
            {/if}
          </label>
        {/each}
      </div>

      {#if form.authMethod === "password"}
        <label class="field">
          <span>{t($lang, "servers.labelPassword")}</span>
          <input
            class="input-base"
            type="password"
            bind:value={form.password}
            placeholder={editing
              ? t($lang, "servers.keepPwd")
              : t($lang, "servers.sshPwd")}
          />
        </label>
      {:else}
        <!-- 密钥认证:复用已生成的服务器端密钥 -->
        <div class="flex flex-col gap-2">
          <div class="flex items-end gap-2">
            <label class="field min-w-0 flex-1">
              <span>{t($lang, "servers.labelKey")}</span>
              <select
                class="input-base"
                bind:value={form.keyId}
                bind:clientWidth={keySelectWidth}
              >
                <option value="">{t($lang, "servers.selectKey")}</option>
                {#each $keys as k}
                  <option value={k.id}
                    >{k.name} · {shortFingerprint(
                      k.fingerprint,
                      keyFpMax,
                    )}</option
                  >
                {/each}
              </select>
            </label>
            <button
              class="btn-mini"
              on:click={copyPublicKey}
              title={t($lang, "servers.copyPubkey")}
              >{t($lang, "servers.copyPubkey")}</button
            >
            <button
              class="btn-mini"
              on:click={() => (keyNameOpen = true)}
              title={t($lang, "servers.genKey")}
              >{t($lang, "servers.genKey")}</button
            >
            <button
              class="btn-mini"
              on:click={startInstall}
              title={t($lang, "servers.installKey")}
              >{t($lang, "servers.installKey")}</button
            >
          </div>
          {#if $keys.length === 0}
            <p class="text-xs text-zinc-500">{t($lang, "servers.noKeys")}</p>
          {/if}
        </div>
      {/if}
    </div>

    <!-- 启动命令 (每行一条,终端启动后执行) -->
    <div class="section">
      <StartupSnippet bind:value={form.startup} />
    </div>

    <!-- 校验算法 (MAC) - 下拉多选 -->
    <div class="section">
      <p class="section-title">{t($lang, "servers.mac")}</p>
      <div class="relative">
        <button
          type="button"
          class="input-base flex items-center pr-8"
          on:click={() => (macMenuOpen = !macMenuOpen)}
        >
          <span class="min-w-0 flex-1 truncate">
            {form.macs.length === MAC_ALGORITHMS.length
              ? t($lang, "servers.macAll")
              : form.macs.length === 0
              ? t($lang, "servers.macDefault")
              : t($lang, "servers.macSelected", { n: form.macs.length })}
          </span>
        </button>
        <ChevronDownIcon
          size="14"
          class="pointer-events-none absolute right-2.5 top-1/2 -translate-y-1/2 text-zinc-500"
        />
        {#if macMenuOpen}
          <div
            class="absolute left-0 right-0 top-full z-20 mt-1 max-h-48 overflow-y-auto rounded-md border border-zinc-700 bg-zinc-900 py-1 shadow-lg no-scrollbar"
          >
            {#each MAC_ALGORITHMS as algo}
              <label
                class="flex cursor-pointer items-center gap-2 px-3 py-1 text-sm text-zinc-300 hover:bg-zinc-800"
              >
                <input
                  type="checkbox"
                  class="accent-indigo-500"
                  checked={form.macs.includes(algo)}
                  on:change={() => toggleMac(algo)}
                />
                <span class="truncate font-mono text-xs">{algo}</span>
              </label>
            {/each}
          </div>
        {/if}
      </div>
    </div>
    <!-- 主机链 (Host chaining / ProxyJump) -->
    <div class="section">
      <div class="flex items-center justify-between">
        <div>
          <p class="section-title">{t($lang, "servers.chain")}</p>
          <p class="section-desc">{t($lang, "servers.chainDesc")}</p>
        </div>
        <button
          class="btn-secondary"
          on:click={() =>
            (form.hosts = [
              ...form.hosts,
              {
                host: "",
                port: DEFAULT_SSH_PORT,
                username: "",
                password: "",
              },
            ])}
        >
          {t($lang, "servers.chainAdd")}
        </button>
      </div>
      {#if form.hosts.length > 0}
        {#each form.hosts as jump, i (i)}
          <div class="jump-row">
            <input
              class="input-base min-w-0 flex-[3]"
              placeholder={t($lang, "servers.labelHost")}
              bind:value={jump.host}
            />
            <input
              class="input-base !w-[5.3rem] shrink-0"
              type="number"
              placeholder={t($lang, "servers.labelPort")}
              bind:value={jump.port}
            />
            <input
              class="input-base min-w-0 flex-[2]"
              placeholder={t($lang, "servers.labelUser")}
              bind:value={jump.username}
            />
            <div class="min-w-0 flex-[3]">
              <JumpAuthInput
                keys={$keys}
                bind:password={jump.password}
                bind:keyId={jump.keyId}
                placeholder={t($lang, "servers.jumpAuthPlaceholder")}
              />
            </div>
            <button
              class="icon-btn-sm shrink-0 text-red-400"
              on:click={() =>
                (form.hosts = form.hosts.filter((_, j) => j !== i))}
              title={t($lang, "servers.chainRemove")}
            >
              <TrashIcon size="14" />
            </button>
          </div>
        {/each}
      {:else}
        <p class="text-xs text-zinc-600">{t($lang, "servers.chainEmpty")}</p>
      {/if}
    </div>

    <!-- 连接代理(出站:经代理连目标服务器) -->
    <div class="section">
      <label class="flex items-center gap-2">
        <input
          type="checkbox"
          class="accent-indigo-500"
          bind:checked={form.proxyEnabled}
        />
        <p class="section-title">{t($lang, "servers.proxy")}</p>
      </label>
      <p class="section-desc">{t($lang, "servers.proxyHint")}</p>
      {#if form.proxyEnabled}
        <div class="grid grid-cols-3 gap-3">
          {#each PROXY_FIELDS.slice(0, 3) as f (f.labelKey)}
            {@const key = f.key}
            <label class="field">
              <span>{t($lang, f.labelKey)}</span>
              {#if f.select === "proxyKind"}
                <select class="input-base" bind:value={form.proxyKind}>
                  <option value="http">HTTP</option>
                  <option value="socks5">SOCKS5</option>
                </select>
              {:else if key}
                <input
                  class="input-base"
                  type={f.type ?? "text"}
                  value={form[key]}
                  on:input={(e) => (form[key] = inputValue(e, f))}
                  min={f.min}
                  max={f.max}
                  placeholder={f.placeholder ??
                    (f.placeholderKey ? t($lang, f.placeholderKey) : undefined)}
                />
              {/if}
            </label>
          {/each}
        </div>
        <div class="grid grid-cols-2 gap-3">
          {#each PROXY_FIELDS.slice(3) as f (f.labelKey)}
            {@const key = f.key}
            <label class="field">
              <span>{t($lang, f.labelKey)}</span>
              {#if key}
                <input
                  class="input-base"
                  type={f.type ?? "text"}
                  value={form[key]}
                  on:input={(e) => (form[key] = inputValue(e, f))}
                />
              {/if}
            </label>
          {/each}
        </div>
      {/if}
    </div>

    <!-- SOCKS5 隧道(入站:本机开放端口访问远程内网服务;勾选启用后由列表 ⚡ 开关控制) -->
    <div class="section">
      <label class="flex items-center gap-2">
        <input
          type="checkbox"
          class="accent-indigo-500"
          bind:checked={form.socks5Enabled}
        />
        <p class="section-title">{t($lang, "servers.socks5Tunnel")}</p>
      </label>
      <p class="section-desc">{t($lang, "servers.socks5TunnelHint")}</p>
      {#if form.socks5Enabled}
        <div class="grid grid-cols-3 gap-3">
          {#each SOCKS5_FIELDS as f (f.labelKey)}
            {@const key = f.key}
            <label class="field">
              <span>{t($lang, f.labelKey)}</span>
              {#if key}
                <input
                  class="input-base"
                  type={f.type ?? "text"}
                  value={form[key]}
                  on:input={(e) => (form[key] = inputValue(e, f))}
                  min={f.min}
                  max={f.max}
                  placeholder={f.placeholder ??
                    (f.placeholderKey ? t($lang, f.placeholderKey) : undefined)}
                />
              {/if}
            </label>
          {/each}
        </div>
        <p class="section-desc">{t($lang, "servers.socks5PortHint")}</p>
      {/if}
    </div>

    <div class="flex justify-end gap-2">
      <button class="btn-secondary" on:click={cancelForm}
        >{t($lang, "common.cancel")}</button
      >
      <button
        class="btn-test"
        on:click={testConnection}
        disabled={testing}
        title={t($lang, "servers.test")}
      >
        {testing ? t($lang, "servers.testing") : t($lang, "servers.test")}
      </button>
      <button class="btn-primary" on:click={submitForm}>
        {editing ? t($lang, "common.save") : t($lang, "servers.addBtn")}
      </button>
    </div>
  </div>

  <!-- The prompt dialogs must live INSIDE the OverlayMenu dialog so their
       headlessui StackContextProvider chains into the form's dialog stack.
       Rendered as a sibling, clicking their input would look like an
       "outside click" to the form dialog, which closes itself. -->
  <PromptDialog
    open={keyNameOpen}
    title={t($lang, "servers.keyNameTitle")}
    message={t($lang, "servers.keyNameMessage")}
    label={t($lang, "servers.keyNameLabel")}
    placeholder={t($lang, "servers.keyNamePh")}
    confirmText={t($lang, "common.ok")}
    on:confirm={(event) => doGenerateKey(event.detail)}
    on:cancel={() => (keyNameOpen = false)}
  />

  <PromptDialog
    open={installPwdOpen}
    title={t($lang, "servers.keyInstallTitle")}
    message={t($lang, "servers.keyInstallMessage", {
      host: form.host || form.username,
    })}
    label={t($lang, "servers.keyInstallLabel")}
    type="password"
    confirmText={t($lang, "servers.keyInstallBtn")}
    on:confirm={(event) => doInstall(event.detail)}
    on:cancel={() => (installPwdOpen = false)}
  />
</OverlayMenu>

<ConfirmDialog
  open={deleteTarget !== null}
  title={deleteTarget ? t($lang, "servers.delTitle") : ""}
  message={deleteTarget
    ? t($lang, "servers.delMessage", { name: deleteTarget.name })
    : ""}
  danger
  confirmText={t($lang, "common.delete")}
  on:confirm={confirmDelete}
  on:cancel={() => (deleteTarget = null)}
/>

<style lang="postcss">
  .field {
    @apply flex flex-col gap-1 text-sm text-zinc-300;
  }

  .btn-primary {
    @apply inline-flex items-center gap-1 rounded-md bg-indigo-900 px-3 py-1.5 text-sm font-medium text-indigo-100 transition-colors hover:bg-indigo-800;
  }

  .btn-secondary {
    @apply rounded-md px-3 py-1.5 text-sm text-zinc-300 transition-colors hover:bg-zinc-700;
  }

  .btn-test {
    @apply inline-flex items-center gap-1 rounded-md px-3 py-1.5 text-sm font-medium text-emerald-300 transition-colors hover:bg-emerald-900/40 disabled:cursor-not-allowed disabled:opacity-50;
  }

  .btn-mini {
    /* py-2.5 makes the buttons the same height as a `.input-base` select
       (text-sm 20px line-height): 2px border + 20px padding + 16px line-height
       = 38px, matching 2px + 16px + 20px. */
    @apply inline-flex shrink-0 items-center gap-1 whitespace-nowrap rounded-md border border-zinc-700 px-2 py-2.5 text-xs text-zinc-300 transition-colors hover:bg-zinc-700 hover:text-zinc-100;
  }

  .section {
    @apply rounded-lg border border-zinc-800 bg-zinc-800/20 p-3 flex flex-col gap-2;
  }

  .section-title {
    @apply text-sm font-medium text-zinc-200;
  }

  .section-desc {
    @apply text-xs text-zinc-500;
  }

  .jump-row {
    @apply flex items-center gap-1.5;
  }
</style>
