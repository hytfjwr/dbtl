import { search, SearchUnavailable, type SearchHit } from "./search.ts";

const dialog = document.querySelector<HTMLDialogElement>("#search");
const input = document.querySelector<HTMLInputElement>("#s-input");
const list = document.querySelector<HTMLUListElement>("#s-list");
const note = document.querySelector<HTMLElement>("#s-note");

function escapeHtml(text: string): string {
  return text.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}

if (dialog && input && list && note) {
  let hits: SearchHit[] = [];
  let active = 0;
  let seq = 0;

  const say = (message: string) => {
    note.textContent = message;
    note.hidden = message === "";
  };

  const render = () => {
    list.innerHTML = hits
      .map(
        (h, i) =>
          `<li role="option" id="s-opt-${i}" data-i="${i}" aria-selected="${i === active}">` +
          `<span class="t">${escapeHtml(h.title)}</span>` +
          (h.section ? `<span class="sec">${escapeHtml(h.section)}</span>` : "") +
          // Pagefind has already escaped the excerpt text and wrapped matches in <mark>.
          `<span class="d">${h.excerpt}</span></li>`,
      )
      .join("");
    if (hits.length > 0) input.setAttribute("aria-activedescendant", `s-opt-${active}`);
    else input.removeAttribute("aria-activedescendant");
  };

  const setActive = (i: number) => {
    if (hits.length === 0) return;
    active = (i + hits.length) % hits.length;
    for (const option of list.querySelectorAll<HTMLElement>("[role=option]")) {
      const on = Number(option.dataset.i) === active;
      option.setAttribute("aria-selected", String(on));
      if (on) option.scrollIntoView({ block: "nearest" });
    }
    input.setAttribute("aria-activedescendant", `s-opt-${active}`);
  };

  const run = async () => {
    const mine = ++seq;
    const query = input.value.trim();
    if (query === "") {
      hits = [];
      say("");
      render();
      return;
    }
    try {
      const found = await search(query);
      if (mine !== seq) return;
      hits = found;
      active = 0;
      say(found.length === 0 ? `No results for "${query}".` : "");
    } catch (error) {
      if (mine !== seq) return;
      hits = [];
      say(error instanceof SearchUnavailable ? error.message : "Search failed.");
    }
    render();
  };

  const open = () => {
    if (dialog.open) return;
    input.value = "";
    hits = [];
    say("");
    render();
    dialog.showModal();
    input.focus();
  };

  const go = (i: number) => {
    const hit = hits[i];
    if (hit) window.location.assign(hit.url);
  };

  input.addEventListener("input", () => void run());
  input.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive(active + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive(active - 1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      go(active);
    }
  });
  list.addEventListener("click", (event) => {
    const option = (event.target as Element).closest<HTMLElement>("[role=option]");
    if (option) go(Number(option.dataset.i));
  });
  // A click on the backdrop lands on the dialog element itself.
  dialog.addEventListener("click", (event) => {
    if (event.target === dialog) dialog.close();
  });

  for (const button of document.querySelectorAll("[data-search-open]")) {
    button.addEventListener("click", open);
  }
  document.addEventListener("keydown", (event) => {
    if ((event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === "k") {
      event.preventDefault();
      if (dialog.open) dialog.close();
      else open();
      return;
    }
    if (dialog.open || event.metaKey || event.ctrlKey || event.altKey) return;
    const el = event.target as HTMLElement | null;
    if (el && (["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName) || el.isContentEditable)) return;
    if (event.key === "/") {
      event.preventDefault();
      open();
    }
  });

  try {
    if (/Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent)) {
      for (const kbd of document.querySelectorAll("[data-search-kbd]")) kbd.textContent = "⌘K";
    }
  } catch {
    // keep the default label
  }
}
