import { describe, expect, it } from "vitest";
import { validateForcePins, forcePairChanged, FORCE_PIN_MAX_LENGTH } from "@/lib/apiKeys/forcePins.js";

describe("validateForcePins", () => {
  it("returns no values when nothing is supplied", () => {
    expect(validateForcePins({}, {})).toEqual({ values: {}, error: null });
  });

  it("trims supplied pins", () => {
    const { values, error } = validateForcePins({ forceProvider: "  openai ", forceModel: " openai/gpt-4o  " });
    expect(error).toBeNull();
    expect(values).toEqual({ forceProvider: "openai", forceModel: "openai/gpt-4o" });
  });

  it("treats empty string and null as a clear", () => {
    expect(validateForcePins({ forceProvider: "", forceModel: null }).values).toEqual({ forceProvider: "", forceModel: "" });
  });

  it("rejects non-string values", () => {
    for (const bad of [5, true, {}, ["a"]]) {
      expect(validateForcePins({ forceModel: bad }).error).toBe("forceModel must be a string");
      expect(validateForcePins({ forceProvider: bad }).error).toBe("forceProvider must be a string");
    }
  });

  it("rejects over-long pins", () => {
    const long = "a".repeat(FORCE_PIN_MAX_LENGTH + 1);
    expect(validateForcePins({ forceModel: long }).error).toMatch(/256 characters or fewer/);
    expect(validateForcePins({ forceModel: "a".repeat(FORCE_PIN_MAX_LENGTH) }).error).toBeNull();
  });

  it("rejects inner whitespace and control characters", () => {
    expect(validateForcePins({ forceModel: "openai/gpt 4o" }).error).toMatch(/whitespace or control/);
    expect(validateForcePins({ forceProvider: "open\nai" }).error).toMatch(/whitespace or control/);
    expect(validateForcePins({ forceProvider: "open\u0000ai" }).error).toMatch(/whitespace or control/);
  });

  it("rejects a pin that is in the matching blocked list", () => {
    expect(validateForcePins({ forceModel: "openai/gpt-4o" }, { forceModel: "openai/gpt-4o", blockedModels: "a, openai/gpt-4o" }).error)
      .toBe("forceModel must not be in blockedModels");
    expect(validateForcePins({ forceProvider: "openai" }, { forceProvider: "openai", blockedProviders: ["openai"] }).error)
      .toBe("forceProvider must not be in blockedProviders");
  });

  it("checks the stored pin when only a blocked list is supplied", () => {
    const result = validateForcePins({}, { forceModel: "openai/gpt-4o", blockedModels: "openai/gpt-4o" });
    expect(result.error).toBe("forceModel must not be in blockedModels");
  });

  it("allows clearing a pin that would otherwise conflict", () => {
    const result = validateForcePins({ forceModel: "" }, { forceModel: "", blockedModels: "openai/gpt-4o" });
    expect(result.error).toBeNull();
    expect(result.values).toEqual({ forceModel: "" });
  });
});

describe("validateForcePins provider-prefix normalisation", () => {
  it("rejects a bare forceModel when blockedModels lists it with a provider prefix", () => {
    const state = { forceModel: "gpt-4o", blockedModels: "openai/gpt-4o" };
    expect(validateForcePins({ forceModel: "gpt-4o" }, state).error).toBe("forceModel must not be in blockedModels");
  });

  it("does not flag a prefixed pin against a bare blocked entry or a different suffix", () => {
    // Enforcement checks the requested id and provider/model, so `openai/gpt-4o` is not blocked by `gpt-4o`.
    expect(validateForcePins({ forceModel: "openai/gpt-4o" }, { forceModel: "openai/gpt-4o", blockedModels: "gpt-4o" }).error).toBeNull();
    expect(validateForcePins({ forceModel: "gpt-4o" }, { forceModel: "gpt-4o", blockedModels: "openai/gpt-4o-mini,xgpt-4o" }).error).toBeNull();
  });
});

describe("validateForcePins with a stored (pre-existing) conflict", () => {
  const stored = { forceModel: "openai/gpt-4o", forceProvider: "openai", blockedModels: "openai/gpt-4o", blockedProviders: "openai" };

  it("does not block an edit that leaves the same conflicts in place", () => {
    expect(validateForcePins({ blockedModels: "openai/gpt-4o,x" }, { ...stored, blockedModels: "openai/gpt-4o,x" }, stored).error).toBeNull();
  });

  it("allows fixing or clearing the conflicting pin", () => {
    expect(validateForcePins({ forceModel: "" }, { ...stored, forceModel: "" }, stored).error).toBeNull();
    expect(validateForcePins({ forceProvider: "" }, { ...stored, forceProvider: "" }, stored).error).toBeNull();
  });

  it("rejects introducing a new, different conflict", () => {
    const next = { ...stored, forceModel: "openai/o1", blockedModels: "openai/gpt-4o,openai/o1" };
    expect(validateForcePins({ forceModel: "openai/o1" }, next, stored).error).toBe("forceModel must not be in blockedModels");
  });

  it("without a stored state every conflict is new (create path)", () => {
    expect(validateForcePins({ forceModel: "openai/gpt-4o" }, stored).error).toBe("forceModel must not be in blockedModels");
  });
});

describe("forcePairChanged", () => {
  const stored = { forceProvider: "openai", forceModel: "openai/gpt-4o" };
  it("is false when nothing about the pair changes", () => {
    expect(forcePairChanged({}, stored)).toBe(false);
    expect(forcePairChanged({ forceProvider: "openai", forceModel: "openai/gpt-4o" }, stored)).toBe(false);
  });
  it("is true when either side changes", () => {
    expect(forcePairChanged({ forceProvider: "anthropic" }, stored)).toBe(true);
    expect(forcePairChanged({ forceModel: "" }, stored)).toBe(true);
  });
});
