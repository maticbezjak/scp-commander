import { test, expect } from "@playwright/test";
import { open, connect, closeLogin, clickRow, selectedNames } from "./harness.js";

// The WinSCP/Norton-style bottom function-key bar (App.svelte .fnbar).
// Enabled states are derived from the ACTIVE pane (focusLocal), so every test
// that cares about focus uses clickRow() for real pointer events.

/** The .fk button carrying a given key hint ("F2" … "Del"). */
const fk = (page, key) =>
  page.locator(".fnbar .fk").filter({ has: page.locator(`kbd:text-is("${key}")`) });

test("renders all seven function keys in order with their hints", async ({ page }) => {
  await open(page);
  await closeLogin(page);

  const keys = page.locator(".fnbar .fk");
  await expect(keys).toHaveCount(7);

  const bar = await keys.evaluateAll((els) =>
    els.map((el) => {
      const kbd = el.querySelector("kbd");
      return { key: kbd.textContent.trim(), label: el.textContent.slice(kbd.textContent.length).trim() };
    }),
  );
  expect(bar).toEqual([
    { key: "F2", label: "Rename" },
    { key: "F3", label: "View" },
    { key: "F4", label: "Edit" },
    { key: "F5", label: "Copy" },
    { key: "F6", label: "Move" },
    { key: "F7", label: "New folder" },
    { key: "Del", label: "Delete" },
  ]);
});

test("with no selection only F7 New folder is enabled", async ({ page }) => {
  await open(page);
  await closeLogin(page);
  expect(await selectedNames(page, "local")).toEqual([]);

  for (const key of ["F2", "F3", "F4", "F5", "F6", "Del"]) {
    await expect(fk(page, key), `${key} should be disabled`).toBeDisabled();
  }
  await expect(fk(page, "F7")).toBeEnabled();
});

test("selecting a remote file enables rename/view/edit/copy/move/delete", async ({ page }) => {
  await open(page);
  await connect(page);
  await clickRow(page, "remote", "config.xml");
  expect(await selectedNames(page, "remote")).toEqual(["config.xml"]);

  for (const key of ["F2", "F3", "F5", "F6", "F7", "Del"]) {
    await expect(fk(page, key), `${key} should be enabled`).toBeEnabled();
  }
  // Edit is remote-only, and the remote pane is the focused one here.
  await expect(fk(page, "F4")).toBeEnabled();
});

test("F4 Edit stays disabled for a local file even when connected", async ({ page }) => {
  await open(page);
  await connect(page);
  await clickRow(page, "local", "a.txt");
  expect(await selectedNames(page, "local")).toEqual(["a.txt"]);

  // Everything else lights up for a local file…
  await expect(fk(page, "F2")).toBeEnabled();
  await expect(fk(page, "F3")).toBeEnabled();
  await expect(fk(page, "F5")).toBeEnabled();
  // …but Edit only ever applies to the remote pane.
  await expect(fk(page, "F4")).toBeDisabled();
});

test("F7 opens the New folder dialog", async ({ page }) => {
  await open(page);
  await closeLogin(page);
  await fk(page, "F7").click();

  const dialog = page.locator(".modal");
  await expect(dialog.locator(".mtitle")).toHaveText("New folder");
  const input = dialog.locator("input.dlg-input");
  await expect(input).toBeVisible();
  await expect(input).toHaveValue("");
  await expect(input).toHaveAttribute("placeholder", "folder name");
});

test("F2 opens the Rename dialog prefilled with the selected name", async ({ page }) => {
  await open(page);
  await closeLogin(page);
  await clickRow(page, "local", "b.txt");
  await fk(page, "F2").click();

  const dialog = page.locator(".modal");
  await expect(dialog.locator(".mtitle")).toHaveText("Rename");
  await expect(dialog.locator("input.dlg-input")).toHaveValue("b.txt");
});
