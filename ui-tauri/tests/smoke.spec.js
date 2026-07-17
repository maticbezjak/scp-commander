import { test, expect } from "@playwright/test";
import { open, connect, closeLogin, row, renderedNames } from "./harness.js";

test("app renders the local pane and lists files", async ({ page }) => {
  await open(page);
  await closeLogin(page);
  expect(await renderedNames(page, "local")).toEqual(["sub", "a.txt", "b.txt"]);
  await expect(page.locator(".pane[data-kind=local] .foot")).toContainText("3 items");
});

test("connecting opens the remote pane at the remote path", async ({ page }) => {
  await open(page);
  await connect(page);
  await expect(page.locator(".pane[data-kind=remote] .pathbar")).toHaveValue("/data");
  expect(await renderedNames(page, "remote")).toEqual(["drop", "config.xml"]);
  await expect(row(page, "remote", "config.xml")).toBeVisible();
});
