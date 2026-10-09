import { regressions } from "./regressions";
import { chromium } from "playwright";
import { createServer } from "node:net";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import assert from "node:assert/strict";

const binary = resolve(process.argv[2] ?? "../zig-out/bin/xfer");
const root = await mkdtemp(join(tmpdir(), "xfer-browser-"));
const children: ReturnType<typeof Bun.spawn>[] = [];
async function launch(name: string) {
  const reservation = createServer();
  await new Promise<void>((done) => reservation.listen(0, "127.0.0.1", done));
  const address = reservation.address();
  assert(address && typeof address !== "string");
  const port = address.port;
  await new Promise<void>((done) => reservation.close(() => done()));
  const child = Bun.spawn(
    [
      binary,
      "--no-open",
      "--json",
      "--port",
      String(port),
      "--bind",
      "127.0.0.1",
      "--name",
      name,
      "--output",
      join(root, name),
    ],
    { stdout: "pipe", stderr: "inherit" },
  );
  children.push(child);
  const reader = child.stdout.getReader();
  let text = "";
  const decoder = new TextDecoder();
  const timer = setTimeout(() => child.kill(), 10000);
  try {
    while (!text.includes("\n")) {
      const result = await reader.read();
      assert(!result.done, "Engine exited before startup");
      text += decoder.decode(result.value);
    }
    const event = JSON.parse(text.split("\n")[0]!);
    assert.equal(event.event, "desktop");
    return { url: event.message as string, port };
  } finally {
    clearTimeout(timer);
    reader.releaseLock();
  }
}

