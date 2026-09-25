import { NextResponse } from "next/server";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken";
import { getAlerts } from "@/lib/observability/metrics.js";

export async function GET(request) {
  if (!await hasValidAdminApiToken(request)) return NextResponse.json({ error: "Unauthorized" }, { status: 401 });
  return NextResponse.json({ alerts: await getAlerts() });
}
