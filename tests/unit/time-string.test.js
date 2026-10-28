import { describe, it, expect, vi, afterEach } from "vitest";
import { hhmmss, makeTimeFormatter } from "../../open-sse/utils/timeString.js";

// The log-line timestamp formatter is cached (Intl.DateTimeFormat built once, string memoised per
// second). It must stay byte-identical to the toLocaleTimeString call it replaced.
describe("timeString", () => {
  afterEach(() => vi.useRealTimers());

  it("matches toLocaleTimeString for the HH:MM:SS options", () => {
    const opts = { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" };
    for (const iso of ["2026-01-02T03:04:05", "2026-06-15T13:59:59", "2026-12-31T09:00:00", "2026-03-08T12:30:15"]) {
      vi.useFakeTimers();
      vi.setSystemTime(new Date(iso));
      expect(hhmmss()).toBe(new Date(iso).toLocaleTimeString("en-US", opts));
      vi.useRealTimers();
    }
  });

  it("matches toLocaleTimeString for the default hour12:false options used by the sse logger", () => {
    const fmt = makeTimeFormatter({ hour12: false });
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-05-05T07:08:09"));
    expect(fmt()).toBe(new Date("2026-05-05T07:08:09").toLocaleTimeString("en-US", { hour12: false }));
  });

  it("advances when the wall-clock second changes and is memoised within a second", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-01-01T10:00:00.100"));
    const a = hhmmss();
    vi.setSystemTime(new Date("2026-01-01T10:00:00.900"));
    expect(hhmmss()).toBe(a);
    vi.setSystemTime(new Date("2026-01-01T10:00:01.000"));
    expect(hhmmss()).not.toBe(a);
  });
});
