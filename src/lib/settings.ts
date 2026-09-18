import { persisted } from "svelte-persisted-store";
import {
  DEFAULT_FONT_SIZE,
  DEFAULT_SCROLLBACK,
  FONT_SIZE_MAX,
  FONT_SIZE_MIN,
  STORAGE_KEY_SETTINGS,
} from "./constants";
import themes, { defaultTheme, type ThemeName } from "./ui/themes";
import { derived, type Readable } from "svelte/store";

export type Settings = {
  theme: ThemeName;
  scrollback: number;
  fontSize: number;
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

    // Clamped, not rejected: a value stored by an older/hand-edited build must
    // not be able to make the terminal unusable (a 0 made every row overlap).
    let fontSize = $storedSettings.fontSize;
    if (typeof fontSize !== "number" || !Number.isFinite(fontSize)) {
      fontSize = DEFAULT_FONT_SIZE;
    }
    fontSize = Math.min(
      FONT_SIZE_MAX,
      Math.max(FONT_SIZE_MIN, Math.round(fontSize)),
    );

    return {
      theme,
      scrollback,
      fontSize,
    };
  },
);

export function updateSettings(values: Partial<Settings>) {
  storedSettings.update((settings) => ({ ...settings, ...values }));
}
