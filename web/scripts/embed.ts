import { readdir, mkdir } from "node:fs/promises";
import { join, relative, resolve } from "node:path";
import { createHash } from "node:crypto";
import { gzipSync } from "node:zlib";

const types: Record<string, string> = {
  html: "text/html; charset=utf-8",
  js: "text/javascript; charset=utf-8",
  css: "text/css; charset=utf-8",
  svg: "image/svg+xml",
  png: "image/png",
  jpg: "image/jpeg",
  webp: "image/webp",
  ico: "image/x-icon",
  woff2: "font/woff2",
};
export async function pack(root: string, output: string) {
  await mkdir(output, { recursive: true });
  const files = (await readdir(root, { recursive: true, withFileTypes: true }))
    .filter((entry) => entry.isFile())
    .map((entry) => relative(root, join(entry.parentPath, entry.name)).replaceAll("\\", "/"))
    .sort();
  if (!files.includes("index.html")) throw new Error("Missing frontend index.html");
  const entries: string[] = [];
  const chunks: Uint8Array[] = [];
  const report: { path: string; raw: number; stored: number; gzip: boolean }[] = [];
  let offset = 0;
  for (const path of files) {
    if (!/^[a-zA-Z0-9_./-]+$/.test(path) || path.endsWith(".map"))
      throw new Error(`Unsupported production asset: ${path}`);
    const raw = await Bun.file(join(root, path)).bytes();
    const zipped = gzipSync(raw, { level: 9 });
    const compressed = zipped.length + 32 < raw.length;
    const body = compressed ? zipped : raw;
    const etag = 'W/"' + createHash("sha256").update(raw).digest("hex") + '"';
    const type = types[path.split(".").pop()!] ?? "application/octet-stream";
    entries.push(
      `.{ .path = ${JSON.stringify(path === "index.html" ? "/" : "/" + path)}, .body = blob[${offset}..${offset + body.length}], .raw_size = ${raw.length}, .gzip = ${compressed}, .content_type = ${JSON.stringify(type)}, .etag = ${JSON.stringify(etag)}, .immutable = ${path.startsWith("assets/")} }`,
    );
    chunks.push(body);
    offset += body.length;
    report.push({ path, raw: raw.length, stored: body.length, gzip: compressed });
  }
  await Bun.write(join(output, "assets.bin"), Buffer.concat(chunks));
  await Bun.write(
    join(output, "assets.zig"),
    `// Generated asset index. Payload is binary, not Zig source literals.\nconst std = @import("std");\nconst blob = @embedFile("assets.bin");\npub const Asset = struct { path: []const u8, body: []const u8, raw_size: usize, gzip: bool, content_type: []const u8, etag: []const u8, immutable: bool };\nconst assets = [_]Asset{\n${entries.join(",\n")}\n};\npub fn get(path: []const u8) ?Asset { for (assets) |asset| { if (std.mem.eql(u8, path, asset.path)) return asset; } return null; }\n`,
  );
  await Bun.write(
    join(output, "stats.json"),
    JSON.stringify(
      { raw: report.reduce((sum, entry) => sum + entry.raw, 0), stored: offset, assets: report },
      null,
      2,
    ) + "\n",
  );
  console.log(
    `Embedded UI: ${report.length} assets, ${offset} stored bytes (${report.reduce((sum, entry) => sum + entry.raw, 0)} uncompressed)`,
  );
}
if (import.meta.main)
  await pack(
    resolve(import.meta.dir, "../dist"),
    resolve(process.argv[2] ?? resolve(import.meta.dir, "../.generated/ui")),
  );
