import { resolve } from "node:path";

// One command owns the native backend and Vite+ dev server. The proxy stays
// loopback-only; the capability never enters a build artifact or an env file.
const root = resolve(import.meta.dir, "../..");
const build = Bun.spawn(["zig", "build", "-Doptimize=ReleaseSafe"], {
  cwd: root,
  stdout: "inherit",
  stderr: "inherit",
});
if ((await build.exited) !== 0) process.exit(1);
const engine = Bun.spawn(
  [
    resolve(root, "zig-out/bin", process.platform === "win32" ? "xfer.exe" : "xfer"),
    "--no-open",
    "--json",
  ],
  { cwd: root, stdout: "pipe", stderr: "inherit" },
);
let frontend: ReturnType<typeof Bun.spawn> | undefined;
let stopping = false;
function stop() {
  if (stopping) return;
  stopping = true;
  frontend?.kill();
  engine.kill();
}
process.on("SIGINT", stop);
process.on("SIGTERM", stop);
const reader = engine.stdout.getReader();
const decoder = new TextDecoder();
let startup = "";
const timeout = setTimeout(stop, 15000);
try {
  while (!startup.includes("\n")) {
    const chunk = await reader.read();
    if (chunk.done) throw new Error("XFER exited before starting its API");
    startup += decoder.decode(chunk.value);
  }
  const event = JSON.parse(startup.split("\n")[0]);
  if (event.event !== "desktop") throw new Error("Unexpected XFER startup event");
  const url = new URL(event.message);
  frontend = Bun.spawn(["bun", "--bun", "run", "vp", "dev", "--port", "5173", "--strictPort"], {
    cwd: resolve(root, "web"),
    env: { ...process.env, XFER_DEV_URL: url.origin },
    stdout: "inherit",
    stderr: "inherit",
  });
  console.log(`\nOpen the UI: http://127.0.0.1:5173/${url.hash}\n`);
  clearTimeout(timeout);
  // Drain diagnostic events so the native stdout pipe cannot fill up.
  void (async () => {
    while (!(await reader.read()).done) {
      /* Drain. */
    }
  })();
  await Promise.race([engine.exited, frontend.exited]);
} finally {
  clearTimeout(timeout);
  stop();
  await Promise.all([engine.exited, frontend?.exited]);
}
