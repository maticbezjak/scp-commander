import { test, expect } from "@playwright/test";
import { open, connect, closeLogin, fireXfer, calls } from "./harness.js";

// A "started" xfer payload as the backend emits it.
const started = (over = {}) => ({
  event: "started",
  id: 1,
  session: 1,
  name: "big.iso",
  upload: true,
  total: 4096,
  local: "/home/user/big.iso",
  remote: "/data/big.iso",
  is_dir: false,
  overwrite: 0,
  ...over,
});

const qhead = (page) => page.locator(".queue .qhead");
const pauseBtn = (page) => page.locator(".queue .qhead .qctl");
const speedSel = (page) => page.locator(".queue .qhead .qspeed");
const retryAll = (page) => page.locator(".queue .qhead .retry-all");
const qrow = (page, name) => page.locator(".queue .qrow").filter({ has: page.locator(".qname", { hasText: name }) });
const lsGet = (page, key) => page.evaluate((k) => localStorage.getItem(k), key);

test("pause button toggles and drives set_paused on the backend", async ({ page }) => {
  await open(page);
  await connect(page);
  await fireXfer(page, started());

  await expect(qhead(page)).toBeVisible();
  await expect(pauseBtn(page)).toHaveText("❚❚ Pause");
  await expect(speedSel(page)).toBeVisible();

  await pauseBtn(page).click();
  await expect(pauseBtn(page)).toHaveText("▶ Resume");
  await expect(pauseBtn(page)).toHaveClass(/\bon\b/);
  expect((await calls(page, "set_paused")).at(-1)).toEqual({ paused: true });
  await expect(page.locator(".statusbar")).toContainText("Transfers paused");

  await pauseBtn(page).click();
  await expect(pauseBtn(page)).toHaveText("❚❚ Pause");
  expect((await calls(page, "set_paused")).at(-1)).toEqual({ paused: false });
  await expect(page.locator(".statusbar")).toContainText("Transfers resumed");
});

test("speed limit select drives set_speed_limit and persists to localStorage", async ({ page }) => {
  await open(page);
  await connect(page);
  await fireXfer(page, started());

  await expect(speedSel(page)).toHaveValue("0");

  await speedSel(page).selectOption({ label: "1 MB/s" });

  await expect(speedSel(page)).toHaveValue("1024");
  await expect
    .poll(async () => (await calls(page, "set_speed_limit")).at(-1))
    .toEqual({ kbs: 1024 });
  await expect.poll(() => lsGet(page, "speed-kbs")).toBe("1024");
});

test("a failed transfer shows the error on its row and offers Retry all", async ({ page }) => {
  await open(page);
  await connect(page);
  await fireXfer(page, started({ name: "report.pdf" }));
  await expect(qrow(page, "report.pdf")).toHaveClass(/\bactive\b/);

  await fireXfer(page, { event: "failed", id: 1, session: 1, message: "connection reset" });

  const rowLoc = qrow(page, "report.pdf");
  await expect(rowLoc).toHaveClass(/\bfailed\b/);
  await expect(rowLoc.locator(".qstat")).toHaveText("failed: connection reset");
  await expect(retryAll(page)).toHaveText("↻ Retry all (1)");

  await retryAll(page).click();
  // Retrying re-enqueues with resume and drops the failed row from the queue.
  expect((await calls(page, "enqueue")).at(-1)).toMatchObject({ name: "report.pdf", resume: true });
  await expect(rowLoc).toHaveCount(0);
  await expect(retryAll(page)).toHaveCount(0);
});

test("REGRESSION: queue survives a restart — in-flight and failed come back resumable", async ({ page }) => {
  await open(page);
  await connect(page);

  // One transfer still in flight, one already failed.
  await fireXfer(page, started({ id: 1, name: "inflight.bin", remote: "/data/inflight.bin" }));
  await fireXfer(page, started({ id: 2, name: "broken.bin", remote: "/data/broken.bin" }));
  await fireXfer(page, { event: "failed", id: 2, session: 1, message: "disk full" });

  await expect(qrow(page, "inflight.bin")).toHaveClass(/\bactive\b/);
  await expect(qrow(page, "broken.bin")).toHaveClass(/\bfailed\b/);
  // The slim snapshot must have landed before we restart.
  await expect.poll(async () => JSON.parse((await lsGet(page, "xfer-queue")) ?? "[]").length).toBe(2);

  await page.reload();
  await page.waitForSelector(".pane[data-kind=local]");
  await closeLogin(page);

  // Both rows are back, both resumable, in their original order.
  await expect(page.locator(".queue .qrow")).toHaveCount(2);
  expect(await page.locator(".queue .qrow .qname").allInnerTexts()).toEqual(["inflight.bin", "broken.bin"]);
  await expect(page.locator(".queue .qrow.failed")).toHaveCount(2);

  // The interrupted one is relabelled; the genuinely failed one keeps its error.
  await expect(qrow(page, "inflight.bin").locator(".qstat")).toHaveText("failed: interrupted (app closed)");
  await expect(qrow(page, "broken.bin").locator(".qstat")).toHaveText("failed: disk full");
  // Live progress is reset, so both restart from 0%.
  await expect(qrow(page, "inflight.bin").locator(".qpct")).toHaveText("0%");

  await expect(retryAll(page)).toHaveText("↻ Retry all (2)");
});
