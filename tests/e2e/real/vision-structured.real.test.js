import { describe, expect, it } from "vitest";
import {
  REAL_TIMEOUT_MS,
  RUN_REAL,
  invokeChat,
  prepareTarget,
  requireChatSuccess,
  textFromJson,
  validateJsonSchema,
} from "./_helpers.js";

const IMAGE_DATA_URL = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFklEQVR4nGO4I2JDEmIY1TCqYfhqAAAeBCwQ8YdREQAAAABJRU5ErkJggg==";
const COLOR_SCHEMA = {
  type: "object",
  properties: {
    color: { type: "string", enum: ["red"] },
    evidence: { type: "string", minLength: 1 },
  },
  required: ["color", "evidence"],
  additionalProperties: false,
};
const suite = RUN_REAL ? describe : describe.skip;

suite("REAL vision and structured output", () => {
  it("accepts a base64 image", async (context) => {
    const target = await prepareTarget({ requiredCapabilities: { vision: true } });
    if (!target) context.skip("no active credential with a vision-capable chat model");
    const result = await invokeChat(target, {
      stream: false,
      max_tokens: 64,
      messages: [{
        role: "user",
        content: [
          { type: "text", text: "Read the supplied image, then reply exactly IMAGE_ACCEPTED." },
          { type: "image_url", image_url: { url: IMAGE_DATA_URL } },
        ],
      }],
    });
    requireChatSuccess(context, target, result);
    expect(textFromJson(result.json)).toContain("IMAGE_ACCEPTED");
  }, REAL_TIMEOUT_MS);

  it("uses semantic evidence from the image", async (context) => {
    const target = await prepareTarget({ requiredCapabilities: { vision: true } });
    if (!target) context.skip("no active credential with a vision-capable chat model");
    const result = await invokeChat(target, {
      stream: false,
      max_tokens: 32,
      messages: [{
        role: "user",
        content: [
          { type: "text", text: "What is the dominant color in this image? Reply with one lowercase color." },
          { type: "image_url", image_url: { url: IMAGE_DATA_URL } },
        ],
      }],
    });
    requireChatSuccess(context, target, result);
    expect(textFromJson(result.json).toLowerCase()).toContain("red");
  }, REAL_TIMEOUT_MS);

  it("returns schema-valid JSON non-streaming", async (context) => {
    const target = await prepareTarget({ requiredCapabilities: { vision: true } });
    if (!target) context.skip("no active credential with a vision-capable chat model");
    const result = await invokeChat(target, {
      stream: false,
      max_tokens: 128,
      response_format: {
        type: "json_schema",
        json_schema: { name: "image_color", strict: true, schema: COLOR_SCHEMA },
      },
      messages: [{
        role: "user",
        content: [
          { type: "text", text: "Identify the dominant color and cite the visual evidence. Return only JSON." },
          { type: "image_url", image_url: { url: IMAGE_DATA_URL } },
        ],
      }],
    });
    requireChatSuccess(context, target, result);
    const parsed = JSON.parse(textFromJson(result.json));
    validateJsonSchema(parsed, COLOR_SCHEMA);
    expect(parsed.color).toBe("red");
  }, REAL_TIMEOUT_MS);

  it("returns the same schema-valid JSON while streaming", async (context) => {
    const target = await prepareTarget({ requiredCapabilities: { vision: true } });
    if (!target) context.skip("no active credential with a vision-capable chat model");
    const result = await invokeChat(target, {
      stream: true,
      max_tokens: 128,
      response_format: {
        type: "json_schema",
        json_schema: { name: "image_color", strict: true, schema: COLOR_SCHEMA },
      },
      messages: [{
        role: "user",
        content: [
          { type: "text", text: "Identify the dominant color and cite the visual evidence. Return only JSON." },
          { type: "image_url", image_url: { url: IMAGE_DATA_URL } },
        ],
      }],
    });
    requireChatSuccess(context, target, result);
    expect(result.stream.terminal).toBe(true);
    const parsed = JSON.parse(result.stream.text);
    validateJsonSchema(parsed, COLOR_SCHEMA);
    expect(parsed.color).toBe("red");
  }, REAL_TIMEOUT_MS);
});
