import { NextResponse } from "next/server";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken";
import { prometheusMetrics, snapshotMetrics } from "@/lib/observability/metrics.js";

export async function GET(request) {
  if (!await hasValidAdminApiToken(request)) return NextResponse.json({ error: "Unauthorized" }, { status: 401 });
  const format = new URL(request.url).searchParams.get("format");
  if (format === "prometheus") return new Response(prometheusMetrics(), { headers: { "Content-Type": "text/plain; version=0.0.4; charset=utf-8" } });
  return NextResponse.json(snapshotMetrics());
}
