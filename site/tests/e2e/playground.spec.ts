import { expect, test, type Page } from "@playwright/test";

// The playground's contract is the wasm engine in a worker: every assertion
// below waits on rendered output, never on a clock.
const ready = (page: Page) =>
  expect(page.locator("#pg-status")).toContainText("ready", { timeout: 30_000 });

test("SSR ships engine output before hydration", async ({ page }) => {
  // The initial HTML must already contain the engine-rendered lineage and
  // lint report — the page is useful before the worker boots.
  const html = await (await page.request.get("/dbtl/playground/")).text();
  expect(html).toContain('class="lin-stage"');
  expect(html).toContain("stg_payment__scores");
  expect(html).toContain("matched <b>6</b> of 93");
  expect(html).toContain("select_star");
});

test("selector, lineage, and lint run through the wasm engine", async ({ page }) => {
  await page.goto("/dbtl/playground/");
  await ready(page);

  // The default selector paints the same state the SSR shipped.
  await expect(page.locator("#pg-match-title")).toContainText("matched 6 of 93");
  await expect(page.locator("#pg-lin .lin-stage")).toBeVisible();
  await expect(page.locator("#pg-lin .nm.hit")).toHaveCount(6);

  // Lint output is the CLI's: rule ids, source positions, excerpts, docs links.
  await page.locator("#pt-lint").click();
  const lint = page.locator("#pg-lint");
  await expect(lint).toContainText("warning[select_star]");
  await expect(lint).toContainText("stg_payment__scores.sql:2:8");
  await expect(lint).toContainText("= docs:");
  expect(await lint.locator("a").count()).toBeGreaterThan(0);
  await expect(page.locator("#pg-lint-foot")).toContainText("exit 0");

  // A new selector re-runs select + lineage + lint in the worker.
  await page.locator("#pt-sel").click();
  await page.fill("#pg-sel", "tag:marts");
  await expect(page.locator("#pg-match-title")).toContainText("matched 9 of 93");
  await expect(page.locator("#pg-lin .nm.hit")).toHaveCount(9);

  // Selector errors surface instead of a stale diagram.
  await page.fill("#pg-sel", "tag:no_such_tag");
  await expect(page.locator("#pg-match-title")).toContainText("matched 0 of 93");
});

test("editing a model's SQL re-lints; reset restores", async ({ page }) => {
  await page.goto("/dbtl/playground/");
  await ready(page);
  await page.locator("#pt-lint").click();
  await expect(page.locator("#pg-lint")).toContainText("stg_payment__scores's final SELECT");

  // Pick the first matched node; the editor fills with its raw SQL.
  await page.locator("#pt-sel").click();
  await page.locator("#pg-match li[data-id]").first().click();
  await expect(page.locator("#pg-ed-name")).toContainText("stg_payment__scores");
  await expect(page.locator("#pg-ed-sql")).not.toHaveValue("");

  // Dropping the `select *` removes that node's finding from the report.
  await page.fill("#pg-ed-sql", "select 1 as id");
  await expect(page.locator("#pg-lint")).not.toContainText("stg_payment__scores's final SELECT");
  await expect(page.locator("#pg-lint")).toContainText("stg_payment__shoppers's final SELECT");

  // Reset reverts the engine's SQL and the finding returns.
  await page.locator("#pg-ed-revert").click();
  await expect(page.locator("#pg-lint")).toContainText("stg_payment__scores's final SELECT");
  await expect(page.locator("#pg-ed-sql")).toHaveValue(/select \*/);
});

test(".dbtl.yml edits re-lint and config errors surface", async ({ page }) => {
  await page.goto("/dbtl/playground/");
  await ready(page);
  await page.locator("#pt-lint").click();
  await expect(page.locator("#pg-lint")).toContainText("warning[select_star]");

  // A severity override changes the verdict like the CLI would.
  await page.fill("#pg-cfg", "lint:\n  rules:\n    select_star: {severity: error}\n");
  await expect(page.locator("#pg-lint")).toContainText("error[select_star]");
  await expect(page.locator("#pg-lint-foot")).toContainText("exit 1");

  // A broken config reads as an engine error, not a crash.
  await page.fill("#pg-cfg", "[not yaml at all");
  await expect(page.locator("#pg-cfg-err")).toContainText("invalid .dbtl.yml");
  await expect(page.locator("#pg-lint")).toContainText("error: invalid .dbtl.yml");

  // Reset returns the shipped defaults.
  await page.locator("#pg-cfg-revert").click();
  await expect(page.locator("#pg-lint")).toContainText("warning[select_star]");
  await expect(page.locator("#pg-lint-foot")).toContainText("exit 0");
});

test("the columns tab reports column-level lineage", async ({ page }) => {
  await page.goto("/dbtl/playground/");
  await ready(page);
  await page.locator("#pt-cols").click();

  // Pick a node; its first column's downstream report renders immediately.
  const node = page.locator("#pg-col-node");
  await expect(node.locator("option")).not.toHaveCount(1);
  const uid = await page
    .locator("#pg-col-node option", { hasText: "stg_payment__scores" })
    .getAttribute("value");
  await node.selectOption(uid!);
  await expect(page.locator("#pg-cols")).toContainText("# Column lineage:");
  await expect(page.locator("#pg-cols")).toContainText("stg_payment__scores.");
});

test("a local manifest.json loads in memory only", async ({ page }) => {
  // Fetch the bundled sample manifest, then hand it back as an upload — the
  // file never leaves the page; lint on it runs with no .yml excerpts.
  await page.goto("/dbtl/playground/");
  await ready(page);
  const manifest = await (await page.request.get("/dbtl/samples/sample/manifest.json")).body();
  await page.locator("#pg-file").setInputFiles({
    name: "my-manifest.json",
    mimeType: "application/json",
    buffer: manifest,
  });

  await expect(page.locator("#pg-file-hint")).toContainText(
    "my-manifest.json — 30 nodes, kept in memory only",
  );
  await expect(page.locator("#pg-match-title")).toContainText("of 30");
  await page.locator("#pt-lint").click();
  await expect(page.locator("#pg-lint-foot")).toContainText("excerpts cite positions only");
});

test("nothing on the playground requests an external origin", async ({ page }) => {
  const leaked: string[] = [];
  page.on("request", (r) => {
    const u = r.url();
    if (u.startsWith("http") && !u.startsWith("http://localhost:4377/")) leaked.push(u);
  });
  await page.goto("/dbtl/playground/");
  await ready(page);

  // Exercise every code path that could conceivably fetch: select, lint,
  // columns, and a project switch.
  await page.fill("#pg-sel", "tag:marts");
  await expect(page.locator("#pg-match-title")).toContainText("matched 9 of 93");
  await page.locator("#pt-lint").click();
  await expect(page.locator("#pg-lint")).toContainText("select_star");
  await page.locator("#pt-cols").click();
  await page.locator("#pg-col-node").selectOption({ index: 1 });
  await expect(page.locator("#pg-cols")).toContainText("# Column lineage:");
  await page.locator('#pg-proj button[data-p="sample"]').click();
  await expect(page.locator("#pg-match-title")).toContainText("of 30");
  await page.locator("#pt-sel").click();
  await expect(page.locator("#pg-lin .lin-stage")).toBeVisible();

  expect(leaked).toEqual([]);
});
