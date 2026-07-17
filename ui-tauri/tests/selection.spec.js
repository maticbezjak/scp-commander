import { test, expect } from "@playwright/test";
import { open, closeLogin, clickRow, renderedNames, selectedNames, calls, FILE } from "./harness.js";

// Five plain files so the visible order is a stable a…e (dirs sort first, then
// name ascending — no dirs here, so display index == alphabetical index).
const FILES = ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"].map((n) => FILE(n));

const foot = (page) => page.locator(".pane[data-kind=local] .foot");
const menu = (page) => page.locator(".ctx");
const deleteItem = (page) => page.locator(".ctx button.danger");

/** Open the app with FILES in the local pane, no connection. */
async function openLocal(page) {
  await open(page, { local: FILES });
  await closeLogin(page);
  expect(await renderedNames(page, "local")).toEqual(["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]);
}

test("plain click selects one row; clicking another replaces the selection", async ({ page }) => {
  await openLocal(page);

  await clickRow(page, "local", "b.txt");
  expect(await selectedNames(page, "local")).toEqual(["b.txt"]);
  await expect(foot(page)).toContainText("1 of 5 selected");

  await clickRow(page, "local", "d.txt");
  expect(await selectedNames(page, "local")).toEqual(["d.txt"]);
  await expect(foot(page)).toContainText("1 of 5 selected");
});

test("⌘-click adds and toggles rows; the footer counts the selection", async ({ page }) => {
  await openLocal(page);

  await clickRow(page, "local", "a.txt");
  await clickRow(page, "local", "c.txt", { modifiers: ["Meta"] });
  await clickRow(page, "local", "e.txt", { modifiers: ["Meta"] });
  expect(await selectedNames(page, "local")).toEqual(["a.txt", "c.txt", "e.txt"]);
  await expect(foot(page)).toContainText("3 of 5 selected");

  // ⌘-clicking a selected row removes it again.
  await clickRow(page, "local", "c.txt", { modifiers: ["Meta"] });
  expect(await selectedNames(page, "local")).toEqual(["a.txt", "e.txt"]);
  await expect(foot(page)).toContainText("2 of 5 selected");
});

test("shift-click selects a contiguous range from the anchor", async ({ page }) => {
  await openLocal(page);

  await clickRow(page, "local", "b.txt"); // anchor
  await clickRow(page, "local", "d.txt", { modifiers: ["Shift"] });
  expect(await selectedNames(page, "local")).toEqual(["b.txt", "c.txt", "d.txt"]);
  await expect(foot(page)).toContainText("3 of 5 selected");

  // Shifting back above the anchor re-anchors nothing — the range still spans
  // anchor..clicked, so it grows upward instead.
  await clickRow(page, "local", "a.txt", { modifiers: ["Shift"] });
  expect(await selectedNames(page, "local")).toEqual(["a.txt", "b.txt"]);
  await expect(foot(page)).toContainText("2 of 5 selected");
});

test("⌘A selects all and ⌘I inverts the selection in the focused pane", async ({ page }) => {
  await openLocal(page);

  // A real pointer click is what focuses the pane (pointerdown -> onFocus).
  await clickRow(page, "local", "a.txt");
  await page.keyboard.press("Meta+a");
  expect(await selectedNames(page, "local")).toEqual(["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]);
  await expect(foot(page)).toContainText("5 of 5 selected");

  // Invert a full selection -> nothing selected (footer falls back to the count).
  await page.keyboard.press("Meta+i");
  expect(await selectedNames(page, "local")).toEqual([]);
  await expect(foot(page)).toHaveText("5 items");

  // Invert a partial selection -> exactly the complement.
  await clickRow(page, "local", "a.txt");
  await clickRow(page, "local", "b.txt", { modifiers: ["Meta"] });
  await page.keyboard.press("Meta+i");
  expect(await selectedNames(page, "local")).toEqual(["c.txt", "d.txt", "e.txt"]);
  await expect(foot(page)).toContainText("3 of 5 selected");
});

test("right-click inside a multi-selection keeps it; right-click outside collapses it", async ({ page }) => {
  await openLocal(page);

  await clickRow(page, "local", "a.txt");
  await clickRow(page, "local", "b.txt", { modifiers: ["Meta"] });
  await clickRow(page, "local", "c.txt", { modifiers: ["Meta"] });
  expect(await selectedNames(page, "local")).toEqual(["a.txt", "b.txt", "c.txt"]);

  // REGRESSION: right-clicking a row that is already part of the selection must
  // not collapse it — the menu acts on all three.
  await clickRow(page, "local", "b.txt", { button: "right" });
  await expect(menu(page)).toBeVisible();
  expect(await selectedNames(page, "local")).toEqual(["a.txt", "b.txt", "c.txt"]);
  await expect(foot(page)).toContainText("3 of 5 selected");
  await expect(deleteItem(page)).toHaveText("Delete (3)…");

  await page.keyboard.press("Escape");
  await expect(menu(page)).toHaveCount(0);

  // Right-clicking a row outside the selection collapses to just that row.
  await clickRow(page, "local", "e.txt", { button: "right" });
  await expect(menu(page)).toBeVisible();
  expect(await selectedNames(page, "local")).toEqual(["e.txt"]);
  await expect(foot(page)).toContainText("1 of 5 selected");
  await expect(deleteItem(page)).toHaveText("Delete…");
});

test("REGRESSION: ctrl-right-click outside the selection replaces it, never adds", async ({ page }) => {
  await openLocal(page);

  await clickRow(page, "local", "a.txt");
  await clickRow(page, "local", "b.txt", { modifiers: ["Meta"] });
  expect(await selectedNames(page, "local")).toEqual(["a.txt", "b.txt"]);

  // ctrl-click IS the standard macOS right-click gesture, so a right-click can
  // legitimately arrive with ctrlKey set. It must behave like any other
  // right-click (collapse to the row), not take rowClick's meta/ctrl "add" path.
  await clickRow(page, "local", "d.txt", { button: "right", modifiers: ["Control"] });
  await expect(menu(page)).toBeVisible();
  expect(await selectedNames(page, "local")).toEqual(["d.txt"]);
  await expect(deleteItem(page)).toHaveText("Delete…");
});

test("REGRESSION: a filter hides rows AND drops them as operation targets", async ({ page }) => {
  await openLocal(page);

  // Select everything, then filter down to a single visible row.
  await clickRow(page, "local", "a.txt");
  await page.keyboard.press("Meta+a");
  await expect(foot(page)).toContainText("5 of 5 selected");

  await page.locator(".pane[data-kind=local] .filter").fill("c.txt");
  expect(await renderedNames(page, "local")).toEqual(["c.txt"]);

  // The footer must not claim "5 of 1 selected" — only visible rows count…
  await expect(foot(page)).toContainText("1 of 1 selected");

  // …and, critically, Delete must act ONLY on the visible row. Previously the
  // 4 filtered-out files were still live targets — deleting what you can't see.
  await page.locator('.pane[data-kind=local] .tb[title="Delete"]').click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("c.txt"); // names the one visible file, not "5 items"
  await dialog.getByRole("button", { name: "Delete", exact: true }).click();

  await expect.poll(() => calls(page, "local_delete")).toHaveLength(1);
  expect((await calls(page, "local_delete"))[0].path).toContain("c.txt");
});

test("REGRESSION: ⌘I re-anchors, so a following shift-click extends from the new selection", async ({ page }) => {
  await openLocal(page);

  await clickRow(page, "local", "a.txt");
  await clickRow(page, "local", "b.txt", { modifiers: ["Meta"] });
  await page.keyboard.press("Meta+i"); // -> c, d, e selected; anchor must follow
  expect(await selectedNames(page, "local")).toEqual(["c.txt", "d.txt", "e.txt"]);

  // The anchor is now the last inverted row (e.txt), so shift-clicking c.txt
  // spans c..e. With the stale anchor (b.txt) this selected b..c instead.
  await clickRow(page, "local", "c.txt", { modifiers: ["Shift"] });
  expect(await selectedNames(page, "local")).toEqual(["c.txt", "d.txt", "e.txt"]);
});
