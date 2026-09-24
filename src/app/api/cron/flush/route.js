import { NextResponse } from "next/server";
import {
  flushAnalyticsBuffer,
  flushRequestDetailsBuffer,
  getAnalyticsBufferSize,
  isAnalyticsBufferingEnabled,
} from "@/lib/db/index.js";

function isAuthorized(request) {
  const secret = process.env.CRON_SECRET;
  if (!secret) return true;
  const header = request.headers.get("authorization") || "";
  const token = header.startsWith("Bearer ") ? header.slice(7) : request.headers.get("x-cron-secret") || "";
  return token === secret;
}

async function runFlush() {
  const before = getAnalyticsBufferSize();
  await flushAnalyticsBuffer();
  const detailsFlushed = await flushRequestDetailsBuffer().catch(() => 0);
  const after = getAnalyticsBufferSize();
  const buffering = await isAnalyticsBufferingEnabled().catch(() => false);
  return {
    ok: true,
    buffering,
    analytics: { before, after },
    detailsFlushed,
  };
}

export async function GET(request) {
  if (!isAuthorized(request)) {
    return NextResponse.json({ error: "Unauthorized" }, { status: 401 });
  }
  try {
    return NextResponse.json(await runFlush());
  } catch (error) {
    return NextResponse.json({ ok: false, error: String(error?.message || error) }, { status: 500 });
  }
}

export async function POST(request) {
  return GET(request);
}
