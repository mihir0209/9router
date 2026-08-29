import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { buildCustomMediaHeaders, resolveCustomMediaUrl, sanitizeCustomHeaders } from "../../open-sse/config/customMedia.js";
import { handleImageGenerationCore } from "../../open-sse/handlers/imageGenerationCore.js";
import { handleTtsCore } from "../../open-sse/handlers/ttsCore.js";
import { handleSttCore } from "../../open-sse/handlers/sttCore.js";

const originalFetch = global.fetch;

describe("custom media providers", () => {
  beforeEach(() => { global.fetch = vi.fn(); });
  afterEach(() => { global.fetch = originalFetch; });

  it("resolves standard and full endpoint URLs", () => {
    expect(resolveCustomMediaUrl({ baseUrl: "https://media.test/v1", mediaKind: "image" }, "image"))
      .toBe("https://media.test/v1/images/generations");
    expect(resolveCustomMediaUrl({ endpointUrl: "https://media.test/render", mediaKind: "image" }, "image"))
      .toBe("https://media.test/render");
    expect(() => resolveCustomMediaUrl({ baseUrl: "https://media.test", mediaKind: "tts" }, "image"))
      .toThrow("configured for tts");
  });

  it("builds configured auth and blocks sensitive custom headers", () => {
    const config = {
      authHeader: "x-api-key",
      customHeaders: { "X-Tenant": "acme", Authorization: "unsafe", Cookie: "unsafe" },
    };
    expect(buildCustomMediaHeaders(config, { apiKey: "secret" })).toEqual({
      "Content-Type": "application/json",
      "X-Tenant": "acme",
      "x-api-key": "secret",
    });
    expect(sanitizeCustomHeaders({ "X-Bad": "value\r\ninjected", Host: "evil" })).toEqual({});
  });

  it("forwards OpenAI image fields and passes through an OpenAI response", async () => {
    global.fetch.mockResolvedValue(new Response(JSON.stringify({
      created: 42,
      data: [{ b64_json: "aW1hZ2U=", revised_prompt: "revised" }],
    }), { status: 200, headers: { "Content-Type": "application/json" } }));

    const result = await handleImageGenerationCore({
      body: {
        prompt: "draw a fox", n: 2, size: "1024x1024", quality: "high",
        style: "vivid", response_format: "b64_json", background: "transparent",
        output_format: "webp", output_compression: 80, moderation: "low", user: "client-user",
        unsupported: "drop-me",
      },
      modelInfo: { provider: "custom-media-image", model: "image-v2" },
      credentials: {
        apiKey: "image-key",
        providerSpecificData: {
          mediaKind: "image",
          endpointUrl: "https://images.test/custom-generate",
          authHeader: "api-key",
          customHeaders: { "X-Region": "eu" },
        },
      },
    });

    expect(result.success).toBe(true);
    const [url, init] = global.fetch.mock.calls[0];
    expect(url).toBe("https://images.test/custom-generate");
    expect(init.headers).toEqual(expect.objectContaining({ "api-key": "image-key", "X-Region": "eu" }));
    expect(JSON.parse(init.body)).toEqual({
      model: "image-v2", prompt: "draw a fox", n: 2, quality: "high",
      response_format: "b64_json", size: "1024x1024", style: "vivid", user: "client-user",
      background: "transparent", moderation: "low", output_compression: 80, output_format: "webp",
    });
    expect(await result.response.json()).toEqual({
      created: 42,
      data: [{ b64_json: "aW1hZ2U=", revised_prompt: "revised" }],
    });
  });

  it("routes TTS and STT through full endpoints", async () => {
    global.fetch
      .mockResolvedValueOnce(new Response(new Uint8Array([1, 2, 3]), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ text: "hello" }), { status: 200, headers: { "Content-Type": "application/json" } }));
    const credentials = {
      apiKey: "media-key",
      providerSpecificData: { mediaKind: "tts", endpointUrl: "https://voice.test/speak", authHeader: "bearer" },
    };
    const tts = await handleTtsCore({ provider: "custom-media-voice", model: "tts-1/alloy", input: "hello", credentials });
    expect(tts.success).toBe(true);
    expect(global.fetch.mock.calls[0][0]).toBe("https://voice.test/speak");
    expect(JSON.parse(global.fetch.mock.calls[0][1].body)).toEqual({ model: "tts-1", input: "hello", voice: "alloy", response_format: "mp3" });

    const formData = new FormData();
    formData.set("model", "whisper-1");
    formData.set("file", new File([new Uint8Array([1])], "audio.wav", { type: "audio/wav" }));
    const sttConfig = { custom: true, format: "openai", baseUrl: "https://speech.test/transcribe", isFullEndpoint: true, authType: "apikey", authHeader: "x-api-key", customHeaders: { "X-Tenant": "one" } };
    const stt = await handleSttCore({ provider: "custom-media-stt", model: "whisper-1", formData, credentials: { apiKey: "stt-key" }, sttConfig });
    expect(stt.success).toBe(true);
    expect(global.fetch.mock.calls[1][0]).toBe("https://speech.test/transcribe");
    expect(global.fetch.mock.calls[1][1].headers).toEqual({ "X-Tenant": "one", "x-api-key": "stt-key" });
  });
});
