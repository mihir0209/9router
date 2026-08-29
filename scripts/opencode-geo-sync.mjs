#!/usr/bin/env node
/**
 * OpenCode geo-breakdown sync — cron-friendly replacement for the Python extractor.
 *
 * Fetches https://opencode.ai/data, extracts the "Tokens used by country"
 * section, and writes:
 *   - opencode_geo_breakdown.csv / .json / .md   (all countries, rank order)
 *   - opencode_geo_targets.csv / .json           (ascending token usage above
 *     the threshold — the least-used non-zero markets for proxy acquisition)
 *   - <DATA_DIR>/opencode-geo.json               (state consumed by the app's
 *     proxy-pool geo targeting)
 *
 * Cron (every 12 hours):
 *   0 0,12 * * * cd /path/to/9router && node scripts/opencode-geo-sync.mjs >> $HOME/.9router/opencode-geo/sync.log 2>&1
 *
 * Env:
 *   OPENCODE_GEO_MIN_TOKENS  minimum token usage in trillions, exclusive (default "0")
 *   OPENCODE_GEO_OUT_DIR     output directory (default <DATA_DIR>/opencode-geo)
 *   OPENCODE_GEO_URL         source URL override
 */
import fs from "node:fs";
import path from "node:path";
import { DATA_DIR } from "../src/lib/dataDir.js";
import {
  OPENCODE_DATA_URL,
  fetchGeoRecords,
  selectLowUsageTargets,
  toCsv,
  writeGeoState,
} from "../src/lib/opencode/geoUsage.js";

const BREAKDOWN_FIELDS = [
  "rank",
  "country",
  "country_code",
  "continent_code",
  "tokens_trillion",
  "share_percent",
];
const TARGET_FIELDS = [
  "country_code",
  "country",
  "continent_code",
  "tokens_trillion",
  "share_percent",
  "rank",
];

function formatNumber(value) {
  return Number.isInteger(value) ? String(value) : String(Number(value.toFixed(6)));
}

function buildMarkdown(records, meta) {
  const lines = [
    "# OpenCode Geo Breakdown",
    "",
    `Source: ${meta.sourceUrl}`,
    "",
    'The page labels this section "Tokens used by country." Token values below are reported in **trillions of tokens (T)**, matching the source page\'s embedded numeric data.',
    "",
    `**Records extracted:** ${records.length}`,
    `**Extracted at:** ${meta.fetchedAt}`,
    "",
    "| Rank | Country | ISO-2 | Continent | Token usage (T) | Share |",
    "|---:|---|:---:|:---:|---:|---:|",
  ];
  for (const r of records) {
    lines.push(
      `| ${r.rank} | ${r.country} | ${r.country_code} | ${r.continent_code || "—"} | ` +
        `${formatNumber(r.tokens_trillion)} | ${formatNumber(r.share_percent)}% |`
    );
  }
  return `${lines.join("\n")}\n`;
}

async function main() {
  const outDir = process.env.OPENCODE_GEO_OUT_DIR
    ? path.resolve(process.env.OPENCODE_GEO_OUT_DIR)
    : path.join(DATA_DIR, "opencode-geo");
  const url = process.env.OPENCODE_GEO_URL || OPENCODE_DATA_URL;
  const minTokens = Number.parseFloat(process.env.OPENCODE_GEO_MIN_TOKENS ?? "0");

  const records = await fetchGeoRecords({ url });
  const fetchedAt = new Date().toISOString();

  // Persist state for the app-side proxy refresh before writing reports.
  await writeGeoState({
    schemaVersion: 1,
    sourceUrl: url,
    fetchedAt,
    updatedAt: fetchedAt,
    records,
  });

  const targets = selectLowUsageTargets(records, {
    minTokens: Number.isFinite(minTokens) ? minTokens : 0,
  });

  fs.mkdirSync(outDir, { recursive: true });
  fs.writeFileSync(
    path.join(outDir, "opencode_geo_breakdown.csv"),
    toCsv(records, BREAKDOWN_FIELDS),
    "utf8"
  );
  fs.writeFileSync(
    path.join(outDir, "opencode_geo_breakdown.json"),
    `${JSON.stringify(records, null, 2)}\n`,
    "utf8"
  );
  fs.writeFileSync(
    path.join(outDir, "opencode_geo_breakdown.md"),
    buildMarkdown(records, { sourceUrl: url, fetchedAt }),
    "utf8"
  );
  fs.writeFileSync(
    path.join(outDir, "opencode_geo_targets.csv"),
    toCsv(targets, TARGET_FIELDS),
    "utf8"
  );
  fs.writeFileSync(
    path.join(outDir, "opencode_geo_targets.json"),
    `${JSON.stringify(
      {
        sourceUrl: url,
        fetchedAt,
        minTokens: Number.isFinite(minTokens) ? minTokens : 0,
        count: targets.length,
        countries: targets,
      },
      null,
      2
    )}\n`,
    "utf8"
  );

  console.log(
    JSON.stringify(
      {
        ok: true,
        records: records.length,
        targets: targets.length,
        minTokens: Number.isFinite(minTokens) ? minTokens : 0,
        first_target: targets[0] || null,
        token_sum_trillion: Number(sumTokens(records).toFixed(6)),
        share_sum_percent: Number(records.reduce((acc, r) => acc + r.share_percent, 0).toFixed(6)),
        out_dir: outDir,
        state_file_updated: true,
        fetched_at: fetchedAt,
      },
      null,
      2
    )
  );
}

function sumTokens(records) {
  let total = 0;
  for (const r of records) total += r.tokens_trillion;
  return total;
}

main().catch((err) => {
  console.error(JSON.stringify({ ok: false, error: err?.message || String(err) }));
  process.exit(1);
});
