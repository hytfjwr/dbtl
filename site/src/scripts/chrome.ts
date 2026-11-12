// Page chrome behaviour: keyboard shortcuts, the docs drawer on narrow screens,
// wide-table scroll cues and the theme chip in the status line.
import { pulse } from "./motion.ts";
import { closeSearch, isSearchOpen, openSearch } from "./search-modal.ts";
import { cycleTheme, onThemeChange } from "./theme.ts";

const $ = <T extends Element = HTMLElement>(selector: string, scope: ParentNode = document) =>
  scope.querySelector<T>(selector);
const $$ = <T extends Element = HTMLElement>(selector: string, scope: ParentNode = document) =>
  Array.from(scope.querySelectorAll<T>(selector));

/* ---- docs drawer (narrow screens): focus moves in, Tab stays in, Esc and the scrim close, focus returns ---- */
const side = $("#sidebar");
const scrim = $("#scrim");
const openButton = $<HTMLButtonElement>("#open-drawer");
const closeButton = $<HTMLButtonElement>("#close-drawer");

const drawerOpen = () => side?.classList.contains("open") === true;

// Everything outside the drawer that should be unreachable while it is open.
// The drawer itself sits inside <main>, and the scrim has to stay live so a
// tap on it still closes, so neither can be marked inert.
const inertTargets = $$(".skip, .top, .docbar, .doc-main, .toc, .site-foot, .statusline");

function setDrawer(open: boolean): void {
  if (!side || !scrim || !openButton || open === drawerOpen()) return;
  side.classList.toggle("open", open);
  scrim.hidden = !open;
  openButton.setAttribute("aria-expanded", String(open));
  document.body.classList.toggle("drawer-open", open);
  for (const el of inertTargets) el.toggleAttribute("inert", open);
  // Inert comes off above before focus moves back out of the drawer.
  if (open) closeButton?.focus();
  else openButton.focus();
}

if (side && scrim && openButton && closeButton) {
  openButton.addEventListener("click", () => setDrawer(true));
  closeButton.addEventListener("click", () => setDrawer(false));
  scrim.addEventListener("click", () => setDrawer(false));
  side.addEventListener("click", (event) => {
    if ((event.target as Element).closest("a")) setDrawer(false);
  });
  side.addEventListener("keydown", (event) => {
    if (event.key !== "Tab" || !drawerOpen()) return;
    const items = $$("a[href], button", side);
    const first = items[0];
    const last = items[items.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last?.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first?.focus();
    }
  });
  // Back on a wide screen the sidebar is a column again: drop the drawer state.
  // Mirrors the CSS breakpoint exactly (max-width:900px) so fractional widths
  // from zoom or DPI can't leave the drawer state stuck on.
  window.matchMedia("(max-width: 900px)").addEventListener("change", (e) => {
    if (!e.matches) setDrawer(false);
  });
}

/* ---- wide tables: show the edge fade and the hint only while there is more to the right ---- */
const wrappers = $$(".tbl-wrap");
if (wrappers.length > 0) {
  const check = (wrap: HTMLElement) => {
    const scroller = $(".tbl", wrap);
    if (!scroller) return;
    const scrolls = scroller.scrollWidth > scroller.clientWidth + 1;
    wrap.dataset.scrolls = scrolls ? "1" : "0";
    wrap.dataset.more =
      scrolls && scroller.scrollLeft + scroller.clientWidth < scroller.scrollWidth - 2 ? "1" : "0";
  };
  const run = () => wrappers.forEach(check);
  for (const wrap of wrappers)
    $(".tbl", wrap)?.addEventListener("scroll", () => check(wrap), { passive: true });
  window.addEventListener("resize", run);
  run();
  void document.fonts?.ready.then(run);
}

/* ---- stacked wide tables do not scroll, so they are not a tab stop or a region ---- */
const stackable = $$(".tbl.wide");
if (stackable.length > 0) {
  const label = new Map(stackable.map((el) => [el, el.getAttribute("aria-label") ?? ""]));
  const stacked = window.matchMedia("(max-width: 680px)");
  const apply = () => {
    for (const el of stackable) {
      if (stacked.matches) {
        el.removeAttribute("tabindex");
        el.removeAttribute("role");
        el.removeAttribute("aria-label");
      } else {
        el.tabIndex = 0;
        el.setAttribute("role", "region");
        el.setAttribute("aria-label", label.get(el) ?? "");
      }
    }
  };
  stacked.addEventListener("change", apply);
  apply();
}

/* ---- theme chip in the status line ---- */
const chip = $("#chip-theme");
if (chip) onThemeChange((choice) => (chip.textContent = `theme:${choice}`));
for (const button of $$('.hints [data-act="theme"]')) button.addEventListener("click", cycleTheme);

/* ---- keyboard: Cmd-K / Ctrl-K and "/" open search; a key shown in the status line runs that hint ---- */
document.addEventListener("keydown", (event) => {
  if ((event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === "k") {
    event.preventDefault();
    if (isSearchOpen()) closeSearch();
    else openSearch();
    return;
  }
  if (event.key === "Escape" && drawerOpen()) {
    setDrawer(false);
    return;
  }
  if (event.defaultPrevented || isSearchOpen() || event.metaKey || event.ctrlKey || event.altKey)
    return;
  const el = event.target as HTMLElement | null;
  if (el && (["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName) || el.isContentEditable)) return;
  const hint = $$("[data-key]", $(".hints") ?? document).find((h) => h.dataset.key === event.key);
  if (!hint) return;
  event.preventDefault();
  pulse(hint, "pressed", 160);
  hint.click();
});
