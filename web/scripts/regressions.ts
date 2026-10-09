import { strict as assert } from "node:assert";
import type { Browser } from "playwright";
import type { State } from "../src/types";

export async function regressions(browser: Browser, url: string) {
  const page = await browser.newPage();
  const state: State = {
    name: "Regression device",
    output: "/tmp/received",
    busy: false,
    phase: "ready",
    message: "",
    bytes: 0,
    total: 0,
    peers: [{ name: "Peer", address: "localhost" }],
    pending: null,
  };
  const calls: string[] = [];
  let offline = false;
  let failNew = true;
  let failUpload = false;
  let nextPending: State["pending"] | undefined;
  await page.route("**/api/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    calls.push(path);
    if (path === "/api/state") {
      if (offline) return route.abort();
      return route.fulfill({ json: state });
    }
    if (path === "/api/new" && failNew)
      return route.fulfill({ status: 400, json: { error: "TransferInProgress" } });
    if (path === "/api/upload" && failUpload)
      return route.fulfill({ status: 400, json: { error: "InvalidUpload" } });
    if (path === "/api/decision") {
      if (nextPending !== undefined) state.pending = nextPending;
      return route.fulfill({ status: 400, json: { error: "StaleApproval" } });
    }
    return route.fulfill({ json: {} });
  });
  try {
    await page.goto(url);
    await page.locator("#identity").filter({ hasText: state.name }).waitFor();
    await page.getByRole("button", { name: /Peer/ }).click();
    await page
      .locator("#files")
      .setInputFiles({ name: "test.txt", mimeType: "text/plain", buffer: Buffer.from("test") });
    await page.locator("#send").click();
    await page.getByRole("alert").filter({ hasText: "Another transfer" }).waitFor();
    assert(!calls.includes("/api/cancel"), "Rejected staging must not cancel incoming work");
    failNew = false;
    failUpload = true;
    await page.locator("#send").click();
    await page.getByRole("alert").filter({ hasText: "invalid path" }).waitFor();
    assert(calls.includes("/api/cancel"), "Owned failed upload must clean up");
    // Large selections render in bounded batches and can be filtered/edited.
    await page.locator("#files").setInputFiles(
      Array.from({ length: 120 }, (_, index) => ({
        name: `item-${index}.txt`,
        mimeType: "text/plain",
        buffer: Buffer.from("item"),
      })),
    );
    assert.equal(await page.locator(".file-queue li").count(), 100);
    await page.getByRole("button", { name: /Show more/ }).click();
    assert.equal(await page.locator(".file-queue li").count(), 120);
    await page.getByRole("textbox", { name: "Filter selected items" }).fill("item-119");
    assert.equal(await page.locator(".file-queue li").count(), 1);
    await page
      .getByRole("button", { name: "Remove Shared items/item-119.txt", exact: true })
      .click();
    await page.locator(".queue-empty").waitFor();
    await page.locator("#clear").click();
    await page
      .locator("#files")
      .setInputFiles({ name: "test.txt", mimeType: "text/plain", buffer: Buffer.from("test") });
    offline = true;
    await page.locator("#online").filter({ hasText: "Reconnecting" }).waitFor();
    assert(await page.locator("#send").isDisabled());
    offline = false;
    await page.locator("#online").filter({ hasText: "Ready to receive" }).waitFor();
    state.pending = { id: 1, code: "123456789abc", name: "test.txt", total: 4, receiving: true };
    await page.locator("#approval[open]").waitFor();
    await page.locator("#match").check();
    await page.locator("#accept").click();
    await page.locator("#approval .error").waitFor();
    assert(
      await page.locator("#accept").isEnabled(),
      "Failed decision permits retry on same request",
    );
    nextPending = { ...state.pending, id: 2, code: "abcdef123456" };
    await page.locator("#accept").click();
    await page.locator("#code").filter({ hasText: "abcd-ef12-3456" }).waitFor();
    assert(!(await page.locator("#match").isChecked()), "A different request resets confirmation");
    assert(await page.locator("#accept").isDisabled());
    await page.locator("#match").check();
    nextPending = null;
    await page.locator("#accept").click();
    await page.locator("#approval").waitFor({ state: "detached" });
    await page.locator("#quit").click();
    await page.locator("#online").filter({ hasText: "XFER is closed" }).waitFor();
    const count = calls.filter((path) => path === "/api/state").length;
    await page.waitForTimeout(1300);
    assert.equal(calls.filter((path) => path === "/api/state").length, count, "Quit stops polling");
    console.log("PASS React ownership, reconnect, decision retry, stale/expired consent and quit");
  } finally {
    await page.close();
  }
}
