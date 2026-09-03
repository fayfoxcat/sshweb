/**
 * @file Central registry mapping file extensions to CodeMirror language
 * extensions. Each entry lazily loads its language package so that opening an
 * editor only pulls in the grammar actually needed.
 *
 * Modern `@codemirror/lang-*` packages return a `LanguageSupport` (grammar +
 * built-in highlighting). Legacy StreamLanguage modes (shell, nginx,
 * properties) are wrapped here and rely on a shared fallback highlight style.
 */

import type { Extension } from "@codemirror/state";
import type { StreamParser } from "@codemirror/language";

/** A language descriptor: matcher for a file name/path + lazy loader. */
type LanguageEntry = {
  /** Extensions (or filename suffixes) this language matches. */
  extensions: string[];
  /** Loader returning a CodeMirror extension (Extension | LanguageSupport). */
  load: () => Promise<Extension>;
};

/** A modern `@codemirror/lang-*` loader factory. The dynamic `import` path
 *  stays a literal inside each loader (Vite cannot resolve a computed one —
 *  see 交接文档 坑 2), never build it from a variable. */
function modernLang(
  exts: string[],
  load: () => Promise<Extension>,
): LanguageEntry {
  return { extensions: exts, load };
}

/** A legacy StreamLanguage loader factory (shell / nginx / properties / toml),
 *  reusing the shared fallback theme in [`streamLanguage`]. */
function streamLang(
  exts: string[],
  load: () => Promise<StreamParser<unknown>>,
): LanguageEntry {
  return {
    extensions: exts,
    load: async () => streamLanguage(await load()),
  };
}

/** Lazily load the legacy shell grammar (shared by the registry entry and
 *  [`shellLanguage`], which feeds small inline editors). */
async function loadShell(): Promise<StreamParser<unknown>> {
  const { shell } = await import("@codemirror/legacy-modes/mode/shell");
  return shell;
}

const extensions: LanguageEntry[] = [
  // JavaScript family
  modernLang(["js", "jsx", "mjs", "cjs", "ts", "tsx"], async () =>
    (await import("@codemirror/lang-javascript")).javascript(),
  ),
  modernLang(["json"], async () =>
    (await import("@codemirror/lang-json")).json(),
  ),
  modernLang(["py"], async () =>
    (await import("@codemirror/lang-python")).python(),
  ),
  modernLang(["html", "htm"], async () =>
    (await import("@codemirror/lang-html")).html(),
  ),
  modernLang(["css", "scss", "less"], async () =>
    (await import("@codemirror/lang-css")).css(),
  ),
  modernLang(["c", "h", "cpp", "cxx", "cc", "cs"], async () =>
    (await import("@codemirror/lang-cpp")).cpp(),
  ),
  modernLang(["java"], async () =>
    (await import("@codemirror/lang-java")).java(),
  ),
  modernLang(["php"], async () => (await import("@codemirror/lang-php")).php()),
  modernLang(["md", "markdown"], async () =>
    (await import("@codemirror/lang-markdown")).markdown(),
  ),
  modernLang(["sql"], async () => (await import("@codemirror/lang-sql")).sql()),
  modernLang(["xml", "svg"], async () =>
    (await import("@codemirror/lang-xml")).xml(),
  ),
  modernLang(["rs"], async () =>
    (await import("@codemirror/lang-rust")).rust(),
  ),
  modernLang(["go"], async () => (await import("@codemirror/lang-go")).go()),
  modernLang(["yaml", "yml"], async () =>
    (await import("@codemirror/lang-yaml")).yaml(),
  ),
  streamLang(["sh", "bash", "zsh", "fish"], loadShell),
  streamLang(["nginx.conf", "nginx"], async () => {
    const { nginx } = await import("@codemirror/legacy-modes/mode/nginx");
    return nginx;
  }),
  streamLang(["properties", "conf", "config", "ini", "cfg"], async () => {
    const { properties } = await import(
      "@codemirror/legacy-modes/mode/properties"
    );
    return properties;
  }),
  streamLang(["toml"], async () => {
    const { toml } = await import("@codemirror/legacy-modes/mode/toml");
    return toml;
  }),
];

