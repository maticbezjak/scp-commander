// Ghost placeholder rows: a file mid-transfer shows in its DESTINATION pane as
// a dimmed `tr.pending` row with a live mini progress bar, until the real
// listing replaces it. Source: App.svelte pendingFor()/remotePending +
// Pane.svelte {#each pending}.
import { test, expect } from "@playwright/test";
import { open, connect, fireXfer, row } from "./harness.js";

const SESSION = 1; // stub connect_session returns session_id: 1
const ghosts = (page) => page.locator(".pane[data-kind=remote] tbody tr.pending");

/** An upload "started" payload, as onXfer() consumes it. */
const started = (extra = {}) => ({
  event: "started",
  id: 1,
  session: SESSION,
  name: "payload.bin",
  upload: true,
  total: 1000,
  local: "/home/user/payload.bin",
  remote: "/data/payload.bin",
  is_dir: false,
  overwrite: 0,
  ...extra,
});

const progress = (done, total = 1000, id = 1) => ({ event: "progress", id, session: SESSION, done, total });

/** Rendered width of .ghost-fill as a fraction of its .ghost-bar track. */
async function fillRatio(page) {
  const bar = await page.locator(".pane[data-kind=remote] tr.pending .ghost-bar").boundingBox();
  const fill = await page.locator(".pane[data-kind=remote] tr.pending .ghost-fill").boundingBox();
  return fill.width / bar.width;
}

test("upload into the shown remote dir renders a live ghost row", async ({ page }) => {
  await open(page);
  await connect(page);
  await expect(ghosts(page)).toHaveCount(0);

  await fireXfer(page, started());
  await expect(ghosts(page)).toHaveCount(1);
  await expect(ghosts(page).locator(".nm")).toHaveText("payload.bin");
  // No progress yet: the bar reads 0% and the fill is empty.
  await expect(ghosts(page).locator(".ghost-pct")).toHaveText("↑ 0%");
  expect(await fillRatio(page)).toBeCloseTo(0, 2);

  await fireXfer(page, progress(440));
  await expect(ghosts(page).locator(".ghost-pct")).toHaveText("↑ 44%");
  expect(await fillRatio(page)).toBeCloseTo(0.44, 1);

  // The fill tracks progress rather than jumping straight to full.
  await fireXfer(page, progress(910));
  await expect(ghosts(page).locator(".ghost-pct")).toHaveText("↑ 91%");
  expect(await fillRatio(page)).toBeCloseTo(0.91, 1);

  // The ghost is a placeholder, not a real entry — it carries no data-name.
  await expect(ghosts(page)).not.toHaveAttribute("data-name", /./);
});

test("regression: the ghost is a single-line row with the bar right of the name", async ({ page }) => {
  await open(page);
  await connect(page);
  await fireXfer(page, started());
  await fireXfer(page, progress(500));
  await expect(ghosts(page).locator(".ghost-pct")).toHaveText("↑ 50%");

  const ghostBox = await ghosts(page).boundingBox();
  const normalBox = await row(page, "remote", "config.xml").boundingBox();

  // It once wrapped to two lines: assert it stays the height of a normal row.
  expect(ghostBox.height).toBeLessThan(normalBox.height * 1.5);
  expect(ghostBox.height).toBeCloseTo(normalBox.height, 0);

  // The bar lives in the cell to the RIGHT of the name cell, not under it.
  const nameBox = await ghosts(page).locator("td.name").boundingBox();
  const barBox = await ghosts(page).locator(".ghost-bar").boundingBox();
  expect(barBox.x).toBeGreaterThanOrEqual(nameBox.x + nameBox.width);
  // Same line: the bar's vertical centre sits inside the name cell's band.
  const barMid = barBox.y + barBox.height / 2;
  expect(barMid).toBeGreaterThan(nameBox.y);
  expect(barMid).toBeLessThan(nameBox.y + nameBox.height);
});

test("regression: the ghost disappears on done (no stuck 100% rows)", async ({ page }) => {
  await open(page);
  await connect(page);
  await fireXfer(page, started());
  await fireXfer(page, progress(1000));
  await expect(ghosts(page)).toHaveCount(1);
  await expect(ghosts(page).locator(".ghost-pct")).toHaveText("↑ 100%");

  await fireXfer(page, { event: "done", id: 1, session: SESSION, name: "payload.bin", upload: true });

  // The uploaded file is absent from the refreshed listing (the stub always
  // returns the same entries) — the ghost must still go, not linger at 100%.
  await expect(ghosts(page)).toHaveCount(0);
  await expect(page.locator(".pane[data-kind=remote] tbody tr[data-name]")).toHaveCount(2);
});

test("a folder upload ghosts with a folder icon", async ({ page }) => {
  await open(page);
  await connect(page);
  await fireXfer(page, started({ name: "assets", remote: "/data/assets", is_dir: true, total: 0 }));

  await expect(ghosts(page)).toHaveCount(1);
  await expect(ghosts(page).locator(".nm")).toHaveText("assets");
  await expect(ghosts(page).locator("svg.ti.dir")).toBeVisible();
  await expect(ghosts(page).locator("svg.ti.file")).toHaveCount(0);
  // A dir has no byte total, so the bar stays at 0%.
  await expect(ghosts(page).locator(".ghost-pct")).toHaveText("↑ 0%");
});

test("a ghost only shows in the pane displaying the transfer's destination dir", async ({ page }) => {
  await open(page);
  await connect(page);

  // Destination /other while the pane shows /data — no ghost.
  await fireXfer(page, started({ id: 7, name: "elsewhere.bin", remote: "/other/elsewhere.bin" }));
  await expect(page.locator(".pane[data-kind=remote] .pathbar")).toHaveValue("/data");
  await expect(ghosts(page)).toHaveCount(0);

  // Control: a sibling transfer into /data does ghost, so the pane is live.
  await fireXfer(page, started({ id: 8, name: "here.bin", remote: "/data/here.bin" }));
  await expect(ghosts(page)).toHaveCount(1);
  await expect(ghosts(page).locator(".nm")).toHaveText("here.bin");

  // Nested destination /data/drop is a different dir than /data — still one.
  await fireXfer(page, started({ id: 9, name: "deep.bin", remote: "/data/drop/deep.bin" }));
  await expect(ghosts(page)).toHaveCount(1);
});
