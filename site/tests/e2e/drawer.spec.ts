import { expect, test } from "@playwright/test";

// The docs drawer is a touch-device feature: it replaces the sidebar below the
// 900px breakpoint and is operated by taps.
test.use({ hasTouch: true, isMobile: true });

const viewports = [
  { width: 375, height: 667 },
  { width: 320, height: 568 },
];

// The sidebar's last link, from the last group in the nav.
const LAST_LINK = 'a[href$="reference/config/"]';

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

    // The drawer fills the viewport: that is what lets it scroll its own list.
    const box = await sidebar.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.height).toBeCloseTo(viewport.height, 0);

    // The last link scrolls into view inside the drawer.
    const lastLink = sidebar.locator(LAST_LINK);
    await lastLink.scrollIntoViewIfNeeded();
    await expect(lastLink).toBeInViewport();
    expect(await sidebar.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);

    // The page behind does not move while the drawer scrolls.
    expect(await page.evaluate(() => window.scrollY)).toBe(scrollBefore);

    // Esc closes and focus returns to the Menu button.
    await page.keyboard.press("Escape");
    await expect(sidebar).not.toHaveClass(/open/);
    await expect(openButton).toBeFocused();
    expect(await page.evaluate(() => window.scrollY)).toBe(scrollBefore);

    // The close button closes and focus returns.
    await openButton.tap();
    await expect(sidebar).toHaveClass(/open/);
    await closeButton.tap();
    await expect(sidebar).not.toHaveClass(/open/);
    await expect(openButton).toBeFocused();

    // A tap on the scrim (the part not covered by the drawer) closes too.
    await openButton.tap();
    await expect(sidebar).toHaveClass(/open/);
    await scrim.tap({ position: { x: viewport.width - 4, y: viewport.height / 2 } });
    await expect(sidebar).not.toHaveClass(/open/);
    await expect(openButton).toBeFocused();

    // The last link is actually pressable: tapping it navigates.
    await openButton.tap();
    await expect(sidebar).toHaveClass(/open/);
    await lastLink.scrollIntoViewIfNeeded();
    await lastLink.tap();
    await page.waitForURL("**/reference/config/");
  });
}
