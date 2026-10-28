#!/usr/bin/env node
// Minimal L4 round-robin balancer for the multi-process scaling benchmark.
//
//   node scripts/bench/balancer.mjs --listen 20127 --backends 20301,20302,20303 [--host 127.0.0.1]
//
// Every accepted TCP connection is pinned to the next backend in turn and piped both ways (no HTTP
// parsing), which is what an L4 load balancer (haproxy mode tcp, nginx stream) does. Balancing is
// per connection, so a closed-loop client with c keep-alive connections gets c/n requests in
// flight per backend. The peer address the backends see is always 127.0.0.1 (no PROXY protocol).
import net from "node:net";

export function startBalancer({ listen, backends, host = "127.0.0.1" }) {
  let next = 0;
  const perBackend = backends.map(() => 0);
  const server = net.createServer({ noDelay: true }, (client) => {
    const i = next++ % backends.length;
    perBackend[i]++;
    const upstream = net.connect({ port: backends[i], host, noDelay: true });
    client.pipe(upstream);
    upstream.pipe(client);
    const kill = () => { client.destroy(); upstream.destroy(); };
    client.on("error", kill);
    upstream.on("error", kill);
    client.on("close", () => upstream.destroy());
    upstream.on("close", () => client.destroy());
  });
  server.listen(listen, host);
  return { server, stats: () => ({ connectionsPerBackend: perBackend.slice() }) };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const argv = process.argv.slice(2);
  const arg = (n, d) => { const i = argv.indexOf(`--${n}`); return i >= 0 ? argv[i + 1] : d; };
  const lb = startBalancer({
    listen: Number(arg("listen", 20127)),
    backends: String(arg("backends", "")).split(",").filter(Boolean).map(Number),
    host: arg("host", "127.0.0.1"),
  });
  console.log(`[balancer] listening, backends ${arg("backends")}`);
  process.on("SIGTERM", () => { console.log(JSON.stringify(lb.stats())); process.exit(0); });
}
