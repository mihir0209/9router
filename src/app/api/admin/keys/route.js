import { NextResponse } from "next/server";
import { getConsistentMachineId } from "@/shared/utils/machineId";
import { getApiKeys, createApiKey, updateApiKey } from "@/lib/localDb";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken";
import { serializeAdminKey, validateOwnerRef, validatePolicy } from "@/lib/apiKeys/adminPolicy";
import { appendAuditEvent } from "@/lib/db/repos/auditRepo.js";
import { trustedClientIp } from "@/lib/auth/clientIp.js";

export const dynamic = "force-dynamic";

async function requireAdmin(request) {
  return await hasValidAdminApiToken(request) ? null : NextResponse.json({ error: "Unauthorized" }, { status: 401 });
}

function ownerFromQuery(request) {
  return validateOwnerRef(new URL(request.url).searchParams.get("ownerRef"));
}

function badRequest(error) {
  return NextResponse.json({ error }, { status: 400 });
}

function duplicateResponse() {
  return NextResponse.json({ error: "API key already in use" }, { status: 409 });
}

export async function GET(request) {
  const unauthorized = await requireAdmin(request);
  if (unauthorized) return unauthorized;
  const owner = ownerFromQuery(request);
  if (owner.error) return badRequest(owner.error);
  const keys = await getApiKeys(owner.value);
  return NextResponse.json({ keys: keys.map((key) => serializeAdminKey(key)) });
}

export async function POST(request) {
  const unauthorized = await requireAdmin(request);
  if (unauthorized) return unauthorized;
  let body;
  try {
    body = await request.json();
  } catch {
    return badRequest("Request body must be valid JSON");
  }
  const policy = validatePolicy(body);
  if (policy.error) return badRequest(policy.error);
  if (policy.data.billingMode === "byok") return badRequest("Configure BYOK credentials after creating the key");

  try {
    const { name, ownerRef, key, machineId: requestedMachineId, isActive, ...options } = policy.data;
    const machineId = requestedMachineId || await getConsistentMachineId();
    let apiKey = await createApiKey(name, machineId, { ...options, ownerRef, ...(key ? { key } : {}) });
    if (isActive === false) apiKey = { ...apiKey, ...(await updateApiKey(apiKey.id, { isActive: false })) };
    void appendAuditEvent({ actor: "admin-token", action: "api_key.created", resourceType: "api_key", resourceId: apiKey.id, ownerRef, requestId: request.headers.get("x-request-id"), sourceIp: trustedClientIp(request), after: serializeAdminKey(apiKey) }).catch(() => {});
    return NextResponse.json(serializeAdminKey(apiKey, { includeSecret: true }), { status: 201 });
  } catch (error) {
    console.error("ADMIN_KEY_CREATE_FAILED", error?.stack || error?.message || error);
    if (error?.code === "DUPLICATE_API_KEY") return duplicateResponse();
    return NextResponse.json({ error: "Failed to create key" }, { status: 500 });
  }
}
