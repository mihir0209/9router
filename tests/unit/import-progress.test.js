import { describe, it, expect } from "vitest";
import {
  IMPORT_STAGES,
  formatBytes,
  formatElapsed,
  getStageLabel,
  formatProgressMessage,
  withDuration,
} from "@/lib/importProgress.js";

describe("formatBytes", () => {
  it("formats bytes, KB and MB", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2 KB");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(1024 * 1024)).toBe("1 MB");
    expect(formatBytes(1.5 * 1024 * 1024)).toBe("1.5 MB");
    expect(formatBytes(150 * 1024)).toBe("150 KB");
  });

  it("treats invalid input as zero", () => {
    expect(formatBytes(undefined)).toBe("0 B");
    expect(formatBytes(-5)).toBe("0 B");
    expect(formatBytes("abc")).toBe("0 B");
  });
});

describe("formatElapsed", () => {
  it("formats seconds and minutes", () => {
    expect(formatElapsed(0)).toBe("0s");
    expect(formatElapsed(999)).toBe("0s");
    expect(formatElapsed(5400)).toBe("5s");
    expect(formatElapsed(59999)).toBe("59s");
    expect(formatElapsed(60000)).toBe("1m 00s");
    expect(formatElapsed(65000)).toBe("1m 05s");
    expect(formatElapsed(3725000)).toBe("62m 05s");
  });

  it("treats invalid input as zero", () => {
    expect(formatElapsed(NaN)).toBe("0s");
    expect(formatElapsed(-10)).toBe("0s");
    expect(formatElapsed(undefined)).toBe("0s");
  });
});

describe("getStageLabel", () => {
  it("includes the file size in the reading stage", () => {
    expect(getStageLabel(IMPORT_STAGES.reading, 2048)).toBe("Reading backup file (2 KB)");
    expect(getStageLabel(IMPORT_STAGES.reading)).toBe("Reading backup file");
  });

  it("labels every other stage", () => {
    expect(getStageLabel(IMPORT_STAGES.validating)).toBe("Validating backup");
    expect(getStageLabel(IMPORT_STAGES.loadingCurrent)).toBe("Loading current database");
    expect(getStageLabel(IMPORT_STAGES.merging)).toBe("Merging");
    expect(getStageLabel(IMPORT_STAGES.uploading)).toBe("Uploading to server");
    expect(getStageLabel(IMPORT_STAGES.restoring)).toBe(
      "Restoring database (this can take a while for large backups)"
    );
    expect(getStageLabel(IMPORT_STAGES.refreshing)).toBe("Refreshing settings");
  });

  it("returns an empty string for unknown stages", () => {
    expect(getStageLabel("nope")).toBe("");
  });
});

describe("formatProgressMessage", () => {
  const startedAt = 1_000_000;

  it("omits the ticker during the first second", () => {
    const msg = formatProgressMessage({ stage: IMPORT_STAGES.uploading, startedAt }, startedAt + 400);
    expect(msg).toBe("Uploading to server");
  });

  it("appends elapsed time after a second", () => {
    const msg = formatProgressMessage({ stage: IMPORT_STAGES.restoring, startedAt }, startedAt + 7200);
    expect(msg).toBe("Restoring database (this can take a while for large backups) - 7s elapsed");
  });

  it("passes the detail through to the reading label", () => {
    const msg = formatProgressMessage(
      { stage: IMPORT_STAGES.reading, detail: 1024 * 1024, startedAt },
      startedAt
    );
    expect(msg).toBe("Reading backup file (1 MB)");
  });

  it("returns an empty string for missing or unknown progress", () => {
    expect(formatProgressMessage(null)).toBe("");
    expect(formatProgressMessage({})).toBe("");
    expect(formatProgressMessage({ stage: "nope", startedAt })).toBe("");
  });

  it("never shows negative elapsed time", () => {
    const msg = formatProgressMessage({ stage: IMPORT_STAGES.merging, startedAt }, startedAt - 5000);
    expect(msg).toBe("Merging");
  });
});

describe("withDuration", () => {
  it("appends the total duration", () => {
    expect(withDuration("Done", 4200)).toBe("Done (took 4s)");
    expect(withDuration("Done", 125000)).toBe("Done (took 2m 05s)");
  });
});
