import { NextResponse } from "next/server";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken";
import { getAlerts } from "@/lib/observability/metrics.js";

// ?tenant=<ownerRef|keyId> narrows the alerts to one tenant; omit it for all.
export async function GET(request) {
  if (!await hasValidAdminApiToken(request)) return NextResponse.json({ error: "Unauthorized" }, { status: 401 });
  const raw = new URL(request.url).searchParams.get("tenant");
  return NextResponse.json({ alerts: await getAlerts(raw && raw.trim() ? raw.trim() : null) });
}
