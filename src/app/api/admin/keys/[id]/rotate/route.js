import { NextResponse } from "next/server";
import { regenerateApiKey } from "@/lib/db/index.js";
import { getApiKeyById } from "@/lib/localDb";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken";
import { serializeAdminKey, validateOwnerRef } from "@/lib/apiKeys/adminPolicy";
import { appendAuditEvent } from "@/lib/db/repos/auditRepo.js";
import { trustedClientIp } from "@/lib/auth/clientIp.js";

async function requireAdmin(request) {
  return await hasValidAdminApiToken(request) ? null : NextResponse.json({ error: "Unauthorized" }, { status: 401 });
}

async function rotate(request, { params }) {
  const unauthorized = await requireAdmin(request);
  if (unauthorized) return unauthorized;
  let body;
  try {
    body = await request.json();
  } catch {
    return NextResponse.json({ error: "Request body must be valid JSON" }, { status: 400 });
  }
  const owner = validateOwnerRef(body?.ownerRef);
  if (owner.error) return NextResponse.json({ error: owner.error }, { status: 400 });
  const { id } = await params;
  const existing = await getApiKeyById(id);
  if (!existing || existing.ownerRef !== owner.value) return NextResponse.json({ error: "Key not found" }, { status: 404 });
  const key = await regenerateApiKey(id);
  if (key) {
    try {
      await appendAuditEvent({ actor: "admin-token", action: "api_key.rotated", resourceType: "api_key", resourceId: id, ownerRef: owner.value, requestId: request.headers.get("x-request-id"), sourceIp: trustedClientIp(request), metadata: { regenerateVersion: key.regenerateVersion } });
    } catch (error) {
      console.error("AUDIT_WRITE_FAILED", error?.message || error);
    }
  }
  return key ? NextResponse.json(serializeAdminKey(key, { includeSecret: true })) : NextResponse.json({ error: "Key not found" }, { status: 404 });
}

export async function POST(request, context) {
  return rotate(request, context);
}

export async function PUT(request, context) {
  return rotate(request, context);
}
