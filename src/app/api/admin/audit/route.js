import { NextResponse } from "next/server";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken";
import { listAuditEvents } from "@/lib/db/repos/auditRepo.js";

export async function GET(request) {
  if (!await hasValidAdminApiToken(request)) return NextResponse.json({ error: "Unauthorized" }, { status: 401 });
  const params = new URL(request.url).searchParams;
  const ownerRef = params.get("ownerRef");
  if (!ownerRef) return NextResponse.json({ error: "ownerRef is required" }, { status: 400 });
  const action = params.get("action") || undefined;
  const limit = Number(params.get("limit") || 100);
  return NextResponse.json({ events: await listAuditEvents({ ownerRef, action, limit }) });
}
