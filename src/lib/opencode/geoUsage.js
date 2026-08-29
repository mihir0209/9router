import fs from "node:fs";
import path from "node:path";
import { DATA_DIR } from "../dataDir.js";
import { ISO3166_NAMES } from "./iso3166Names.js";

export const OPENCODE_DATA_URL = "https://opencode.ai/data";
export const GEO_STATE_TTL_MS = 12 * 60 * 60 * 1000; // matches the 12h cron cadence
const FETCH_TIMEOUT_MS = 30000;
const FETCH_RETRIES = 2;
const FETCH_UA =
  "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";

// The page serializes the geo section as objects with this exact field pattern.
const GEO_RECORD_PATTERN =
  /\{country:"(?<code>[A-Z0-9]{2})",continent:"(?<continent>[A-Z]{0,2})",tokens:(?<tokens>[0-9]+(?:\.[0-9]+)?),share:(?<share>[0-9]+(?:\.[0-9]+)?),rank:(?<rank>[0-9]+)\}/g;

export function getStateFilePath() {
  return path.join(DATA_DIR, "opencode-geo.json");
}

/**
 * Extract country usage records from the opencode.ai/data page HTML.
 * @param {string} html
 * @returns {Array<{rank:number, country_code:string, continent_code:string, tokens_trillion:number, share_percent:number, country:string}>}
 */
export function extractGeoRecords(html) {
  const records = [];
  const pattern = new RegExp(GEO_RECORD_PATTERN.source, "g");
  for (const match of String(html || "").matchAll(pattern)) {
    records.push({
      rank: Number.parseInt(match.groups.rank, 10),
      country_code: match.groups.code,
      continent_code: match.groups.continent,
      tokens_trillion: Number.parseFloat(match.groups.tokens),
      share_percent: Number.parseFloat(match.groups.share),
    });
  }

  for (const record of records) {
    record.country = ISO3166_NAMES[record.country_code] || record.country_code;
  }

  records.sort((a, b) => a.rank - b.rank);
  return records;
}

/**
 * Validate the embedded data before trusting it.
 * @param {Array} records
 */
export function validateGeoRecords(records) {
  if (!Array.isArray(records) || records.length === 0) {
    throw new Error("No geo records found in page HTML");
  }
  const ranks = records.map((r) => r.rank);
  const expected = Array.from({ length: records.length }, (_, i) => i + 1);
  if (ranks.some((r) => !Number.isInteger(r)) || ranks.join(",") !== expected.join(",")) {
    throw new Error("Geo ranks are not contiguous");
  }
  const codes = new Set(records.map((r) => r.country_code));
  if (codes.size !== records.length) {
    throw new Error("Duplicate country codes found");
  }
  if (records.some((r) => !(r.tokens_trillion >= 0))) {
    throw new Error("Negative token usage found");
  }
  if (records.some((r) => !(r.share_percent >= 0 && r.share_percent <= 100))) {
    throw new Error("Share percent out of range");
  }
  return records;
}

/**
 * Select the target countries for proxy acquisition: countries sorted ascending
 * by token usage above the threshold — the least-used non-zero markets.
 * @param {Array} records
 * @param {{ minTokens?: number, limit?: number }} [options]
 */
export function selectLowUsageTargets(records, options = {}) {
  const minTokens = Number.isFinite(options.minTokens) ? Number(options.minTokens) : 0;
  const limit = Number.isFinite(options.limit) && options.limit > 0 ? Math.floor(options.limit) : Infinity;
  return records
    .filter((r) => r.tokens_trillion > minTokens)
    .sort((a, b) => a.tokens_trillion - b.tokens_trillion || a.country_code.localeCompare(b.country_code))
    .slice(0, limit)
    .map((r) => ({
      rank: r.rank,
      country_code: r.country_code,
      country: r.country,
      continent_code: r.continent_code,
      tokens_trillion: r.tokens_trillion,
      share_percent: r.share_percent,
    }));
}

function csvEscape(value) {
  const raw = String(value ?? "");
  return /[",\n\r]/.test(raw) ? `"${raw.replace(/"/g, '""')}"` : raw;
}

export function toCsv(rows, fields) {
  const lines = [fields.join(",")];
  for (const row of rows) {
    lines.push(fields.map((field) => csvEscape(row[field])).join(","));
  }
  return `${lines.join("\n")}\n`;
}

/**
 * Fetch the live page and extract + validate geo records.
 * @param {{ url?: string, timeoutMs?: number, retries?: number }} [options]
 */
export async function fetchGeoRecords(options = {}) {
  const url = options.url || OPENCODE_DATA_URL;
  const timeoutMs = options.timeoutMs || FETCH_TIMEOUT_MS;
  const retries = Number.isFinite(options.retries) ? options.retries : FETCH_RETRIES;

  let lastError = null;
  for (let attempt = 0; attempt <= retries; attempt += 1) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
      const res = await fetch(url, {
        cache: "no-store",
        signal: controller.signal,
        headers: { "User-Agent": FETCH_UA, Accept: "text/html" },
      });
      if (!res.ok) throw new Error(`opencode.ai/data returned ${res.status}`);
      const html = await res.text();
      return validateGeoRecords(extractGeoRecords(html));
    } catch (err) {
      lastError = err;
      if (attempt < retries) {
        await new Promise((resolve) => setTimeout(resolve, 2000 * (attempt + 1)));
      }
    } finally {
      clearTimeout(timer);
    }
  }
  throw lastError;
}

export async function readGeoState() {
  try {
    const raw = fs.readFileSync(getStateFilePath(), "utf8");
    const state = JSON.parse(raw);
    if (!state || !Array.isArray(state.records)) return null;
    return state;
  } catch {
    return null;
  }
}

export async function writeGeoState(state) {
  const filePath = getStateFilePath();
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  const tmp = `${filePath}.tmp`;
  fs.writeFileSync(tmp, `${JSON.stringify(state, null, 2)}\n`, "utf8");
  fs.renameSync(tmp, filePath);
  return filePath;
}

function isFresh(state, maxAgeMs) {
  if (!state?.fetchedAt) return false;
  const age = Date.now() - new Date(state.fetchedAt).getTime();
  return Number.isFinite(age) && age >= 0 && age <= maxAgeMs;
}

/**
 * Low-usage target countries from the cached state; refreshes from the network
 * when stale and falls back to stale data when the network fails.
 * @param {{ minTokens?: number, limit?: number, maxAgeMs?: number, forceRefresh?: boolean }} [options]
 * @returns {Promise<Array<{country_code:string, country:string, tokens_trillion:number, share_percent:number, rank:number}>>}
 */
export async function getLowUsageCountries(options = {}) {
  const maxAgeMs = Number.isFinite(options.maxAgeMs) ? options.maxAgeMs : GEO_STATE_TTL_MS;
  let state = options.forceRefresh ? null : await readGeoState();

  if (!isFresh(state, maxAgeMs)) {
    try {
      const records = await fetchGeoRecords({ url: options.url });
      state = {
        schemaVersion: 1,
        sourceUrl: options.url || OPENCODE_DATA_URL,
        fetchedAt: new Date().toISOString(),
        updatedAt: new Date().toISOString(),
        records,
      };
      await writeGeoState(state);
    } catch (err) {
      if (!state) throw err;
      // Keep serving stale targets; annotate why.
      state.lastError = err?.message || String(err);
    }
  }

  return selectLowUsageTargets(state.records, { minTokens: options.minTokens, limit: options.limit });
}
