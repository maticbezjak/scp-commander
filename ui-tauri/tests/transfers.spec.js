import { test, expect } from "@playwright/test";
import { open, connect, fireXfer } from "./harness.js";

// Payload shapes mirror the backend's xfer events (see App.svelte onXfer).
const started = (id, name, extra = {}) => ({
  event: "started",
  session: 1,
  id,
  name,
  upload: true,
  total: 0,
  local: `/home/user/${name}`,
  remote: `/data/${name}`,
  is_dir: false,
  overwrite: 0,
  ...extra,
});
const progress = (id, done, total) => ({ event: "progress", session: 1, id, done, total });
const done = (id, name, extra = {}) => ({ event: "done", session: 1, id, name, upload: true, ...extra });

test("queue panel is absent until a transfer starts, then shows the file", async ({ page }) => {
  await open(page);
  await connect(page);

  await expect(page.locator(".queue")).toHaveCount(0);

  await fireXfer(page, started(1, "big.iso"));

  await expect(page.locator(".queue")).toBeVisible();
  await expect(page.locator(".queue .qrow")).toHaveCount(1);
  await expect(page.locator(".queue .qrow .qname")).toHaveText("big.iso");
  // Uploads are marked with an up-arrow glyph while active.
  await expect(page.locator(".queue .qrow .qg")).toHaveText("↑");
  await expect(page.locator(".queue .qhead")).toContainText("Transfers");
});

test("REGRESSION: queue panel renders above the status bar, not clipped", async ({ page }) => {
  await open(page);
  await connect(page);
  await fireXfer(page, started(1, "big.iso"));
  await expect(page.locator(".queue")).toBeVisible();

  const queue = await page.locator(".queue").boundingBox();
  const status = await page.locator(".statusbar").boundingBox();
  const viewport = page.viewportSize();

  expect(queue).not.toBeNull();
  expect(status).not.toBeNull();
  // The panel has real height (it used to collapse / render off the bottom).
  expect(queue.height).toBeGreaterThan(0);
  expect(queue.width).toBeGreaterThan(0);
  // …and sits entirely above the status bar, which stays the last row.
  expect(queue.y + queue.height).toBeLessThanOrEqual(status.y + 1);
  // …and is fully on-screen.
  expect(queue.y).toBeGreaterThanOrEqual(0);
  expect(queue.y + queue.height).toBeLessThanOrEqual(viewport.height);
  expect(status.y + status.height).toBeLessThanOrEqual(viewport.height + 1);
  // The queue row itself is visible, not clipped out of the panel.
  await expect(page.locator(".queue .qrow .qname")).toBeVisible();
});

test("progress events update the per-file percent and the aggregate header", async ({ page }) => {
  await open(page);
  await connect(page);
  await fireXfer(page, started(1, "big.iso"));
  await fireXfer(page, started(2, "notes.txt"));
  await expect(page.locator(".queue .qrow")).toHaveCount(2);
  await expect(page.locator(".queue .qrow .qpct")).toHaveText(["0%", "0%"]);

  // One sample per file: no speed is derived yet, so the header shows no rate.
  await fireXfer(page, progress(1, 512, 1024));
  await fireXfer(page, progress(2, 256, 1024));

  await expect(page.locator(".queue .qrow .qpct")).toHaveText(["50%", "25%"]);
  // 768 B of 2048 B across 2 active transfers => 38%.
  await expect(page.locator(".queue .qhead .agg")).toContainText("2 active · 768 B / 2.0 KB · 38%");
  await expect(page.locator(".queue .qhead .aggbar")).toHaveJSProperty("value", 38);
  await expect(page.locator(".queue .qrow").first().locator(".qstat")).toContainText("512 B / 1.0 KB");
  // The status bar mirrors the aggregate while transfers are in flight.
  await expect(page.locator(".statusbar .xfer-ind")).toContainText("Transferring 2 files — 38%");
});

test("a done event marks the row done and the status bar reports completion", async ({ page }) => {
  await open(page);
  await connect(page);
  await fireXfer(page, started(1, "big.iso"));
  await fireXfer(page, progress(1, 512, 1024));
  await expect(page.locator(".queue .qrow .qpct")).toHaveText("50%");

  // Record every status the bar passes through, so we can prove the completion
  // message is the one that STICKS (the destination-pane refresh that follows
  // must not overwrite it).
  await page.evaluate(() => {
    window.__stxt = [];
    const el = document.querySelector(".statusbar .stxt");
    new MutationObserver(() => window.__stxt.push(el.textContent)).observe(el, {
      childList: true, characterData: true, subtree: true,
    });
  });

  await fireXfer(page, done(1, "big.iso"));

  const row = page.locator(".queue .qrow");
  await expect(row).toHaveClass(/\bdone\b/);
  await expect(row.locator(".qg")).toHaveText("✓");
  await expect(row.locator(".qpct")).toHaveText("100%");
  await expect(row.locator(".qstat")).toHaveText("done · 1.0 KB");
  // No transfer is active any more, so the in-flight indicator goes away…
  await expect(page.locator(".statusbar .xfer-ind")).toHaveCount(0);
  // …and the queue-drained effect puts the completion message in the status bar.
  await expect
    .poll(() => page.evaluate(() => window.__stxt))
    .toContain("✓ All transfers complete");

  // REGRESSION: the "done" handler also refreshes the destination pane. That
  // refresh is silent (record: false), so it must NOT clobber the completion
  // message — which used to land for one tick and then be replaced by
  // "<path> — N item(s)".
  await expect(page.locator(".statusbar .stxt")).toHaveText("✓ All transfers complete");
  expect(await page.evaluate(() => window.__stxt)).toEqual(["✓ All transfers complete"]);
});
