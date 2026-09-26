import { NextResponse } from "next/server";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken";
import { getMetricsSnapshot, prometheusMetrics } from "@/lib/observability/metrics.js";

// ?tenant=<ownerRef|keyId> narrows the view to one tenant; omit it for the
// cluster aggregate. Requires the admin token either way.
export async function GET(request) {
  if (!await hasValidAdminApiToken(request)) return NextResponse.json({ error: "Unauthorized" }, { status: 401 });
  const params = new URL(request.url).searchParams;
  const raw = params.get("tenant");
  const tenant = raw && raw.trim() ? raw.trim() : null;
  const snapshot = await getMetricsSnapshot(tenant);
  const format = params.get("format");
  if (format === "prometheus") return new Response(prometheusMetrics(snapshot), { headers: { "Content-Type": "text/plain; version=0.0.4; charset=utf-8" } });
  return NextResponse.json(snapshot);
}
