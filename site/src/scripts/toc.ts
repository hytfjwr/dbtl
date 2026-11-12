// Scroll-spy for "On this page" (the right-hand list, or the chips on generated
// pages): the last heading that has scrolled past the top of the viewport marks its
// link, and the marker in the list slides to it.
import { slideTo } from "./motion.ts";

const root = document.documentElement;
const links = new Map<string, HTMLAnchorElement>();
for (const a of document.querySelectorAll<HTMLAnchorElement>(
  ".toc a[href^='#'], .jump a[href^='#']",
)) {
  links.set(decodeURIComponent(a.hash.slice(1)), a);
}

const headings = [...links.keys()]
  .map((id) => document.getElementById(id))
  .filter((el): el is HTMLElement => el !== null);

if (headings.length > 0) {
  const ink = document.querySelector<HTMLElement>("#toc-ink");
  let hold = 0;
  let ticking = false;

  // Keep the active entry inside a scrolled list without moving the page.
  const reveal = (li: HTMLElement) => {
    const ul = li.parentElement;
    if (!ul || ul.scrollHeight <= ul.clientHeight) return;
    if (li.offsetTop < ul.scrollTop) ul.scrollTop = li.offsetTop;
    else if (li.offsetTop + li.offsetHeight > ul.scrollTop + ul.clientHeight) {
      ul.scrollTop = li.offsetTop + li.offsetHeight - ul.clientHeight;
    }
  };

  const update = () => {
    if (headings[0]!.offsetParent === null) return;
    const limit = parseInt(getComputedStyle(root).getPropertyValue("--top-h"), 10) + 70;
    let current = headings[0]!.id;
    for (const h of headings) if (h.getBoundingClientRect().top <= limit) current = h.id;
    const atEnd = window.innerHeight + window.scrollY >= root.scrollHeight - 4;
    if (atEnd && Date.now() > hold) current = headings[headings.length - 1]!.id;

    let marked: HTMLElement | null = null;
    for (const [id, a] of links) {
      const on = id === current;
      a.classList.toggle("active", on);
      if (on) {
        a.setAttribute("aria-current", "location");
        marked = a.parentElement;
      } else {
        a.removeAttribute("aria-current");
      }
    }
    slideTo(ink, marked);
    if (marked) reveal(marked);
  };

  window.addEventListener(
    "scroll",
    () => {
      if (ticking) return;
      ticking = true;
      requestAnimationFrame(() => {
        ticking = false;
        update();
      });
    },
    { passive: true },
  );
  window.addEventListener("resize", update);
  // Choosing an entry scrolls to a heading near the end of the page; keep the end-of-page rule from overriding it.
  document.addEventListener("click", (event) => {
    if ((event.target as Element).closest(".toc a, .jump a")) hold = Date.now() + 900;
  });
  update();
  // Web fonts change line heights after first paint.
  void document.fonts?.ready.then(update);
}
