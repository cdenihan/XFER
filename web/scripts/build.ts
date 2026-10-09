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
// Launch after installation: Bun may resolve literal dynamic imports before
// executing this script and otherwise autoinstall Vite+ outside our lockfile.
const build = Bun.spawn(["bun", "--bun", "run", "vp", "build", "--outDir", join(output, "dist")], {
  cwd: root,
  env: { ...process.env, NODE_ENV: "production" },
  stdout: "inherit",
  stderr: "inherit",
});
if ((await build.exited) !== 0) process.exit(1);
await pack(join(output, "dist"), output);
