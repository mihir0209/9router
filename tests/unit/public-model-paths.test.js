import { describe, it, expect } from "vitest";
import { PUBLIC_MODEL_PATHS, isPublicModelPath } from "../src/lib/publicModelPaths.js";

describe("public model paths", () => {
  it("treats the model catalogue as readable without a key", () => {
    for (const path of ["/v1/models", "/api/v1/models", "/v1beta/models", "/api/v1beta/models"]) {
      expect(isPublicModelPath(path)).toBe(true);
    }
  });

  it("answers a trailing slash the same way, since it is the same resource", () => {
    for (const path of PUBLIC_MODEL_PATHS) {
      expect(isPublicModelPath(`${path}/`)).toBe(true);
    }
  });

  it("does not widen to any other LLM route", () => {
    for (const path of [
      "/v1/chat/completions",
      "/v1/completions",
      "/v1/messages",
      "/v1/embeddings",
      "/v1/models/gemini",
      "/v1/modelsomething",
      "/api/admin/keys",
      "/",
    ]) {
      expect(isPublicModelPath(path)).toBe(false);
    }
  });

  it("keeps the list non-empty, so a bad edit cannot 401 the whole worker", () => {
    expect(PUBLIC_MODEL_PATHS.length).toBeGreaterThan(0);
  });
});
