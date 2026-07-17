// Row virtualization in Pane.svelte: only the rows near the viewport are in the
// DOM (vwin.first/last), with `tr.vpad` spacer rows above/below preserving the
// full scroll height. The full `display` list still drives the footer count,
// keyboard nav and the absolute row indices handed to onRowClick.
import { test, expect } from "@playwright/test";
import { open, connect, row, renderedNames, selectedNames, manyFiles } from "./harness.js";

const N = 2000;
const REMOTE = manyFiles(N); // f0000.dat … f1999.dat
const FIRST = "f0000.dat";
const LAST = "f1999.dat";

/** Open the app with a 2000-entry remote listing and connect. */
async function openBig(page) {
  await open(page, { remote: REMOTE });
  await connect(page);
  await expect(row(page, "remote", FIRST)).toBeVisible();
}

const rowsBox = (page) => page.locator(".pane[data-kind=remote] .rows");
const metrics = (page) =>
  rowsBox(page).evaluate((el) => ({
    scrollTop: el.scrollTop,
    scrollHeight: el.scrollHeight,
    clientHeight: el.clientHeight,
  }));

/** Set scrollTop on the .rows container and wait for the window to slide. */
async function scrollTo(page, top) {
  await rowsBox(page).evaluate((el, t) => (el.scrollTop = t), top);
  await expect.poll(() => metrics(page).then((m) => m.scrollTop)).toBe(top);
}

test("renders only a small window of rows but counts all 2000", async ({ page }) => {
  await openBig(page);

  const names = await renderedNames(page, "remote");
  expect(names.length).toBeGreaterThan(0);
  expect(names.length).toBeLessThan(150); // virtualized, not 2000
  expect(names[0]).toBe(FIRST); // window starts at the top of the list
  expect(names).not.toContain(LAST);

  // The footer reports the whole listing regardless of what's rendered.
  await expect(page.locator(".pane[data-kind=remote] .foot")).toHaveText(`${N} items`);
});

test("spacer rows keep the scrollbar sized for the full list", async ({ page }) => {
  await openBig(page);

  const m = await metrics(page);
  expect(m.scrollHeight).toBeGreaterThan(30000); // ~2000 * rowHeight
  expect(m.scrollHeight).toBeGreaterThan(m.clientHeight * 10);

  // The height comes from vpad spacers, not from 2000 real rows.
  const vpads = page.locator(".pane[data-kind=remote] tbody tr.vpad");
  expect(await vpads.count()).toBeGreaterThan(0);
});

test("scrolling slides the rendered window", async ({ page }) => {
  await openBig(page);
  const before = await renderedNames(page, "remote");

  await scrollTo(page, 20000);
  await expect.poll(() => renderedNames(page, "remote").then((n) => n[0])).not.toBe(before[0]);

  const after = await renderedNames(page, "remote");
  expect(after.length).toBeLessThan(150);
  expect(after).not.toContain(FIRST); // the top of the list is unmounted
  expect(after).not.toEqual(before);
  // Still contiguous, ascending, and far down the list.
  expect(Number(after[0].slice(1, 5))).toBeGreaterThan(500);
  expect(after.map((n) => Number(n.slice(1, 5)))).toEqual(
    after.map((_, i) => Number(after[0].slice(1, 5)) + i),
  );
});

test("clicking a rendered row selects the correct absolute row", async ({ page }) => {
  await openBig(page);
  await scrollTo(page, 20000);

  const names = await renderedNames(page, "remote");
  const anchor = names[Math.floor(names.length / 2)];
  await row(page, "remote", anchor).click();
  await expect.poll(() => selectedNames(page, "remote")).toEqual([anchor]);
  await expect(row(page, "remote", anchor)).toHaveClass(/\bsel\b/);

  // Shift-click 5 rows further down: the range comes from the ABSOLUTE indices
  // handed to onRowClick (vwin.first + i), so a window-local index would select
  // the wrong slice of the 2000-row list.
  const idx = names.indexOf(anchor);
  const target = names[idx + 5];
  await row(page, "remote", target).click({ modifiers: ["Shift"] });
  await expect.poll(() => selectedNames(page, "remote")).toEqual(names.slice(idx, idx + 6));
});

test("End jumps to the last file and Home returns to the first", async ({ page }) => {
  await openBig(page);
  // Real pointer events so the remote pane takes focus before the key presses.
  await row(page, "remote", FIRST).click();
  await expect.poll(() => selectedNames(page, "remote")).toEqual([FIRST]);

  await page.keyboard.press("End");
  await expect.poll(() => selectedNames(page, "remote")).toEqual([LAST]);
  await expect(row(page, "remote", LAST)).toBeVisible();
  await expect(row(page, "remote", LAST)).toBeInViewport();
  expect(await renderedNames(page, "remote")).not.toContain(FIRST);
  const end = await metrics(page);
  expect(end.scrollTop).toBeGreaterThan(30000); // scrolled to the bottom region

  await page.keyboard.press("Home");
  await expect.poll(() => selectedNames(page, "remote")).toEqual([FIRST]);
  await expect(row(page, "remote", FIRST)).toBeVisible();
  await expect(row(page, "remote", FIRST)).toBeInViewport();
  expect(await renderedNames(page, "remote")).not.toContain(LAST);
  // Back at the top. Note: keepVisible() parks the selected row flush under the
  // sticky header rather than at scrollTop 0, so the ".." row ends up scrolled
  // just out of sight — hence "small", not exactly 0.
  const home = await metrics(page);
  expect(home.scrollTop).toBeLessThan(60);
  expect(home.scrollTop).toBeLessThan(end.scrollTop);
});
