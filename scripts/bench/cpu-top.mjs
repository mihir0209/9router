#!/usr/bin/env node
// Summarise a V8 .cpuprofile by self time: top functions and top source files.
//   node scripts/bench/cpu-top.mjs <file.cpuprofile> [topN]
import fs from "node:fs";

const [file, topArg] = process.argv.slice(2);
const top = Number(topArg || 25);
const prof = JSON.parse(fs.readFileSync(file, "utf8"));
const byId = new Map(prof.nodes.map((n) => [n.id, n]));
const self = new Map();
for (let i = 0; i < prof.samples.length; i++) {
  self.set(prof.samples[i], (self.get(prof.samples[i]) || 0) + (prof.timeDeltas[i] || 0));
}
const total = [...self.values()].reduce((a, b) => a + b, 0);
const fns = new Map();
const files = new Map();
for (const [id, us] of self) {
  const { functionName, url, lineNumber } = byId.get(id).callFrame;
  const shortUrl = url.replace(/^file:\/\//, "").replace(/.*\/(node_modules\/)/, "$1").replace(/.*\/\.next\//, ".next/");
  const fkey = `${functionName || "(anon)"} ${shortUrl}:${lineNumber + 1}`;
  fns.set(fkey, (fns.get(fkey) || 0) + us);
  const ukey = shortUrl || "(native/idle)";
  files.set(ukey, (files.get(ukey) || 0) + us);
}
const fmt = (m) => [...m].sort((a, b) => b[1] - a[1]).slice(0, top).map(([k, us]) => `${((us / total) * 100).toFixed(1).padStart(5)}%  ${(us / 1000).toFixed(0).padStart(7)} ms  ${k}`).join("\n");
console.log(`total sampled self time ${(total / 1e6).toFixed(2)} s\n\nTop functions by self time\n${fmt(fns)}\n\nTop files by self time\n${fmt(files)}`);
