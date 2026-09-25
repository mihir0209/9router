import { handleChat } from "@/sse/handlers/chat.js";
import { initTranslators } from "open-sse/translator/index.js";
import { createInternalRequestHeaders } from "@/lib/auth/internalRequest.js";

let initialized = false;

async function ensureInitialized() {
  if (!initialized) {
    await initTranslators();
    initialized = true;
  }
}

export async function POST(request) {
  await ensureInitialized();

  const headers = new Headers(request.headers);
  for (const [name, value] of Object.entries(await createInternalRequestHeaders(new URL(request.url).pathname))) headers.set(name, value);

  const internalRequest = new Request(request.url, {
    method: request.method,
    headers,
    body: request.body,
    signal: request.signal,
    duplex: "half",
  });

  return await handleChat(internalRequest);
}
