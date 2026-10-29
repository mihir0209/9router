// Restores the source files that scripts/build-cloudflare.mjs swaps while it builds.
//
// The build renames src/proxy.js and src/lib/db/driver.js out of the way and moves the Cloudflare
// variants into place. A process that is killed (SIGKILL, a timeout, a lost shell) cannot run the
// restore in its `finally`, and the next build then failed with "refusing to overwrite" while the
// working tree was left half swapped. recoverStaleSwaps() undoes such a leftover swap at the start
// of the next run, and only when the state is unambiguous.
import { existsSync, renameSync } from "node:fs";
import { join } from "node:path";

/** The two swapped pairs: where the file normally lives, its Cloudflare variant, and the parked original. */
export function swapPairs(root) {
  return [
    {
      name: "proxy",
      live: join(root, "src", "proxy.js"),
      variant: join(root, "src", "proxy.cloudflare.js"),
      active: join(root, "src", "middleware.js"),
      parked: join(root, "src", "proxy.js.cloudflare-disabled"),
    },
    {
      name: "driver",
      live: join(root, "src", "lib", "db", "driver.js"),
      variant: join(root, "src", "lib", "db", "driver.cloudflare.js"),
      active: join(root, "src", "lib", "db", "driver.js"),
      parked: join(root, "src", "lib", "db", "driver.js.cloudflare-disabled"),
    },
  ];
}

/**
 * Undo a swap left behind by a killed build.
 * Returns the list of pairs that were restored. Throws when the leftovers are ambiguous
 * (the parked original AND a regular file both exist at the live path of the proxy), because
 * guessing could overwrite real work.
 */
export function recoverStaleSwaps(root, log = () => {}) {
  const restored = [];
  const [proxy, driver] = swapPairs(root);

  // proxy: parked original exists => the swap was applied (fully or partly).
  if (existsSync(proxy.parked)) {
    if (existsSync(proxy.live)) {
      throw new Error(`${proxy.parked} and ${proxy.live} both exist; resolve by hand (git status, git checkout -- src/proxy.js)`);
    }
    if (existsSync(proxy.active)) {
      if (existsSync(proxy.variant)) {
        throw new Error(`${proxy.active} and ${proxy.variant} both exist; resolve by hand`);
      }
      renameSync(proxy.active, proxy.variant); // middleware.js -> proxy.cloudflare.js
    }
    renameSync(proxy.parked, proxy.live);
    restored.push("proxy");
    log("restored src/proxy.js from a previous interrupted build");
  }

  // driver: here the Cloudflare variant lives AT the live path while swapped, so the live file is
  // the variant; the parked file is the real driver.
  if (existsSync(driver.parked)) {
    if (existsSync(driver.live)) {
      if (existsSync(driver.variant)) {
        throw new Error(`${driver.live} and ${driver.variant} both exist next to ${driver.parked}; resolve by hand`);
      }
      renameSync(driver.live, driver.variant); // swapped-in copy goes back to driver.cloudflare.js
    }
    renameSync(driver.parked, driver.live);
    restored.push("driver");
    log("restored src/lib/db/driver.js from a previous interrupted build");
  }

  return restored;
}
