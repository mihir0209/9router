# CPU profile of the chat path (self time)

Profile of one Node server process serving `POST /v1/chat/completions` (non-stream, valid key, instant mock
upstream), taken on the build of commit `e3be0d09` plus the benchmark tooling (no source difference on the request
path). Linked from [REPORT.md](REPORT.md#11-cpu-profile-of-the-chat-path).

How it was taken (the `.cpuprofile` file is not committed, it is 2 MB and machine specific):

```sh
node scripts/bench/run-bench.mjs --src ~/.9router/db/data.sqlite --out /tmp/prof-run \
  --scenarios chat-ns-plain --concurrency 32 --repeats 1 --duration 25 --warmup 3 \
  --server-cpus 0-3 --mock-cpus 8-9 --server-node-args --cpu-prof,--cpu-prof-dir=/tmp/prof
node scripts/bench/cpu-top.mjs /tmp/prof/CPU.*.cpuprofile 22
```

The server sustained 516 req/s at c=32 during the profiled window (about 6% below the unprofiled 519-617 req/s of the
main runs: the sampler costs a little). 32.7 s of samples, which includes the 4 s warm-up and the cold start of the
process (module loading shows up as `wrapSafe`, 0.9%).

## Result

**There is no single function above 5% self time.** The largest one is the settings normaliser at 2.6%. The profile is
flat: the cost is spread across Next.js request plumbing, undici (the upstream call to the mock), bundled application
code and the Node runtime. By the rule applied here (fix only a cheap, safe hot spot above 5% self time) nothing was
changed.

### By category

| Category | Self time |
|---|--:|
| application code (bundled `.next/server/chunks`) | 30.5% |
| Next.js server (`node_modules/next`) | 19.4% |
| native / runtime internals (JSON parse, socket write, microtask drain, ...) | 15.9% |
| undici (upstream `fetch`, `Request`/`Response`) | 9.7% |
| garbage collector | 3.7% |
| idle | 3.7% (server saturated: it was CPU bound) |
| Node core: webstreams 2.7%, module loading 1.8%, streams 1.6%, everything else under 1% each | about 11% |

### Top functions by self time

| Self | Function | What it is |
|--:|---|---|
| 3.7% | `(garbage collector)` | |
| 3.7% | `(idle)` | |
| 3.3% | `runMicrotasks` | promise/async continuation drain, spread over the whole async request pipeline |
| 2.6% | `s` `chunks/9422.js:13` | settings normaliser: merges the stored settings over the defaults and loops over every default key on each call |
| 2.0% | `transaction` `chunks/8520.js:9` | `node:sqlite` adapter transaction wrapper (the per-request usage write; time inside the native `exec` is attributed here) |
| 1.8% | `parse` | `JSON.parse` |
| 1.6% | `writev` | socket write |
| 1.4% | `consoleCall` | the per-request INFO log lines |
| 1.3% | `(anon)` `chunks/9422.js:9` | provider connection list: copies every connection object (`{...a}`) per lookup |
| 1.1% | `writeUtf8String` | response write |
| 1.0% | `P` `chunks/8895.js` | `handleChatCore` body |
| 0.9% | `join` (`node:path`) | path joins on the hot path |
| 0.8% | `i` `chunks/8920.js` | glob matcher (cached since `2eed5ba6`; remaining cost is the `RegExp.test`) |
| 0.8% / 0.8% / 0.6% | `all` / `run` / `get` `chunks/8520.js` | prepared-statement calls of the SQLite adapter |
| 0.8% | `BubbledError` `next/.../trace/tracer.js` | Next's span wrapper |
| 0.7% | `_Request` (undici) | `new Request` for the upstream call |
| 0.6% | `ok` (`node:assert`) | `assert.ok` calls inside Next |
| 0.6% | `execute` `chunks/1438.js` | `BaseExecutor.execute` |

### Reading the numbers

* The SQLite adapter (`transaction` + `all` + `run` + `get`) is about **4.2%** in total: one usage transaction per
  request. This is the part that is shared across processes in the multi-process setup (REPORT.md section 10), so it is the only
  piece whose cost can grow with the number of processes. It no longer fails under contention (`e3be0d09`).
* Settings normalisation (`s`) plus the provider-connection copy (`(anon) chunks/9422.js:9`) are **3.9%** together.
  Both are pure per-call object work on data that changes only when the dashboard saves; a snapshot cache keyed on
  the settings/connection version would remove most of it. It is worth about 3-4% of CPU per request, below the
  5% bar, so it was left alone.
* Per-request INFO logging (`consoleCall` + `writeUtf8String` + `writev`) is about 4% together; silencing it was
  already measured as within noise (REPORT.md section 1).
* Idle is 3.7%: the process was saturated, as intended for a throughput profile.

Not verified: a profile of the stream path, of the pinned/ACL key path, or at other concurrencies; one profile run was
taken for the numbers above (two earlier runs of the same setup showed the same ordering: top application function
2.5% and 2.6%).
