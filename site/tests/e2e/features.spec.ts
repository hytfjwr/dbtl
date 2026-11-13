import { expect, test } from "@playwright/test";

// The home demo, the theme switch, code-block copy buttons and the docs "On
// this page" list — exercised end to end against the built site.

test("home demo: picking a model re-roots the lineage and the inspector", async ({ page }) => {
  await page.goto("/dbtl/");
  await page.waitForLoadState("networkidle");

  const current = page.locator('#hero-list button[aria-current="true"]');
  const firstName = (await current.textContent())!.trim();
  await expect(page.locator("#hero-tab")).toContainText(firstName);

  const first = page.locator('#hero-list button:not([aria-current="true"])').first();
  const otherName = (await first.textContent())!.trim();
  const otherId = await first.getAttribute("data-id");
  const other = page.locator(`#hero-list button[data-id="${otherId}"]`);
  await other.click();

  await expect(page.locator("#hero-tab")).toContainText(otherName);
  await expect(other).toHaveAttribute("aria-current", "true");
  await expect(page.locator("#hero-insp")).toContainText(otherName);
});

test("the theme choice applies, is reported, and persists", async ({ page }) => {
  await page.goto("/dbtl/getting-started/overview/");
  await page.waitForLoadState("networkidle");

  const html = page.locator("html");
  const dark = page.locator('[data-theme-set="dark"]');
  const system = page.locator('[data-theme-set="system"]');

  await dark.click();
  await expect(html).toHaveAttribute("data-theme", "dark");
  await expect(dark).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator("#chip-theme")).toHaveText("theme:dark");

  await page.reload();
  await expect(html).toHaveAttribute("data-theme", "dark");
  await expect(page.locator("#chip-theme")).toHaveText("theme:dark");

  await system.click();
  await expect(html).not.toHaveAttribute("data-theme", "dark");
  await page.reload();
  await expect(html).not.toHaveAttribute("data-theme", "dark");
  await expect(page.locator("#chip-theme")).toHaveText("theme:system");
});

test("a code pane's copy button puts its commands on the clipboard", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.goto("/dbtl/cli/lint/");
  await page.waitForLoadState("networkidle");

  const button = page.locator(".code .copy").first();
  await expect(button).toHaveText("copy");
  await button.click();
  await expect(button).toHaveText("copied");
  const text = await page.evaluate(() => navigator.clipboard.readText());
  expect(text.length).toBeGreaterThan(0);
  await expect(button).toHaveText("copy", { timeout: 3000 });
});

test("the on-this-page list tracks and follows the headings", async ({ page }) => {
  await page.goto("/dbtl/cli/lint/");
  await page.waitForLoadState("networkidle");

  const toc = page.locator(".toc");
  const links = toc.locator("a[href^='#']");
  expect(await links.count()).toBeGreaterThan(1);

  // The first entry is current at the top of the page.
  await expect(links.first()).toHaveAttribute("aria-current", "location");

  // Following a link scrolls to its heading and marks that entry.
  const target = links.nth(1);
  const id = (await target.getAttribute("href"))!;
  await target.click();
  expect(new URL(page.url()).hash).toBe(id);
  await expect(target).toHaveAttribute("aria-current", "location");
  await expect(links.first()).not.toHaveAttribute("aria-current", "location");
});
