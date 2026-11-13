import { expect, test } from "@playwright/test";
import navJson from "../../src/nav.json" with { type: "json" };

// Every sidebar page plus the routes outside the nav (home, playground, 404):
// all of them have to fit narrow screens without a horizontal scrollbar.
const docPaths = (navJson as Array<{ items: string[] }>).flatMap((group) =>
  group.items.map((slug) => `${slug}/`),
);
const paths = ["", ...docPaths, "playground/", "404.html"];
const widths = [320, 360];

for (const width of widths) {
  for (const path of paths) {
    test(`no horizontal overflow: /dbtl/${path} at ${width}px`, async ({ page }) => {
      await page.setViewportSize({ width, height: 720 });
      await page.goto(`/dbtl/${path}`);
      // Fonts and layout-affecting scripts must settle before measuring.
      await page.waitForLoadState("networkidle");
      await page.evaluate(async () => {
        await document.fonts.ready;
      });
      // The stylesheet clips horizontal overflow on <body>; undo that so an
      // overflowing element cannot hide behind the clip.
      await page.addStyleTag({ content: "body { overflow-x: visible !important; }" });
      const [scrollWidth, innerWidth] = await page.evaluate(() => [
        document.documentElement.scrollWidth,
        window.innerWidth,
      ]);
      expect(scrollWidth).toBeLessThanOrEqual(innerWidth);
    });
  }
}
