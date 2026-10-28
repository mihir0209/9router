// Cached wall-clock formatter for log lines.
//
// `new Date().toLocaleTimeString("en-US", opts)` builds a fresh Intl.DateTimeFormat on every call,
// which showed up as ~5.6% of server CPU in a load profile (several log lines per request). This
// builds the formatter once and memoises the string per wall-clock second, with output identical
// to toLocaleTimeString for the same options.
const HHMMSS = { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" };

export function makeTimeFormatter(options = HHMMSS) {
  const formatter = new Intl.DateTimeFormat("en-US", { hour: "numeric", minute: "numeric", second: "numeric", ...options });
  let lastSecond = -1;
  let lastText = "";
  return function formatNow() {
    const now = Date.now();
    const second = Math.floor(now / 1000);
    if (second !== lastSecond) {
      lastText = formatter.format(now);
      lastSecond = second;
    }
    return lastText;
  };
}

/** "HH:MM:SS" (24h, zero-padded), same as toLocaleTimeString("en-US", { hour12: false, hour/minute/second: "2-digit" }). */
export const hhmmss = makeTimeFormatter(HHMMSS);
