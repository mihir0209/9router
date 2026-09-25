import { describe, expect, it } from "vitest";
import { handleComboChat } from "../../../open-sse/services/combo.js";
import {
  REAL_TIMEOUT_MS,
  RUN_REAL,
  invokeChat,
  prepareTarget,
} from "./_helpers.js";

const suite = RUN_REAL ? describe : describe.skip;
const IMAGE_DATA_URL = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFklEQVR4nGO4I2JDEmIY1TCqYfhqAAAeBCwQ8YdREQAAAABJRU5ErkJggg==";

function log() {
  return { info() {}, warn() {} };
}

function failure(status, message) {
  return new Response(JSON.stringify({ error: { message } }), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

async function chatResponse(target, body) {
  const result = await invokeChat(target, body);
  return result.response;
}

suite("REAL combo fallback", () => {
  it("handles a healthy combo directly", async (context) => {
    const target = await prepareTarget({ requiredCapabilities: {} });
    if (!target) context.skip("no active credential with an eligible chat model");
    const response = await handleComboChat({
      body: { stream: false, max_tokens: 32, messages: [{ role: "user", content: "Reply exactly COMBO_OK." }] },
      models: [target.fullModel],
      handleSingleModel: () => chatResponse(target, { stream: false, max_tokens: 32, messages: [{ role: "user", content: "Reply exactly COMBO_OK." }] }),
      log: log(),
    });
    expect(response.ok).toBe(true);
    expect(await response.text()).toContain("COMBO_OK");
  }, REAL_TIMEOUT_MS);

  it("falls through when the first credential is missing", async (context) => {
    const target = await prepareTarget({ requiredCapabilities: {} });
    if (!target) context.skip("no active credential with an eligible chat model");
    const models = ["e2e-missing-provider/e2e-missing-model", target.fullModel];
    const attempted = [];
    const response = await handleComboChat({
      body: { stream: false, max_tokens: 32, messages: [{ role: "user", content: "Reply exactly FALLTHROUGH_OK." }] },
      models,
      handleSingleModel: async (_body, model) => {
        attempted.push(model);
        if (model === models[0]) return failure(401, "missing credential");
        return chatResponse(target, { stream: false, max_tokens: 32, messages: [{ role: "user", content: "Reply exactly FALLTHROUGH_OK." }] });
      },
      log: log(),
    });
    expect(attempted).toEqual(models);
    expect(response.ok).toBe(true);
    expect(await response.text()).toContain("FALLTHROUGH_OK");
  }, REAL_TIMEOUT_MS);

  it("returns 503 when every combo member fails", async () => {
    const response = await handleComboChat({
      body: { stream: false, messages: [{ role: "user", content: "This must not reach a provider." }] },
      models: ["e2e-missing-provider/e2e-missing-model", "e2e-missing-provider/e2e-other-model"],
      handleSingleModel: async () => failure(503, "all credentials unavailable"),
      log: log(),
    });
    expect(response.status).toBe(503);
  }, REAL_TIMEOUT_MS);

  it("reorders a combo for a vision request", async (context) => {
    const vision = await prepareTarget({ requiredCapabilities: { vision: true } });
    const text = await prepareTarget({
      requiredCapabilities: {},
      modelPredicate: ({ capabilities }) => !capabilities.vision,
    });
    if (!vision || !text) context.skip("combo needs one vision and one text-only eligible model");
    const attempted = [];
    const response = await handleComboChat({
      body: {
        stream: false,
        max_tokens: 32,
        messages: [{
          role: "user",
          content: [
            { type: "text", text: "What is the dominant color? Reply with one lowercase color." },
            { type: "image_url", image_url: { url: IMAGE_DATA_URL } },
          ],
        }],
      },
      models: [text.fullModel, vision.fullModel],
      handleSingleModel: async (body, model) => {
        attempted.push(model);
        if (model === text.fullModel) return failure(401, "text model lacks image capability");
        return chatResponse(vision, body);
      },
      log: log(),
    });
    expect(attempted[0]).toBe(vision.fullModel);
    expect(response.ok).toBe(true);
  }, REAL_TIMEOUT_MS);
});
