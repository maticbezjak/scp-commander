import { test, expect } from "@playwright/test";
import { open, closeLogin } from "./harness.js";

// app.css drives every surface through light-dark() tokens; App.svelte forces the
// resolution by writing document.documentElement.style.colorScheme. These tests
// pin the token values so a palette edit can't silently repaint the app.
const LIGHT_BG = "rgb(238, 240, 243)"; // --bg  light  #eef0f3
const LIGHT_PANEL = "rgb(255, 255, 255)"; // --panel light #ffffff
const DARK_PANEL = "rgb(27, 31, 39)"; // --panel dark  #1b1f27
const FOLDER_GOLD = "rgb(227, 160, 8)"; // --folder light #e3a008

const rootColorScheme = (page) => page.evaluate(() => document.documentElement.style.colorScheme);
const bgOf = (locator) => locator.evaluate((el) => getComputedStyle(el).backgroundColor);

test('theme "light" forces the light palette even when the OS is dark', async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await open(page, { theme: "light" });
  await closeLogin(page);

  expect(await rootColorScheme(page)).toBe("light");
  expect(await bgOf(page.locator("body"))).toBe(LIGHT_BG);
  expect(await bgOf(page.locator(".pane[data-kind=local]"))).toBe(LIGHT_PANEL);
});

test('theme "dark" forces the dark palette even when the OS is light', async ({ page }) => {
  await page.emulateMedia({ colorScheme: "light" });
  await open(page, { theme: "dark" });
  await closeLogin(page);

  expect(await rootColorScheme(page)).toBe("dark");
  const paneBg = await bgOf(page.locator(".pane[data-kind=local]"));
  expect(paneBg).toBe(DARK_PANEL);
  expect(paneBg).not.toBe(LIGHT_PANEL);
});

test("folder icons render in the WinSCP-style gold in light theme", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await open(page, { theme: "light" });
  await closeLogin(page);

  // The default local listing has one directory ("sub"); its glyph is filled
  // with currentColor, which .ti.dir sets from --folder.
  const icon = page.locator('.pane[data-kind=local] tbody tr[data-name="sub"] svg.ti.dir');
  await expect(icon).toBeVisible();
  expect(await icon.evaluate((el) => getComputedStyle(el).color)).toBe(FOLDER_GOLD);

  // Regular files must not pick up the folder colour.
  const fileIcon = page.locator('.pane[data-kind=local] tbody tr[data-name="a.txt"] svg.ti.file');
  expect(await fileIcon.evaluate((el) => getComputedStyle(el).color)).not.toBe(FOLDER_GOLD);
});

test("Preferences → Theme applies live and persists to localStorage", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "light" });
  await open(page); // no stored theme → "system"
  await closeLogin(page);
  expect(await rootColorScheme(page)).toBe("light dark");

  await page.getByRole("button", { name: "Preferences", exact: true }).click();
  const select = page.locator(".prefs select");
  await expect(select).toHaveValue("system");

  await select.selectOption("dark");

  // Applied live — no Save needed, the dialog is still open.
  await expect.poll(() => rootColorScheme(page)).toBe("dark");
  expect(await bgOf(page.locator(".pane[data-kind=local]"))).toBe(DARK_PANEL);
  expect(await page.evaluate(() => localStorage.getItem("theme"))).toBe("dark");

  // Cancelling the dialog does not revert the theme.
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  expect(await rootColorScheme(page)).toBe("dark");
});
