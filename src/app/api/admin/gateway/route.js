import { NextResponse } from "next/server";
import { getSettings, updateSettings } from "@/lib/localDb";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken";
import { appendAuditEvent } from "@/lib/db/repos/auditRepo.js";
import { trustedClientIp } from "@/lib/auth/clientIp.js";

function sanitizeControl(input, current) {
  const control = {
    disabled: input.disabled === undefined ? current.disabled === true : input.disabled === true,
    reason: input.reason === undefined ? String(current.reason || "") : String(input.reason || "").trim().slice(0, 500),
    drain: input.drain === undefined ? current.drain === true : input.drain === true,
    blockedProviders: input.blockedProviders === undefined ? current.blockedProviders || [] : input.blockedProviders,
    blockedModels: input.blockedModels === undefined ? current.blockedModels || [] : input.blockedModels,
  };
  if (!Array.isArray(control.blockedProviders) || control.blockedProviders.some((item) => typeof item !== "string")) return { error: "blockedProviders must be a string array" };
  if (!Array.isArray(control.blockedModels) || control.blockedModels.some((item) => typeof item !== "string")) return { error: "blockedModels must be a string array" };
  if (control.reason.length > 500) return { error: "reason must be 500 characters or fewer" };
  return { control };
}

async function requireAdmin(request) {
  return await hasValidAdminApiToken(request) ? null : NextResponse.json({ error: "Unauthorized" }, { status: 401 });
}

export async function GET(request) {
  const unauthorized = await requireAdmin(request);
  if (unauthorized) return unauthorized;
  const settings = await getSettings();
  return NextResponse.json({ gatewayControl: settings.gatewayControl || { disabled: false, reason: "", drain: false, blockedProviders: [], blockedModels: [] } });
}

export async function PUT(request) {
  const unauthorized = await requireAdmin(request);
  if (unauthorized) return unauthorized;
  let body;
  try { body = await request.json(); } catch { return NextResponse.json({ error: "Request body must be valid JSON" }, { status: 400 }); }
  const settings = await getSettings();
  const result = sanitizeControl(body, settings.gatewayControl || {});
  if (result.error) return NextResponse.json({ error: result.error }, { status: 400 });
  const before = settings.gatewayControl || {};
  await updateSettings({ gatewayControl: result.control });
  try {
    await appendAuditEvent({ actor: "admin-token", action: "gateway_control.updated", resourceType: "gateway", resourceId: "global", requestId: request.headers.get("x-request-id"), sourceIp: trustedClientIp(request), before, after: result.control });
  } catch (error) {
    console.error("AUDIT_WRITE_FAILED", error?.message || error);
  }
  return NextResponse.json({ gatewayControl: result.control });
}
