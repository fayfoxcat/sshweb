/**
 * Type-level bridge for the terminal addons.
 *
 * The terminal kernel in this app is `sshx-xterm` (a fork of xterm.js 5.2 — see
 * `lib/ui/XTerm.svelte`), but the addons in use are the scoped `@xterm/addon-*`
 * packages, whose `.d.ts` files import their types from `@xterm/xterm`. Left
 * alone, TypeScript resolves that specifier to the real npm package (5.5.0),
 * whose `Terminal` declares members the fork does not ship (`input`,
 * `attachCustomWheelEventHandler`), so every `term.loadAddon(...)` call fails to
 * type-check against the kernel actually in use.
 *
 * `tsconfig.json` maps `@xterm/xterm` here, so the addons are checked against
 * the fork's own public API. That is what the old unscoped `xterm-addon-*`
 * packages got for free: they imported from `'xterm'`, which `sshx-xterm`
 * declares as an ambient module. Keeping the mapping means an addon that starts
 * using a newer kernel API becomes a compile error instead of a runtime one.
 */
export * from "sshx-xterm";
