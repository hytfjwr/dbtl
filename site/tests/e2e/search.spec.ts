import { expect, test } from "@playwright/test";

test("search opens from the button and closes from its close control", async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 640 });
  await page.goto("/dbtl/getting-started/overview/");
  await page.waitForLoadState("networkidle");

  const dialog = page.locator("dialog#search-modal");
  await page.locator(".search-btn").click();
  await expect(dialog).toHaveAttribute("open", "");
  await expect(dialog).toBeVisible();

  await dialog.locator(".s-close").click();
  await expect(dialog).not.toBeVisible();
});
