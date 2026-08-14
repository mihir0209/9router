import { NextResponse } from "next/server";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken.js";
import { getApiKeyById } from "@/lib/db/index.js";
import { debitBudget } from "@/lib/budgetService.js";

export const dynamic = "force-dynamic";

export async function POST(request) {
  if (!await hasValidAdminApiToken(request)) {
    return NextResponse.json({ error: "Unauthorized" }, { status: 401 });
  }

  try {
    const { keyId, requestId, reserveUsd, commitUsd } = await request.json();
    if (!keyId || typeof requestId !== "string" || !requestId.trim()) {
      return NextResponse.json({ error: "keyId and requestId are required" }, { status: 400 });
    }

    const keyRecord = await getApiKeyById(keyId);
    if (!keyRecord) return NextResponse.json({ error: "API key not found" }, { status: 404 });

    const budget = await debitBudget(keyRecord.key, { requestId, reserveUsd, commitUsd });
    if (budget.reserved === false || budget.committed === false) {
      return NextResponse.json({ error: "Debit exceeds remaining budget", budget }, { status: 402 });
    }
    return NextResponse.json({ budget });
  } catch (error) {
    return NextResponse.json({ error: error.message }, { status: 400 });
  }
}
