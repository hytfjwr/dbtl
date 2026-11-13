/** The text of the first `# ` heading outside a code fence, inline code unwrapped. */
export function referenceTitle(body: string): string {
  let fenced = false;
  for (const line of body.split("\n")) {
    if (/^\s*(```|~~~)/.test(line)) fenced = !fenced;
    if (!fenced && line.startsWith("# ")) {
      return line.slice(2).replace(/`/g, "").trim();
    }
  }
  throw new Error("Reference page has no level-1 heading");
}

/** The first prose paragraph after the title, as plain text, for the meta description. */
export function referenceDescription(body: string): string {
  const paragraphs = body.split(/\n\s*\n/);
  for (const raw of paragraphs) {
    const p = raw.trim();
    // A paragraph that ends with a colon introduces what follows (a list, a code block): not a summary.
    if (!p || /^(<!--|#|\||```|~~~|[-*] )/.test(p) || p.endsWith(":")) continue;
    const plain = p
      .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
      .replace(/[`*_]/g, "")
      .replace(/\s+/g, " ");
    return plain.length > 200 ? `${plain.slice(0, 197).trimEnd()}...` : plain;
  }
  return "";
}

/**
 * Meta descriptions for the generated pages. Their first paragraph introduces the page rather
 * than summarizing it, so the site states each page's scope; a page missing here falls back to
 * its first prose paragraph.
 */
const DESCRIPTIONS: Record<string, string> = {
  cli: "Every dbtl subcommand and flag, with its values, environment variable and default.",
  keybindings: "Every key in the dbtl terminal UI, grouped by mode.",
  "lint-rules": "Every lint rule: why it exists, an example, and its parameters.",
  config: "Every key of .dbtl.yml and of the user config.yml.",
  themes: "Every colour role a dbtl theme file may set, and the built-in presets.",
  changelog: "What changed in each dbtl release, newest first.",
};

/** The meta description for the generated page `id` (its file stem). */
export function referenceDescriptionFor(id: string, body: string): string {
  return DESCRIPTIONS[id] ?? referenceDescription(body);
}
