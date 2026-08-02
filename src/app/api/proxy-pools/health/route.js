import { NextResponse } from "next/server";
import {
  runImmediateHealthCheck,
  runImmediateCleanup,
  getHealthMonitorStatus,
  getProxyHealthStats,
  resetProxyHealthStats,
} from "@/lib/proxy/healthMonitor";

export async function GET(req) {
  try {
    const { searchParams } = new URL(req.url);
    const proxyId = searchParams.get("proxyId");

    if (proxyId) {
      const stats = getProxyHealthStats(proxyId);
      return NextResponse.json({ proxyId, stats });
    }

    const status = getHealthMonitorStatus();
    return NextResponse.json(status);
  } catch (err) {
    return NextResponse.json({ error: err.message }, { status: 500 });
  }
}

export async function POST(req) {
  try {
    const body = await req.json();

    if (body.action === "run") {
      const result = await runImmediateHealthCheck();
      return NextResponse.json(result);
    }

    if (body.action === "cleanup") {
      const immediate = body.immediate === true;
      const result = await runImmediateCleanup(immediate);
      return NextResponse.json(result);
    }

    if (body.action === "reset" && body.proxyId) {
      resetProxyHealthStats(body.proxyId);
      return NextResponse.json({ ok: true });
    }

    return NextResponse.json({ error: "Invalid action" }, { status: 400 });
  } catch (err) {
    return NextResponse.json({ error: err.message }, { status: 500 });
  }
}
