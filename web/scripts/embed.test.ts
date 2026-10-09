import { it, expect } from "vite-plus/test";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { gunzipSync } from "node:zlib";
import { pack } from "./embed";
it("packs deterministic compressed assets without source literals or duplicate raw payloads", async () => {
  const temp = await mkdtemp(join(tmpdir(), "xfer-pack-"));
  try {
    const root = join(temp, "dist");
    await mkdir(join(root, "assets"), { recursive: true });
    const script = 'console.log("portable assets");\n'.repeat(300);
    await writeFile(join(root, "index.html"), "<div>index</div>");
    await writeFile(join(root, "assets", "app-hash.js"), script);
    const first = join(temp, "a");
    const second = join(temp, "b");
    await pack(root, first);
    await pack(root, second);
    const blob = await readFile(join(first, "assets.bin"));
    expect(blob).toEqual(await readFile(join(second, "assets.bin")));
    expect(await readFile(join(first, "assets.zig"), "utf8")).toContain('@embedFile("assets.bin")');
    const stats = JSON.parse(await readFile(join(first, "stats.json"), "utf8"));
    expect(stats.stored).toBeLessThan(stats.raw / 10);
    const entry = stats.assets[0];
    expect(entry.path).toBe("assets/app-hash.js");
    expect(gunzipSync(blob.subarray(0, entry.stored)).toString()).toBe(script);
  } finally {
    await rm(temp, { recursive: true, force: true });
  }
});
