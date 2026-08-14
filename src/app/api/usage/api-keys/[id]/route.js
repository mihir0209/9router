import { NextResponse } from "next/server";
import { getApiKeyById, getApiKeyDailyUsage } from "@/lib/db/index.js";

function parseBoundary(value, endOfDay = false) {
  if (!value) return null;
  const dateOnly = /^\d{4}-\d{2}-\d{2}$/.test(value);
  const date = new Date(dateOnly && endOfDay ? `${value}T23:59:59.999Z` : value);
  if (Number.isNaN(date.getTime())) throw new TypeError("Invalid date range");
  return date.toISOString();
}

export async function GET(request, { params }) {
  try {
    const { id } = await params;
    const key = await getApiKeyById(id);
    if (!key) {
      return NextResponse.json({ error: "Key not found" }, { status: 404 });
    }

    const { searchParams } = new URL(request.url);
    const from = parseBoundary(searchParams.get("from"));
    const to = parseBoundary(searchParams.get("to"), true);
    if (from && to && from > to) {
      return NextResponse.json({ error: "from must be before or equal to to" }, { status: 400 });
    }

    const usage = await getApiKeyDailyUsage(key.key, { from, to });
    const rows = usage.map((row) => ({
      day: row.date,
      requests: row.requests,
      tokensIn: row.promptTokens,
      tokensOut: row.completionTokens,
      costUsd: row.cost,
    }));
    return NextResponse.json({ keyId: id, from, to, rows });
  } catch (error) {
    if (error instanceof TypeError) {
      return NextResponse.json({ error: error.message }, { status: 400 });
    }
    console.error("[API] Failed to get API key usage:", error);
    return NextResponse.json({ error: "Failed to fetch API key usage" }, { status: 500 });
  }
}
