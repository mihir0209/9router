import { NextResponse } from "next/server";
import { createProviderNode, getProviderNodes } from "@/models";
import { OPENAI_COMPATIBLE_PREFIX, ANTHROPIC_COMPATIBLE_PREFIX, CUSTOM_EMBEDDING_PREFIX, CUSTOM_MEDIA_PREFIX } from "@/shared/constants/providers";
import { generateId } from "@/shared/utils";
import { normalizeUserRetryConfig } from "open-sse/retry/index.js";
import { CUSTOM_MEDIA_KINDS, sanitizeCustomHeaders } from "open-sse/config/customMedia.js";

export const dynamic = "force-dynamic";

function sanitizeRetry(raw) {
  if (raw == null) return undefined;
  if (raw === false || raw === "") return null;
  return normalizeUserRetryConfig(raw) || null;
}

const OPENAI_COMPATIBLE_DEFAULTS = {
  baseUrl: "https://api.openai.com/v1",
};

const ANTHROPIC_COMPATIBLE_DEFAULTS = {
  baseUrl: "https://api.anthropic.com/v1",
};

const CUSTOM_EMBEDDING_DEFAULTS = {
  baseUrl: "https://api.openai.com/v1",
};

// GET /api/provider-nodes - List all provider nodes
export async function GET() {
  try {
    const nodes = await getProviderNodes();
    return NextResponse.json({ nodes });
  } catch (error) {
    console.log("Error fetching provider nodes:", error);
    return NextResponse.json({ error: "Failed to fetch provider nodes" }, { status: 500 });
  }
}

// POST /api/provider-nodes - Create provider node
export async function POST(request) {
  try {
    const body = await request.json();
    const { name, prefix, apiType, baseUrl, endpointUrl, type, retry, mediaKind, authHeader, models, customHeaders } = body;
    const retryConfig = sanitizeRetry(retry);

    if (!name?.trim()) {
      return NextResponse.json({ error: "Name is required" }, { status: 400 });
    }

    if (!prefix?.trim()) {
      return NextResponse.json({ error: "Prefix is required" }, { status: 400 });
    }

    // Determine type
    const nodeType = type || "openai-compatible";
    const retryFields = retryConfig ? { retry: retryConfig } : {};

    if (nodeType === "openai-compatible") {
      if (!apiType || !["chat", "responses"].includes(apiType)) {
        return NextResponse.json({ error: "Invalid OpenAI compatible API type" }, { status: 400 });
      }

      const node = await createProviderNode({
        id: `${OPENAI_COMPATIBLE_PREFIX}${apiType}-${generateId()}`,
        type: "openai-compatible",
        prefix: prefix.trim(),
        apiType,
        baseUrl: (baseUrl || OPENAI_COMPATIBLE_DEFAULTS.baseUrl).trim(),
        name: name.trim(),
        ...retryFields,
      });
      return NextResponse.json({ node }, { status: 201 });
    }

    if (nodeType === "custom-embedding") {
      // Strip trailing slash and /embeddings if user pasted full endpoint
      let sanitizedBaseUrl = (baseUrl || CUSTOM_EMBEDDING_DEFAULTS.baseUrl).trim().replace(/\/$/, "");
      if (sanitizedBaseUrl.endsWith("/embeddings")) {
        sanitizedBaseUrl = sanitizedBaseUrl.slice(0, -"/embeddings".length);
      }

      const node = await createProviderNode({
        id: `${CUSTOM_EMBEDDING_PREFIX}${generateId()}`,
        type: "custom-embedding",
        prefix: prefix.trim(),
        baseUrl: sanitizedBaseUrl,
        name: name.trim(),
        ...retryFields,
      });
      return NextResponse.json({ node }, { status: 201 });
    }

    if (nodeType === "custom-media") {
      if (!CUSTOM_MEDIA_KINDS.has(mediaKind)) {
        return NextResponse.json({ error: "Invalid custom media kind" }, { status: 400 });
      }
      if (!baseUrl?.trim() && !endpointUrl?.trim()) {
        return NextResponse.json({ error: "Base URL or full endpoint URL is required" }, { status: 400 });
      }
      const node = await createProviderNode({
        id: `${CUSTOM_MEDIA_PREFIX}${generateId()}`,
        type: "custom-media",
        name: name.trim(),
        prefix: prefix.trim(),
        mediaKind,
        baseUrl: baseUrl?.trim().replace(/\/$/, "") || "",
        endpointUrl: endpointUrl?.trim() || "",
        authHeader: ["bearer", "x-api-key", "api-key"].includes(authHeader) ? authHeader : "bearer",
        models: Array.isArray(models) ? models.map(String).map((v) => v.trim()).filter(Boolean) : [],
        customHeaders: sanitizeCustomHeaders(customHeaders),
        ...retryFields,
      });
      return NextResponse.json({ node }, { status: 201 });
    }

    if (nodeType === "anthropic-compatible") {
      // Sanitize Base URL: remove trailing slash, and remove trailing /messages if user added it
      // This prevents double-appending /messages at runtime
      let sanitizedBaseUrl = (baseUrl || ANTHROPIC_COMPATIBLE_DEFAULTS.baseUrl).trim().replace(/\/$/, "");
      if (sanitizedBaseUrl.endsWith("/messages")) {
        sanitizedBaseUrl = sanitizedBaseUrl.slice(0, -9); // remove /messages
      }

      const node = await createProviderNode({
        id: `${ANTHROPIC_COMPATIBLE_PREFIX}${generateId()}`,
        type: "anthropic-compatible",
        prefix: prefix.trim(),
        baseUrl: sanitizedBaseUrl,
        name: name.trim(),
        ...retryFields,
      });
      return NextResponse.json({ node }, { status: 201 });
    }

    return NextResponse.json({ error: "Invalid provider node type" }, { status: 400 });
  } catch (error) {
    console.log("Error creating provider node:", error);
    return NextResponse.json({ error: "Failed to create provider node" }, { status: 500 });
  }
}