/** Match a file path against the registry by its final name/suffix. */
function matches(entry: LanguageEntry, filePath: string): boolean {
  const base = filePath.toLowerCase();
  return entry.extensions.some((ext) => {
    if (ext.includes(".")) {
      // Whole filename (e.g. "nginx.conf").
      return base === ext || base.endsWith(`/${ext}`);
    }
    return base.endsWith(`.${ext}`);
  });
}

/**
 * Return the CodeMirror language extension for a file path, or `null` if no
 * grammar is registered for it.
 */
export async function languageForPath(
  filePath: string,
): Promise<Extension | null> {
  for (const entry of extensions) {
    if (matches(entry, filePath)) {
      try {
        return await entry.load();
      } catch (err) {
        console.error("failed to load language for", filePath, err);
        return null;
      }
    }
  }
  return null;
}

/** Determine whether a file should use the light (no-highlight) mode. */
export const LIGHT_MODE_LINE_LIMIT = 5000;

/** Shell language extension (legacy StreamLanguage + fallback theme), used by
 *  small inline editors such as the startup snippet field. */
export async function shellLanguage(): Promise<Extension> {
  return streamLanguage(await loadShell());
}

// ---- Loaders -------------------------------------------------------------

/** Wrap a legacy StreamParser as a LanguageSupport with our fallback theme. */
async function streamLanguage(
  parser: StreamParser<unknown>,
): Promise<Extension> {
  const {
    StreamLanguage,
    LanguageSupport,
    HighlightStyle,
    syntaxHighlighting,
  } = await import("@codemirror/language");
  const { tags } = await import("@lezer/highlight");

  const fallback = HighlightStyle.define([
    { tag: tags.keyword, color: "#c678dd" },
    { tag: tags.comment, color: "#7f848e", fontStyle: "italic" },
    { tag: tags.string, color: "#98c379" },
    { tag: tags.number, color: "#d19a66" },
    { tag: tags.propertyName, color: "#61afef" },
    { tag: tags.definition(tags.name), color: "#e06c75" },
    { tag: tags.bool, color: "#d19a66" },
    { tag: tags.operator, color: "#56b6c2" },
  ]);

  // Legacy stream parsers reference tokens by name (e.g. "keyword", "string").
  // StreamLanguage maps those via a token table; without one they resolve to
  // no tags and highlighting silently vanishes. Provide a table mapping the
  // standard legacy token names to Lezer tags so the fallback theme applies.
  const tokenTable = {
    keyword: tags.keyword,
    atom: tags.atom,
    bool: tags.bool,
    url: tags.url,
    labelName: tags.labelName,
    inserted: tags.inserted,
    deleted: tags.deleted,
    literal: tags.literal,
    string: tags.string,
    number: tags.number,
    variableName: tags.variableName,
    typeName: tags.typeName,
    namespace: tags.namespace,
    className: tags.className,
    macroName: tags.macroName,
    propertyName: tags.propertyName,
    operator: tags.operator,
    comment: tags.comment,
    meta: tags.meta,
    invalid: tags.invalid,
    // Extra legacy aliases commonly emitted by CodeMirror 5 modes.
    variable: tags.variableName,
    variable2: tags.variableName,
    string2: tags.string,
    def: tags.definition(tags.variableName),
    tag: tags.tagName,
    attribute: tags.attributeName,
    type: tags.typeName,
    builtin: tags.standard(tags.variableName),
    qualifier: tags.modifier,
    error: tags.invalid,
    header: tags.heading,
    property: tags.propertyName,
  };

  const spec = Object.assign({}, parser, { tokenTable });

  return new LanguageSupport(StreamLanguage.define(spec), [
    syntaxHighlighting(fallback),
  ]);
}
