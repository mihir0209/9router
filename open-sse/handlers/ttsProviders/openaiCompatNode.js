import { Buffer } from "node:buffer";
import { buildCustomMediaHeaders, resolveCustomMediaUrl } from "../../config/customMedia.js";

const openaiCompatNode = {
  async synthesize(text, model, credentials, responseFormat) {
    const config = credentials?.providerSpecificData;
    const [modelId, voice = "alloy"] = String(model || "").split("/");
    const format = responseFormat === "json" ? "mp3" : responseFormat;
    const res = await fetch(resolveCustomMediaUrl(config, "tts"), {
      method: "POST",
      headers: buildCustomMediaHeaders(config, credentials),
      body: JSON.stringify({ model: modelId, input: text, voice, response_format: format }),
    });
    if (!res.ok) {
      const error = await res.text().catch(() => "");
      throw new Error(error || `Custom TTS failed: ${res.status}`);
    }
    return { base64: Buffer.from(await res.arrayBuffer()).toString("base64"), format };
  },
};

export default openaiCompatNode;
