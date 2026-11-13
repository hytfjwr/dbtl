import { expect, test } from "@playwright/test";
import navJson from "../../src/nav.json" with { type: "json" };

// The docs drawer is a touch-device feature: it replaces the sidebar below the
// 900px breakpoint and is operated by taps.
test.use({ hasTouch: true, isMobile: true });

const viewports = [
  { width: 375, height: 667 },
  { width: 320, height: 568 },
];

// The sidebar's last link: the last item of the last nav group, so it moves
// with the nav instead of being a frozen name.
const navGroups = navJson as Array<{ items: string[] }>;
const lastSlug = navGroups.at(-1)!.items.at(-1)!;
const LAST_LINK = `a[href$="${lastSlug}/"]`;

// Everything chrome.ts inerts while the drawer is open.
const INERT_REGIONS = [
  ".skip",
  ".top",
  ".docbar",
  ".doc-main",
  ".toc",
  ".site-foot",
  ".statusline",
];

for (const viewport of viewports) {
  test(`docs drawer at ${viewport.width}x${viewport.height}`, async ({ page }) => {
    await page.setViewportSize(viewport);
    await page.goto("/dbtl/getting-started/overview/");
    await page.waitForLoadState("networkidle");

    const openButton = page.locator("#open-drawer");
    const sidebar = page.locator("#sidebar");
    const scrim = page.locator("#scrim");
    const closeButton = page.locator("#close-drawer");

    // Park the page partway down so a background scroll would be measurable.
    await page.evaluate(() => {
      window.scrollTo({ top: document.documentElement.scrollHeight / 2, behavior: "instant" });
    });
    const scrollBefore = await page.evaluate(() => window.scrollY);
    expect(scrollBefore).toBeGreaterThan(0);

    // Menu opens the drawer.
    await openButton.tap();
    await expect(sidebar).toHaveClass(/open/);
    await expect(sidebar).toBeVisible();

    // Every region outside the drawer is inert while it is open.
    for (const selector of INERT_REGIONS) {
      await expect(page.locator(selector)).toHaveAttribute("inert", "");
    }

    // The drawer fills the viewport: that is what lets it scroll its own list.
    const box = await sidebar.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.height).toBeCloseTo(viewport.height, 0);

    // The last link scrolls into view inside the drawer.
    const lastLink = sidebar.locator(LAST_LINK);
    await lastLink.scrollIntoViewIfNeeded();
    await expect(lastLink).toBeInViewport();
    expect(await sidebar.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);

    // The parked page behind cannot scroll: while the drawer is open its
    // overflow lock makes a wheel on the uncovered scrim a no-op.
    const settle = () =>
      page.evaluate(() => new Promise((r) => requestAnimationFrame(() => r(undefined))));
    await page.mouse.move(viewport.width - 4, viewport.height / 2);
    await page.mouse.wheel(0, 1200);
    await settle();
    expect(await page.evaluate(() => window.scrollY)).toBe(scrollBefore);

    // Wheeling the drawer past its end stays in the drawer too: the
    // overscroll containment does not chain into the page.
    await page.mouse.move(viewport.width / 2, viewport.height / 2);
    await page.mouse.wheel(0, 1200);
    await settle();
    expect(await page.evaluate(() => window.scrollY)).toBe(scrollBefore);

    // Esc closes, inert comes off and focus returns to the Menu button.
    await page.keyboard.press("Escape");
    await expect(sidebar).not.toHaveClass(/open/);
    for (const selector of INERT_REGIONS) {
      await expect(page.locator(selector)).not.toHaveAttribute("inert", "");
    }
    await expect(openButton).toBeFocused();
    expect(await page.evaluate(() => window.scrollY)).toBe(scrollBefore);

    // The close button closes, inert comes off and focus returns.
    await openButton.tap();
    await expect(sidebar).toHaveClass(/open/);
    await closeButton.tap();
    await expect(sidebar).not.toHaveClass(/open/);
    for (const selector of INERT_REGIONS) {
      await expect(page.locator(selector)).not.toHaveAttribute("inert", "");
    }
    await expect(openButton).toBeFocused();

    // A tap on the scrim (the part not covered by the drawer) closes too.
    await openButton.tap();
    await expect(sidebar).toHaveClass(/open/);
    await scrim.tap({ position: { x: viewport.width - 4, y: viewport.height / 2 } });
    await expect(sidebar).not.toHaveClass(/open/);
    for (const selector of INERT_REGIONS) {
      await expect(page.locator(selector)).not.toHaveAttribute("inert", "");
    }
    await expect(openButton).toBeFocused();

    // The last link is actually pressable: tapping it navigates.
    await openButton.tap();
    await expect(sidebar).toHaveClass(/open/);
    await lastLink.scrollIntoViewIfNeeded();
    await lastLink.tap();
    await page.waitForURL(`**/${lastSlug}/`);
  });
}
