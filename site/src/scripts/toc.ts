// Scroll-spy for the "On this page" list: the heading nearest the top of the
// viewport gets aria-current="true" on its link.

const links = new Map<string, HTMLAnchorElement>();
for (const a of document.querySelectorAll<HTMLAnchorElement>(".toc a[href^='#']")) {
  links.set(decodeURIComponent(a.hash.slice(1)), a);
}

if (links.size > 0 && "IntersectionObserver" in window) {
  const headings = [...links.keys()]
    .map((id) => document.getElementById(id))
    .filter((el): el is HTMLElement => el !== null);
  const visible = new Set<string>();

  const mark = () => {
    const first = headings.find((h) => visible.has(h.id));
    for (const [id, a] of links) {
      if (first?.id === id) a.setAttribute("aria-current", "true");
      else a.removeAttribute("aria-current");
    }
  };

  const observer = new IntersectionObserver(
    (entries) => {
      for (const e of entries) {
        if (e.isIntersecting) visible.add(e.target.id);
        else visible.delete(e.target.id);
      }
      mark();
    },
    { rootMargin: "-60px 0px -65% 0px" },
  );
  for (const h of headings) observer.observe(h);
}
