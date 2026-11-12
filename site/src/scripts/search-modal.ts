// The search palette. The backend is `search()` from ./search.ts; with an empty
// query the palette lists the suggestions the page embeds in #s-suggest.
import { slideTo } from "./motion.ts";
import { search, SearchUnavailable, type SearchHit } from "./search.ts";

let openImpl: (() => void) | undefined;

const dialog = document.querySelector<HTMLDialogElement>("#search");
const input = document.querySelector<HTMLInputElement>("#s-input");
const list = document.querySelector<HTMLUListElement>("#s-list");
const bar = document.querySelector<HTMLElement>("#s-bar");
const count = document.querySelector<HTMLElement>("#s-count");
const closeButton = dialog?.querySelector<HTMLButtonElement>(".s-close");

const base = import.meta.env.BASE_URL.replace(/\/?$/, "/");

const ENTITIES: Record<string, string> = {
  "&": "&amp;",
  "<": "&lt;",
  ">": "&gt;",
  '"': "&quot;",
  "'": "&#39;",
};
const escapeHtml = (text: string): string => text.replace(/[&<>"']/g, (c) => ENTITIES[c]!);

/** Escape `text` and wrap the query terms in <mark>. Matching runs on the raw text, so an entity is never split. */
function highlight(text: string, terms: string[]): string {
  if (terms.length === 0) return escapeHtml(text);
  const pattern = new RegExp(
    `(${terms.map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|")})`,
    "gi",
  );
  return text
    .split(pattern)
    .map((part, i) => (i % 2 === 1 ? `<mark>${escapeHtml(part)}</mark>` : escapeHtml(part)))
    .join("");
}

function suggestions(): SearchHit[] {
  try {
    const raw = document.querySelector("#s-suggest")?.textContent;
    return raw ? (JSON.parse(raw) as SearchHit[]) : [];
  } catch {
    return [];
  }
}

/** Keep hits of one section together, sections in order of their best hit. */
function grouped(hits: SearchHit[]): SearchHit[] {
  const order: string[] = [];
  const bySection = new Map<string, SearchHit[]>();
  for (const hit of hits) {
    if (!bySection.has(hit.section)) {
      bySection.set(hit.section, []);
      order.push(hit.section);
    }
    bySection.get(hit.section)!.push(hit);
  }
  return order.flatMap((section) => bySection.get(section)!);
}

if (dialog && input && list && bar && count) {
  let hits: SearchHit[] = [];
  let active = 0;
  let seq = 0;
  let opener: Element | null = null;

  const empty = (html: string) => {
    hits = [];
    list.innerHTML = `<li class="s-empty" role="presentation">${html}</li>`;
    input.removeAttribute("aria-activedescendant");
    input.setAttribute("aria-expanded", "false");
    slideTo(bar, null);
  };

  const select = (i: number, scroll: boolean) => {
    if (hits.length === 0) return;
    active = (i + hits.length) % hits.length;
    let current: HTMLElement | null = null;
    for (const option of list.querySelectorAll<HTMLElement>("[role=option]")) {
      const on = Number(option.dataset.i) === active;
      option.setAttribute("aria-selected", String(on));
      if (on) current = option;
    }
    input.setAttribute("aria-activedescendant", `s-opt-${active}`);
    slideTo(bar, current);
    if (current && scroll) current.scrollIntoView({ block: "nearest" });
  };

  const show = (found: SearchHit[], terms: string[]) => {
    hits = grouped(found);
    active = 0;
    let html = "";
    let last: string | null = null;
    hits.forEach((hit, i) => {
      if (hit.section !== last) {
        html += `<li class="g" role="presentation">${escapeHtml(hit.section)}</li>`;
        last = hit.section;
      }
      html +=
        `<li role="option" id="s-opt-${i}" data-i="${i}" aria-selected="false">` +
        `<span class="t">${highlight(hit.title, terms)}</span>` +
        `<span class="u">${escapeHtml(hit.url.startsWith(base) ? `/${hit.url.slice(base.length)}` : hit.url)}</span>` +
        // Pagefind has already escaped the excerpt text and wrapped matches in <mark>.
        `<span class="d">${hit.excerpt}</span></li>`;
    });
    list.innerHTML = html;
    list.parentElement!.scrollTop = 0;
    input.setAttribute("aria-expanded", "true");
    select(0, false);
  };

  const run = async () => {
    const mine = ++seq;
    const query = input.value.trim();
    const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
    if (terms.length === 0) {
      const suggested = suggestions();
      count.textContent = suggested.length > 0 ? "suggested" : "";
      if (suggested.length > 0) show(suggested, []);
      else empty("Type to search the documentation.");
      return;
    }
    try {
      const found = await search(query);
      if (mine !== seq) return;
      if (found.length === 0) {
        count.textContent = "0 results";
        empty(
          `No results for <b>${escapeHtml(query)}</b>.<br>Try a command, a flag or a key: <b>ls</b>, <b>--diff</b>, <b>tag:</b>, <b>lens</b>.`,
        );
        return;
      }
      count.textContent = `${found.length} result${found.length === 1 ? "" : "s"}`;
      show(found, terms);
    } catch (error) {
      if (mine !== seq) return;
      count.textContent = "";
      empty(escapeHtml(error instanceof SearchUnavailable ? error.message : "Search failed."));
    }
  };

  const go = (i: number) => {
    const hit = hits[i];
    if (!hit) return;
    opener = null;
    dialog.close();
    window.location.assign(hit.url);
  };

  input.addEventListener("input", () => void run());
  input.addEventListener("keydown", (event) => {
    if (event.isComposing) return;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      select(active + 1, true);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      select(active - 1, true);
    } else if (event.key === "Home" && hits.length > 0) {
      event.preventDefault();
      select(0, true);
    } else if (event.key === "End" && hits.length > 0) {
      event.preventDefault();
      select(hits.length - 1, true);
    } else if (event.key === "Enter") {
      event.preventDefault();
      go(active);
    } else if (event.key === "Tab") {
      // Two focusable controls: cycle between them so focus never leaves the dialog.
      event.preventDefault();
      closeButton?.focus();
    }
  });
  list.addEventListener("mousemove", (event) => {
    const option = (event.target as Element).closest<HTMLElement>("[role=option]");
    if (option && Number(option.dataset.i) !== active) select(Number(option.dataset.i), false);
  });
  list.addEventListener("click", (event) => {
    const option = (event.target as Element).closest<HTMLElement>("[role=option]");
    if (option) go(Number(option.dataset.i));
  });
  closeButton?.addEventListener("click", () => dialog.close());
  closeButton?.addEventListener("keydown", (event) => {
    if (event.key === "Tab") {
      event.preventDefault();
      input.focus();
    }
  });
  // A click on the backdrop lands on the dialog element itself.
  dialog.addEventListener("click", (event) => {
    if (event.target === dialog) dialog.close();
  });
  // Esc and every other way of closing end up here: hand focus back to where it was.
  dialog.addEventListener("close", () => {
    input.setAttribute("aria-expanded", "false");
    if (opener instanceof HTMLElement) opener.focus();
    opener = null;
  });

  const open = () => {
    if (dialog.open) return;
    opener = document.activeElement;
    input.value = "";
    dialog.showModal();
    input.focus();
    void run();
  };
  openImpl = open;
  for (const button of document.querySelectorAll("[data-search-open]")) {
    button.addEventListener("click", open);
  }
  try {
    if (/Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent)) {
      for (const kbd of document.querySelectorAll("[data-search-kbd]")) kbd.textContent = "⌘K";
    }
  } catch {
    // keep the default label
  }
}

export const openSearch = (): void => openImpl?.();
export const closeSearch = (): void => dialog?.close();
export const isSearchOpen = (): boolean => dialog?.open === true;
