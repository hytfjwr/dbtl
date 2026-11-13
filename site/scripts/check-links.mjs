// Verifies every internal href/src and #anchor in the built site (dist/).
// External URLs and the Pagefind output are not checked. Exit 1 on any problem.
import fs from "node:fs";
import path from "node:path";
import config from "../astro.config.mjs";
import { fail, siteRoot } from "./shared.mjs";

const dist = path.join(siteRoot, "dist");
const base = `${config.base.replace(/\/+$/, "")}/`;
const ORIGIN = "http://internal.invalid";
const failLinks = fail("check-links");

if (!fs.existsSync(dist)) {
  failLinks("dist/ does not exist; run `astro build` first");
}

function walk(dir) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (full === path.join(dist, "pagefind")) continue;
      out.push(...walk(full));
    } else if (entry.name.endsWith(".html")) {
      out.push(full);
    }
  }
  return out;
}

const decodeEntities = (s) =>
  s
    .replace(/&#x([0-9a-f]+);/gi, (_, h) => String.fromCodePoint(parseInt(h, 16)))
    .replace(/&#(\d+);/g, (_, d) => String.fromCodePoint(Number(d)))
    .replace(/&quot;/g, '"')
    .replace(/&apos;/g, "'")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&amp;/g, "&");

const ATTR = (name) => new RegExp(`\\s${name}=(?:"([^"]*)"|'([^']*)'|([^\\s>"']+))`, "gi");

function attrs(html, name) {
  const out = [];
  for (const m of html.matchAll(ATTR(name))) out.push(decodeEntities(m[1] ?? m[2] ?? m[3] ?? ""));
  return out;
}

const pages = new Map(); // dist-relative file -> { urlPath, ids, hrefs }
for (const file of walk(dist)) {
  const rel = path.relative(dist, file).split(path.sep).join("/");
  const urlPath = rel.endsWith("index.html")
    ? `${base}${rel.slice(0, -"index.html".length)}`
    : `${base}${rel}`;
  const html = fs
    .readFileSync(file, "utf8")
    .replace(/<script\b[\s\S]*?<\/script>/gi, "")
    .replace(/<style\b[\s\S]*?<\/style>/gi, "");
  pages.set(rel, {
    urlPath,
    ids: new Set([...attrs(html, "id"), ...attrs(html, "name")]),
    links: [...attrs(html, "href"), ...attrs(html, "src")],
  });
}

/** The dist-relative HTML file a base-relative URL path serves, or null. */
function fileFor(pathname) {
  let rel = decodeURIComponent(pathname.slice(base.length));
  if (rel === "" || rel.endsWith("/")) rel += "index.html";
  if (pages.has(rel)) return rel;
  return fs.existsSync(path.join(dist, rel)) ? rel : null;
}

const problems = [];
let checked = 0;

for (const [rel, page] of pages) {
  for (const href of page.links) {
    if (/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(href) || href.startsWith("//")) continue; // external
    checked++;
    const url = new URL(href, `${ORIGIN}${page.urlPath}`);
    const fail = (why) => problems.push(`${rel}: ${href}  ->  ${why}`);

    let target = rel;
    if (url.pathname !== page.urlPath) {
      if (!url.pathname.startsWith(base)) {
        fail(`outside the site base ${base}`);
        continue;
      }
      if (url.pathname.startsWith(`${base}pagefind/`)) continue;
      const last = url.pathname.split("/").pop();
      if (!url.pathname.endsWith("/") && !last.includes(".")) {
        fail("missing trailing slash");
        continue;
      }
      const file = fileFor(url.pathname);
      if (!file) {
        fail("no such page or file in dist/");
        continue;
      }
      target = file;
    }

    const anchor = decodeURIComponent(url.hash.slice(1));
    if (anchor === "" || anchor === "top") continue;
    const dest = pages.get(target);
    if (!dest) continue; // a non-HTML asset has no anchors to verify
    if (!dest.ids.has(anchor)) fail(`no element with id "${anchor}" in ${target}`);
  }
}

if (problems.length > 0) {
  console.error(`check-links: ${problems.length} broken link(s) in ${pages.size} pages`);
  for (const p of problems) console.error(`  ${p}`);
  process.exit(1);
}
console.log(`check-links: ${checked} internal links ok across ${pages.size} pages`);
