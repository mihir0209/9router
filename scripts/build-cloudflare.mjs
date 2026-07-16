import { renameSync, existsSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";

const root = process.cwd();
const proxyPath = join(root, "src", "proxy.js");
const disabledProxyPath = join(root, "src", "proxy.js.cloudflare-disabled");
const driverPath = join(root, "src", "lib", "db", "driver.js");
const cloudflareDriverPath = join(root, "src", "lib", "db", "driver.cloudflare.js");
const disabledDriverPath = join(root, "src", "lib", "db", "driver.js.cloudflare-disabled");

let moved = false;
let swappedDriver = false;

function restoreFiles() {
  if (moved && existsSync(disabledProxyPath)) {
    renameSync(disabledProxyPath, proxyPath);
    moved = false;
  }
  if (swappedDriver && existsSync(disabledDriverPath)) {
    if (existsSync(driverPath)) renameSync(driverPath, cloudflareDriverPath);
    renameSync(disabledDriverPath, driverPath);
    swappedDriver = false;
  }
}

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.once(signal, () => {
    try { restoreFiles(); } catch { /* best effort */ }
    process.kill(process.pid, signal);
  });
}

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: root,
    stdio: "inherit",
    shell: process.platform === "win32",
    env: {
      ...process.env,
      NINEROUTER_RUNTIME: "cloudflare",
    },
  });
  if (result.status !== 0) throw new Error(`${command} ${args.join(" ")} failed with status ${result.status || 1}`);
}

try {
  if (existsSync(proxyPath)) {
    if (existsSync(disabledProxyPath)) {
      throw new Error(`${disabledProxyPath} already exists; refusing to overwrite`);
    }
    renameSync(proxyPath, disabledProxyPath);
    moved = true;
  }

  if (existsSync(disabledDriverPath)) {
    throw new Error(`${disabledDriverPath} already exists; refusing to overwrite`);
  }
  renameSync(driverPath, disabledDriverPath);
  renameSync(cloudflareDriverPath, driverPath);
  swappedDriver = true;

  run("npx", ["opennextjs-cloudflare", "build"]);
  process.exitCode = 0;
} catch (error) {
  console.error(error.message || error);
  process.exitCode = 1;
} finally {
  restoreFiles();
}
