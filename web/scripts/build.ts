import { resolve, join } from "node:path";
import { pack } from "./embed";
const root = resolve(import.meta.dir, "..");
const output = resolve(process.argv[2]);
const install = Bun.spawn(["bun", "install", "--frozen-lockfile"], {
  cwd: root,
  stdout: "inherit",
  stderr: "inherit",
});
if ((await install.exited) !== 0) process.exit(1);
process.env.NODE_ENV = "production";
const { build } = await import("vite-plus");
await build({
  configFile: join(root, "vite.config.ts"),
  root,
  build: { outDir: join(output, "dist"), emptyOutDir: true },
});
await pack(join(output, "dist"), output);
