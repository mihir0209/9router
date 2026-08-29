import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it, expect, beforeAll, afterAll, vi } from "vitest";

const originalDataDir = process.env.DATA_DIR;
let tempDir;
let geo;

const FIXTURE_HTML = `
<div id="geo-breakdown">
{country:"CN",continent:"AS",tokens:92.2263,share:22.8,rank:1}
{country:"US",continent:"NA",tokens:45.5,share:11.3,rank:2}
{country:"VG",continent:"NA",tokens:0.0001,share:0,rank:3}
{country:"WS",continent:"OC",tokens:0.0001,share:0,rank:4}
{country:"SH",continent:"AF",tokens:0,share:0,rank:5}
</div>
<script>broken {country:"XX",continent:"ZZ",tokens:-1,share:0,rank:9}</script>
`;

beforeAll(async () => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-opencode-geo-"));
  process.env.DATA_DIR = tempDir;
  vi.resetModules();
  geo = await import("@/lib/opencode/geoUsage.js");
});

afterAll(() => {
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
  if (originalDataDir === undefined) delete process.env.DATA_DIR;
  else process.env.DATA_DIR = originalDataDir;
});

describe("extractGeoRecords", () => {
  it("extracts records and resolves ISO names", () => {
    const records = geo.extractGeoRecords(FIXTURE_HTML);
    expect(records).toHaveLength(5);
    expect(records[0]).toMatchObject({
      rank: 1,
      country_code: "CN",
      continent_code: "AS",
      tokens_trillion: 92.2263,
      share_percent: 22.8,
      country: "China",
    });
    expect(records.find((r) => r.country_code === "VG")?.tokens_trillion).toBe(0.0001);
    expect(records.find((r) => r.country_code === "SH")?.country).toBe(
      "Saint Helena, Ascension and Tristan da Cunha"
    );
  });

  it("ignores malformed/negative entries outside the exact shape", () => {
    const records = geo.extractGeoRecords(FIXTURE_HTML);
    expect(records.find((r) => r.country_code === "XX")).toBeUndefined();
  });

  it("sorts by rank regardless of appearance order", () => {
    const shuffled = geo.extractGeoRecords(
      '{country:"US",continent:"NA",tokens:1,share:1,rank:2}{country:"CN",continent:"AS",tokens:2,share:2,rank:1}'
    );
    expect(shuffled.map((r) => r.rank)).toEqual([1, 2]);
  });
});

describe("validateGeoRecords", () => {
  it("accepts contiguous valid records", () => {
    const records = geo.extractGeoRecords(FIXTURE_HTML);
    expect(geo.validateGeoRecords(records)).toHaveLength(5);
  });

  it.each([
    ["empty", []],
    ["non-contiguous ranks", [
      { rank: 1, country_code: "CN", continent_code: "AS", tokens_trillion: 1, share_percent: 1 },
      { rank: 3, country_code: "US", continent_code: "NA", tokens_trillion: 1, share_percent: 1 },
    ]],
    ["duplicate codes", [
      { rank: 1, country_code: "CN", continent_code: "AS", tokens_trillion: 1, share_percent: 1 },
      { rank: 2, country_code: "CN", continent_code: "AS", tokens_trillion: 1, share_percent: 1 },
    ]],
    ["negative tokens", [
      { rank: 1, country_code: "CN", continent_code: "AS", tokens_trillion: -1, share_percent: 1 },
    ]],
    ["out-of-range share", [
      { rank: 1, country_code: "CN", continent_code: "AS", tokens_trillion: 1, share_percent: 120 },
    ]],
  ])("rejects %s", (_name, records) => {
    expect(() => geo.validateGeoRecords(records)).toThrow();
  });
});

describe("selectLowUsageTargets", () => {
  it("sorts ascending by token usage and excludes zero usage", () => {
    const records = geo.extractGeoRecords(FIXTURE_HTML);
    const targets = geo.selectLowUsageTargets(records);
    expect(targets.map((t) => t.country_code)).toEqual(["VG", "WS", "US", "CN"]);
    const tokens = targets.map((t) => t.tokens_trillion);
    expect([...tokens].sort((a, b) => a - b)).toEqual(tokens);
  });

  it("threshold is exclusive (greater than, not equal)", () => {
    const records = geo.extractGeoRecords(FIXTURE_HTML);
    expect(geo.selectLowUsageTargets(records, { minTokens: 0 })).toHaveLength(4);
    expect(geo.selectLowUsageTargets(records, { minTokens: 0.0001 }).map((t) => t.country_code)).toEqual(["US", "CN"]);
    expect(geo.selectLowUsageTargets(records, { minTokens: 45.5 }).map((t) => t.country_code)).toEqual(["CN"]);
  });

  it("applies limit after sorting", () => {
    const records = geo.extractGeoRecords(FIXTURE_HTML);
    const targets = geo.selectLowUsageTargets(records, { limit: 2 });
    expect(targets.map((t) => t.country_code)).toEqual(["VG", "WS"]);
  });
});

describe("toCsv", () => {
  it("quotes fields containing commas/quotes", () => {
    const csv = geo.toCsv(
      [{ name: 'Saint Helena, Ascension "area"', tokens: 0 }],
      ["name", "tokens"]
    );
    expect(csv).toBe('name,tokens\n"Saint Helena, Ascension ""area""",0\n');
  });
});

describe("state file roundtrip", () => {
  it("writes and reads state atomically", async () => {
    const state = {
      schemaVersion: 1,
      sourceUrl: "https://opencode.ai/data",
      fetchedAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
      records: [{ rank: 1, country_code: "CN", continent_code: "AS", tokens_trillion: 1, share_percent: 1, country: "China" }],
    };
    await geo.writeGeoState(state);
    const loaded = await geo.readGeoState();
    expect(loaded.records).toHaveLength(1);
    expect(loaded.records[0].country_code).toBe("CN");
  });

  it("returns null for missing/corrupt state", async () => {
    fs.writeFileSync(path.join(tempDir, "opencode-geo.json"), "{corrupt", "utf8");
    expect(await geo.readGeoState()).toBeNull();
    fs.rmSync(path.join(tempDir, "opencode-geo.json"));
    expect(await geo.readGeoState()).toBeNull();
  });
});
