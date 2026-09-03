import { persisted } from "svelte-persisted-store";
import { DEFAULT_SCROLLBACK, STORAGE_KEY_SETTINGS } from "./constants";
import themes, { defaultTheme, type ThemeName } from "./ui/themes";
import { derived, type Readable } from "svelte/store";

export type Settings = {
  theme: ThemeName;
  scrollback: number;
};

const storedSettings = persisted<Partial<Settings>>(STORAGE_KEY_SETTINGS, {});

/** A persisted store for settings of the current user. */
export const settings: Readable<Settings> = derived(
  storedSettings,
  ($storedSettings) => {
    // Do some validation on all of the stored settings.
    let theme = $storedSettings.theme;
    if (!theme || !Object.hasOwn(themes, theme)) {
      theme = defaultTheme;
    }

    let scrollback = $storedSettings.scrollback;
    if (typeof scrollback !== "number" || scrollback < 0) {
      scrollback = DEFAULT_SCROLLBACK;
    }

    return {
      theme,
      scrollback,
    };
  },
);

export function updateSettings(values: Partial<Settings>) {
  storedSettings.update((settings) => ({ ...settings, ...values }));
}
