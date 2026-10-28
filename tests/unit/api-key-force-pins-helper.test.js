import { describe, expect, it } from "vitest";
import { validateForcePins, FORCE_PIN_MAX_LENGTH } from "@/lib/apiKeys/forcePins.js";

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