const browser = await chromium.launch({
  executablePath: process.env.XFER_CHROMIUM,
  args: ["--no-sandbox"],
});
try {
  const sender = await launch("Sender");
  const receiver = await launch("Receiver");
  await regressions(browser, sender.url);
  console.log("Starting real transfer browser checks");
  const left = await browser.newPage();
  const right = await browser.newPage();
  left.setDefaultTimeout(10000);
  right.setDefaultTimeout(10000);
  const errors: string[] = [];
  for (const page of [left, right]) {
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
      if (message.type() === "error") errors.push(message.text());
    });
  }
  await Promise.all([left.goto(sender.url), right.goto(receiver.url)]);
  console.log("Both native pages loaded");
  await left.locator("#identity").filter({ hasText: "Sender" }).waitFor();
  assert.equal(new URL(left.url()).hash, "", "Capability must leave the address bar");
  if ((await left.locator("details").getAttribute("open")) === null)
    await left.locator("summary").click();
  await left.locator("#address").fill(`127.0.0.1:${receiver.port}`);
  await left.locator("#connect").click();
  const content = Buffer.from("Encrypted from the real TypeScript browser UI\n");
  await left
    .locator("#files")
    .setInputFiles({ name: "browser.txt", mimeType: "text/plain", buffer: content });
  // Route changes preserve selection and destination. Native route reloads serve the SPA.
  await left.getByRole("link", { name: "Receive", exact: true }).click();
  await left.reload();
  await left.locator("#destination").waitFor();
  await left.getByRole("link", { name: "Share files", exact: false }).click();
  await left
    .locator("#files")
    .setInputFiles({ name: "browser.txt", mimeType: "text/plain", buffer: content });
  if ((await left.locator("details").getAttribute("open")) === null)
    await left.locator("summary").click();
  await left.locator("#address").fill(`127.0.0.1:${receiver.port}`);
  await left.locator("#connect").click();
  await left.getByRole("link", { name: "Receive", exact: true }).click();
  await left.getByRole("link", { name: "Share files", exact: false }).click();
  assert.equal(await left.locator("#selection").textContent(), "browser.txt");
  await left.setViewportSize({ width: 1280, height: 1000 });
  await left.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
  await left.screenshot({ path: "/tmp/xfer-selection.png", fullPage: true });
  console.log("Selection survived navigation; starting send");
  await left.locator("#send").click();
  await Promise.all([
    left.locator("#approval[open]").waitFor(),
    right.locator("#approval[open]").waitFor(),
  ]);
  assert.equal(
    await left.locator("#code").textContent(),
    await right.locator("#code").textContent(),
  );
  assert(
    await left.locator("#accept").isDisabled(),
    "Approval requires explicit code confirmation",
  );
  await Promise.all([left.locator("#match").check(), right.locator("#match").check()]);
  await Promise.all([left.locator("#accept").click(), right.locator("#accept").click()]);
  console.log("Both approvals sent");
  await right.locator("#status-text").filter({ hasText: "Saved" }).waitFor();
  assert.deepEqual(
    await Bun.file(join(root, "Receiver", "browser.txt")).bytes(),
    new Uint8Array(content),
  );
  // Reload uses the same-origin session capability and restores live state.
  await left.reload();
  await left.locator("#identity").filter({ hasText: "Sender" }).waitFor();
  assert.equal(await left.locator("#online").textContent(), "Ready to receive");
  await left.setViewportSize({ width: 1280, height: 1000 });
  await left.screenshot({ path: "/tmp/xfer-desktop.png", fullPage: true });
  await left.setViewportSize({ width: 390, height: 844 });
  await left.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
  assert.equal(await left.locator(".app-shell").count(), 1);
  assert(
    await left.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
    "Mobile layout must not overflow",
  );
  await left.screenshot({
    path: process.env.XFER_SCREENSHOT ?? "/tmp/xfer-share.png",
    fullPage: true,
  });
  await left.getByRole("link", { name: "Receive", exact: true }).click();
  assert(
    await left.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
    "Receive layout must not overflow",
  );
  await left.setViewportSize({ width: 1280, height: 1000 });
  await left.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
  await left.screenshot({ path: "/tmp/xfer-receive.png", fullPage: true });
  assert.deepEqual(errors, [], "No browser exceptions or CSP violations");
  if (process.env.XFER_TEST_TAILCAT === "1") {
    await right.getByRole("link", { name: "Receive", exact: true }).click();
    await right.locator("#remote-toggle").click();
    await right.locator("#your-invite").waitFor();
    const invite = await right.locator("#your-invite").inputValue();
    assert(invite.startsWith("xfer-tailcat:"));
    await left.getByRole("link", { name: "Share files", exact: false }).click();
    await left.getByRole("button", { name: /Across a distance/ }).click();
    await left.locator("#remote-invite").fill(invite);
    await left.locator("#remote-connect").click();
    await left
      .locator("#files")
      .setInputFiles({ name: "remote-browser.txt", mimeType: "text/plain", buffer: content });
    await left.screenshot({ path: "/tmp/xfer-remote.png", fullPage: true });
    await left.locator("#send").click();
    await Promise.all([
      left.locator("#approval[open]").waitFor(),
      right.locator("#approval[open]").waitFor(),
    ]);
    assert.equal(
      await left.locator("#code").textContent(),
      await right.locator("#code").textContent(),
    );
    await Promise.all([left.locator("#match").check(), right.locator("#match").check()]);
    await Promise.all([left.locator("#accept").click(), right.locator("#accept").click()]);
    await right.locator("#status-text").filter({ hasText: "Saved remote-browser.txt" }).waitFor();
    assert.deepEqual(
      await Bun.file(join(root, "Receiver", "remote-browser.txt")).bytes(),
      new Uint8Array(content),
    );
    await right.locator("#remote-toggle").click();
    await right.locator("#your-invite").waitFor({ state: "detached" });
    console.log(
      "PASS real browser remote invitation, Tailcat tunnel, consent and verified delivery",
    );
  }
  await Promise.all([left.locator("#quit").click(), right.locator("#quit").click()]);
  for (const child of children) assert.equal(await child.exited, 0);
  console.log(
    "PASS real Chromium selection, TanStack polling, consent, encrypted delivery, reload and quit",
  );
} finally {
  await browser.close();
  for (const child of children) if (child.exitCode === null) child.kill();
  await rm(root, { recursive: true, force: true });
}
