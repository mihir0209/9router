import { NextResponse } from "next/server";
import { getApiKeyById, updateApiKey, deleteApiKey } from "@/lib/localDb";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken";
import { serializeAdminKey, validateListConflicts, validateOwnerRef, validatePolicy } from "@/lib/apiKeys/adminPolicy";
import { appendAuditEvent } from "@/lib/db/repos/auditRepo.js";
import { trustedClientIp } from "@/lib/auth/clientIp.js";

async function requireAdmin(request) {
  return await hasValidAdminApiToken(request) ? null : NextResponse.json({ error: "Unauthorized" }, { status: 401 });
}

function ownerFromQuery(request) {
  return validateOwnerRef(new URL(request.url).searchParams.get("ownerRef"));
}

function badRequest(error) {
  return NextResponse.json({ error }, { status: 400 });
}

function notFound() {
  return NextResponse.json({ error: "Key not found" }, { status: 404 });
}

async function findOwnedKey(id, ownerRef) {
  const key = await getApiKeyById(id);
  return key?.ownerRef === ownerRef ? key : null;
}

export async function GET(request, { params }) {
  const unauthorized = await requireAdmin(request);
  if (unauthorized) return unauthorized;
  const owner = ownerFromQuery(request);
  if (owner.error) return badRequest(owner.error);
  const { id } = await params;
  const key = await findOwnedKey(id, owner.value);
  return key ? NextResponse.json(serializeAdminKey(key)) : notFound();
}

export async function PUT(request, { params }) {
  const unauthorized = await requireAdmin(request);
  if (unauthorized) return unauthorized;
  const owner = ownerFromQuery(request);
  if (owner.error) return badRequest(owner.error);
  let body;
  try {
    body = await request.json();
  } catch {
    return badRequest("Request body must be valid JSON");
  }
  if (body?.ownerRef !== undefined) {
    const bodyOwner = validateOwnerRef(body.ownerRef);
    if (bodyOwner.error) return badRequest(bodyOwner.error);
    if (bodyOwner.value !== owner.value) return notFound();
  }
  const policy = validatePolicy({ ...body, ownerRef: owner.value }, { partial: true });
  if (policy.error) return badRequest(policy.error);
  const { id } = await params;
  const existing = await findOwnedKey(id, owner.value);
  if (!existing) return notFound();
  const mergedPolicy = {
    allowedModels: existing.allowedModels || "",
    blockedModels: existing.blockedModels || "",
    allowedIPs: existing.allowedIPs || "",
    blockedIPs: existing.blockedIPs || "",
    ...policy.data,
  };
  const conflict = validateListConflicts(mergedPolicy);
  if (conflict) return badRequest(conflict);

  try {
    const updateData = { ...policy.data };
    delete updateData.ownerRef;
    if (!updateData.key) delete updateData.key;
    if (updateData.billingMode === "credits") {
      updateData.byokProvider = null;
      updateData.byokCredentialsCiphertext = null;
      updateData.byokCredsEncVersion = null;
    }
    const updated = await updateApiKey(id, updateData);
    try {
      await appendAuditEvent({ actor: "admin-token", action: "api_key.updated", resourceType: "api_key", resourceId: id, ownerRef: owner.value, requestId: request.headers.get("x-request-id"), sourceIp: trustedClientIp(request), before: serializeAdminKey(existing), after: serializeAdminKey(updated) });
    } catch (error) {
      console.error("AUDIT_WRITE_FAILED", error?.message || error);
    }
    return NextResponse.json(serializeAdminKey(updated));
  } catch (error) {
    if (error?.code === "DUPLICATE_API_KEY") return NextResponse.json({ error: "API key already in use" }, { status: 409 });
    return NextResponse.json({ error: "Failed to update key" }, { status: 500 });
  }
}

export async function DELETE(request, { params }) {
  const unauthorized = await requireAdmin(request);
  if (unauthorized) return unauthorized;
  const owner = ownerFromQuery(request);
  if (owner.error) return badRequest(owner.error);
  const { id } = await params;
  const existing = await findOwnedKey(id, owner.value);
  if (!existing) return notFound();
    const deleted = await deleteApiKey(id);
    if (deleted) {
      try {
        await appendAuditEvent({ actor: "admin-token", action: "api_key.deleted", resourceType: "api_key", resourceId: id, ownerRef: owner.value, requestId: request.headers.get("x-request-id"), sourceIp: trustedClientIp(request), before: serializeAdminKey(existing) });
      } catch (error) {
        console.error("AUDIT_WRITE_FAILED", error?.message || error);
      }
    }
    return deleted ? NextResponse.json({ message: "Key deleted successfully" }) : notFound();
}
