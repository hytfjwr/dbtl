import { expect, test } from "@playwright/test";

// Single-character shortcuts (/ t 1-3) can be switched off in the status line.
// The choice is stored and the hints dim; keys with modifiers still work.

const toggle = (page: import("@playwright/test").Page) => page.locator("[data-keys-toggle]");
const dialog = (page: import("@playwright/test").Page) => page.locator("dialog#search-modal");

test("single-key shortcuts can be switched off and back on", async ({ page }) => {
  await page.goto("/dbtl/getting-started/overview/");
  await page.waitForLoadState("networkidle");

  // On by default: "/" opens search, "t" cycles the theme.
  await expect(toggle(page)).toHaveText("keys:on");
  await page.keyboard.press("/");
  await expect(dialog(page)).toHaveAttribute("open", "");
  await page.keyboard.press("Escape");
  await expect(dialog(page)).not.toHaveAttribute("open", "");

  const chip = page.locator("#chip-theme");
  const themeBefore = await chip.textContent();
  await page.keyboard.press("t");
  await expect(chip).not.toHaveText(themeBefore!);

  // Off: the button reports it, the hints dim, and the keys go quiet.
  await toggle(page).click();
  await expect(toggle(page)).toHaveText("keys:off");
  await expect(toggle(page)).toHaveAttribute("aria-pressed", "false");
  await expect(page.locator("html")).toHaveAttribute("data-keys", "off");

  const themeNow = await chip.textContent();
  await page.keyboard.press("/");
  await expect(dialog(page)).not.toHaveAttribute("open", "");
  await page.keyboard.press("t");
  await expect(chip).toHaveText(themeNow!);
  await page.keyboard.press("2");
  expect(page.url()).toContain("/getting-started/overview/");

  // Modifier chords and pointer input are unaffected.
  await page.keyboard.press("Control+k");
  await expect(dialog(page)).toHaveAttribute("open", "");
  await page.keyboard.press("Escape");
  await page.locator('.hints [data-act="theme"]').click();
  await expect(chip).not.toHaveText(themeNow!);

  // On again.
  await toggle(page).click();
  await expect(toggle(page)).toHaveText("keys:on");
  await page.keyboard.press("/");
  await expect(dialog(page)).toHaveAttribute("open", "");
});

test("the shortcuts choice persists across loads", async ({ page }) => {
  await page.goto("/dbtl/getting-started/overview/");
  await page.waitForLoadState("networkidle");
  await toggle(page).click();
  await expect(toggle(page)).toHaveText("keys:off");

  await page.goto("/dbtl/cli/overview/");
  await expect(page.locator("html")).toHaveAttribute("data-keys", "off");
  await expect(toggle(page)).toHaveText("keys:off");
  await page.keyboard.press("/");
  await expect(dialog(page)).not.toHaveAttribute("open", "");
});
