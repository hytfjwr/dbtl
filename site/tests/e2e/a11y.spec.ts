import { AxeBuilder } from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import navJson from "../../src/nav.json" with { type: "json" };

// Whole-site structure and an axe pass on the three page shapes. Structural
// checks run on every route so a heading or id regression cannot slip through
// on a page nobody looked at.
const docPaths = (navJson as Array<{ items: string[] }>).flatMap((group) =>
  group.items.map((slug) => `${slug}/`),
);
const paths = ["", ...docPaths, "playground/", "404.html"];

for (const path of paths) {
  test(`heading order and unique ids: /dbtl/${path}`, async ({ page }) => {
    await page.goto(`/dbtl/${path}`);
    await page.waitForLoadState("networkidle");

    const report = await page.evaluate(() => {
      const levels = [...document.querySelectorAll("h1,h2,h3,h4,h5,h6")].map((h) =>
        Number(h.tagName[1]),
      );
      const ids = [...document.querySelectorAll("[id]")].map((el) => el.id);
      const dup = ids.filter((id, i) => ids.indexOf(id) !== i);
      return { levels, dup: [...new Set(dup)] };
    });

    // Heading levels may step down one at a time, never skip; a page starts
    // with its own h1 (sidebar and footer labels are not headings).
    expect(report.dup, "duplicate ids").toEqual([]);
    expect(report.levels[0], "first heading is h1").toBe(1);
    for (let i = 1; i < report.levels.length; i++) {
      expect(report.levels[i]!, `heading ${i} on /dbtl/${path}`).toBeLessThanOrEqual(
        report.levels[i - 1]! + 1,
      );
    }
  });
}

test("a <pre> is a tab stop only while it scrolls", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/dbtl/cli/lint/");
  await page.waitForLoadState("networkidle");
  await page.evaluate(async () => {
    await document.fonts.ready;
  });

  const check = () =>
    page.evaluate(() =>
      [...document.querySelectorAll<HTMLElement>("pre")]
        .filter((p) => !p.closest(".lin-stage"))
        .map((p) => ({
          tab: p.tabIndex,
          scrolls: p.scrollWidth > p.clientWidth + 1 || p.scrollHeight > p.clientHeight + 1,
        })),
    );

  for (const p of await check()) {
    expect(p.tab === 0, `tabindex ${p.tab} but scrolls=${p.scrolls}`).toBe(p.scrolls);
  }

  // Layout changes re-measure: at 320px more blocks scroll, so at least one
  // gains a tab stop and the rule still holds for all of them. The wait has
  // to poll the rule itself — counting tab stops is already true at 1280px
  // (the static markup ships tabindex="0"), so it can return before the
  // re-measure lands and let the check below read a stale tabIndex.
  await page.setViewportSize({ width: 320, height: 568 });
  await expect
    .poll(async () => {
      const ps = await check();
      return ps.some((p) => p.scrolls) && ps.every((p) => (p.tab === 0) === p.scrolls);
    })
    .toBe(true);
  for (const p of await check()) {
    expect(p.tab === 0, `tabindex ${p.tab} but scrolls=${p.scrolls}`).toBe(p.scrolls);
  }
});

// Same rule for the 404 page's lineage box: a scroller only gets a tab stop
// while its diagram is wider than the pane.
test("the 404 lineage scroller is a tab stop only while it scrolls", async ({ page }) => {
  const check = () =>
    page.evaluate(() => {
      const el = document.querySelector<HTMLElement>(".scroll")!;
      return {
        tab: el.tabIndex,
        scrolls: el.scrollWidth > el.clientWidth + 1 || el.scrollHeight > el.clientHeight + 1,
      };
    });

  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/dbtl/404.html");
  await page.waitForLoadState("networkidle");
  await page.evaluate(async () => {
    await document.fonts.ready;
  });
  const wide = await check();
  expect(wide.tab === 0, `tabindex ${wide.tab} but scrolls=${wide.scrolls}`).toBe(wide.scrolls);

  // A longer missing path draws a wider diagram, so the pane scrolls at 320px.
  await page.setViewportSize({ width: 320, height: 568 });
  await page.goto(`/dbtl/${"missing/".repeat(6)}`);
  await page.waitForLoadState("networkidle");
  // Same wait as above: the overflow alone can flip before the tab stop is
  // re-measured, so poll until both have landed.
  await expect
    .poll(async () => {
      const s = await check();
      return s.scrolls && s.tab === 0;
    }, "lineage pane overflows and its tab stop lands at 320px")
    .toBe(true);
  const narrow = await check();
  expect(narrow.tab === 0, `tabindex ${narrow.tab} but scrolls=${narrow.scrolls}`).toBe(
    narrow.scrolls,
  );
});

test("interactive targets are at least 24px", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/dbtl/cli/lint/");
  await page.waitForLoadState("networkidle");

  // Inline text links size on their line — WCAG's inline-text exception —
  // so the height is the meaningful dimension for them.
  for (const selector of [".meta a", ".crumb a", ".jump a", "[data-keys-toggle]"]) {
    for (const el of await page.locator(selector).all()) {
      if (!(await el.isVisible())) continue;
      const box = (await el.boundingBox())!;
      expect(box.height, `${selector} target height`).toBeGreaterThanOrEqual(24);
    }
  }

  await page.goto("/dbtl/");
  await page.waitForLoadState("networkidle");
  const buttons = page.locator(".tui-list button");
  expect(await buttons.count()).toBeGreaterThan(3);
  for (const el of await buttons.all()) {
    const box = (await el.boundingBox())!;
    expect(Math.min(box.width, box.height), "demo list button target").toBeGreaterThanOrEqual(24);
  }

  // The playground match rows are click targets too — one pick opens the detail.
  await page.goto("/dbtl/playground/");
  await page.waitForLoadState("networkidle");
  const rows = page.locator(".pg-match li[data-id]");
  expect(await rows.count()).toBeGreaterThan(3);
  for (const el of await rows.all()) {
    const box = (await el.boundingBox())!;
    expect(box.height, "match row target height").toBeGreaterThanOrEqual(24);
  }
});

// The lineage diagrams (pre.lin) dim their context nodes below the contrast
// threshold on purpose — the faded cells are decoration; the same names are
// real text in the model list and the inspector. That region is excluded here.
for (const [name, path] of [
  ["home", "/dbtl/"],
  ["docs", "/dbtl/cli/lint/"],
  ["playground", "/dbtl/playground/"],
  ["404", "/dbtl/404.html"],
] as const) {
  test(`axe: ${name} page has no violations`, async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await page.goto(path);
    await page.waitForLoadState("networkidle");
    const results = await new AxeBuilder({ page }).exclude("pre.lin").analyze();
    expect(
      results.violations.map((v) => `${v.id}: ${v.nodes.length} node(s)`),
      JSON.stringify(
        results.violations.map((v) => ({ id: v.id, targets: v.nodes.map((n) => n.target) })),
        null,
        1,
      ),
    ).toEqual([]);
  });
}
