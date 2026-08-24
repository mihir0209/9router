import { describe, expect, it } from "vitest";
import { resolveRemediation } from "../../open-sse/retry/remediation.js";

const renameError = JSON.stringify({
  error: {
    message: "Unsupported parameter: 'max_completion_tokens'. In the Responses API, this parameter has moved to 'max_output_tokens'.",
  },
});

const thinkingError = JSON.stringify({
  error: {
    message: '"thinking.type.enabled" is not supported for this model. Use "thinking.type.adaptive" and "output_config.effort" to control thinking behavior.',
  },
});

describe("request remediation", () => {
  it("renames the exact top-level argument named by the upstream", async () => {
    const result = await resolveRemediation({
      status: 400,
      errorBody: renameError,
      transformedBody: { max_completion_tokens: 42, input: "hello" },
    });

    expect(result.handlerId).toBe("rename-unsupported-parameter");
    expect(result.transformedBody).toEqual({ max_output_tokens: 42, input: "hello" });
  });

  it("does not overwrite an explicit destination argument", async () => {
    const result = await resolveRemediation({
      status: 400,
      errorBody: renameError,
      transformedBody: { max_completion_tokens: 42, max_output_tokens: 7 },
    });

    expect(result.transformedBody).toEqual({ max_output_tokens: 7 });
  });

  it("applies the same rename rule to arbitrary safe top-level arguments", async () => {
    const result = await resolveRemediation({
      status: 400,
      errorBody: "Unsupported parameter: 'legacy_limit'. This parameter has moved to 'current_limit'.",
      transformedBody: { legacy_limit: 12, input: "hello" },
    });

    expect(result.transformedBody).toEqual({ current_limit: 12, input: "hello" });
  });

  it("does not mutate when the reported source argument is absent", async () => {
    expect(await resolveRemediation({
      status: 400,
      errorBody: renameError,
      transformedBody: { max_tokens: 42 },
    })).toBeNull();
  });

  it("uses captured Claude types and derives effort from the submitted budget", async () => {
    const result = await resolveRemediation({
      status: 400,
      errorBody: thinkingError,
      transformedBody: {
        thinking: { type: "enabled", budget_tokens: 25000 },
        output_config: { format: "json" },
      },
    });

    expect(result.handlerId).toBe("replace-unsupported-claude-thinking-type");
    expect(result.transformedBody).toEqual({
      thinking: { type: "adaptive" },
      output_config: { format: "json", effort: "high" },
    });
  });

  it("preserves an explicit output_config effort", async () => {
    const result = await resolveRemediation({
      status: 400,
      errorBody: thinkingError,
      transformedBody: {
        thinking: { type: "enabled", budget_tokens: 25000 },
        output_config: { effort: "low" },
      },
    });

    expect(result.transformedBody.output_config.effort).toBe("low");
  });

  it("does not repeat a remediation whose handler already ran", async () => {
    expect(await resolveRemediation({
      status: 400,
      errorBody: renameError,
      transformedBody: { max_completion_tokens: 42 },
    }, new Set(["rename-unsupported-parameter"]))).toBeNull();
  });

  it("ignores unrelated bad requests", async () => {
    expect(await resolveRemediation({
      status: 400,
      errorBody: '{"error":{"message":"invalid prompt"}}',
      transformedBody: { max_completion_tokens: 42 },
    })).toBeNull();
  });
});
