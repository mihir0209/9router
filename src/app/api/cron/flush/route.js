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
  return safeEqual(token, secret);
}

// Compare without an early exit so the response time does not reveal how much of a guess matched.
function safeEqual(a, b) {
  const left = String(a);
  const right = String(b);
  let diff = left.length ^ right.length;
  const length = Math.max(left.length, right.length);
  for (let i = 0; i < length; i++) diff |= (left.charCodeAt(i) || 0) ^ (right.charCodeAt(i) || 0);
  return diff === 0;
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
