// Copy buttons. The text comes from, in order: `data-copy`, the element named by
// `data-copy-from`, or the <pre> next to the button.
import { pulse } from "./motion.ts";

function fallbackCopy(text: string): void {
  try {
    const area = document.createElement("textarea");
    area.value = text;
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    // The Clipboard API is unavailable in insecure contexts (local preview over
    // plain HTTP); this legacy call is the only fallback. The structural type
    // sidesteps the deprecated lib.dom signature.
    (document as { execCommand(commandId: string): boolean }).execCommand("copy");
    area.remove();
  } catch {
    // nothing else to try
  }
}

async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    fallbackCopy(text);
  }
}

/** A terminal transcript copies as its commands only: lines that start with `$ `, minus the prompt. */
function textOf(pre: Element): string {
  const text = pre.textContent ?? "";
  const commands = text.split("\n").filter((line) => line.startsWith("$ "));
  return commands.length > 0 ? commands.map((line) => line.slice(2)).join("\n") : text;
}

function sourceOf(button: HTMLElement): string {
  const inline = button.getAttribute("data-copy");
  if (inline !== null) return inline;
  const selector = button.getAttribute("data-copy-from");
  const el = selector ? document.querySelector(selector) : button.parentElement?.querySelector("pre");
  if (!el) return "";
  return el.tagName === "PRE" ? textOf(el) : (el.textContent ?? "");
}

document.addEventListener("click", (event) => {
  const target = event.target;
  if (!(target instanceof Element)) return;
  const button = target.closest<HTMLButtonElement>("button.copy");
  if (!button || button.hasAttribute("data-nocopy")) return;
  void copyText(sourceOf(button));
  button.classList.add("done");
  button.textContent = "copied";
  pulse(button.closest(".pane, .install"), "flash", 700);
  setTimeout(() => {
    button.classList.remove("done");
    button.textContent = "copy";
  }, 1400);
});
