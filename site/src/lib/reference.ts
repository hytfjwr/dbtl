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
    if (!p || /^(<!--|#|\||```|~~~|[-*] )/.test(p)) continue;
    const plain = p
      .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
      .replace(/[`*_]/g, "")
      .replace(/\s+/g, " ");
    return plain.length > 200 ? `${plain.slice(0, 197).trimEnd()}...` : plain;
  }
  return "";
}
