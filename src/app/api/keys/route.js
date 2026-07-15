import { NextResponse } from "next/server";
import { getApiKeys, createApiKey } from "@/lib/localDb";
import { getConsistentMachineId } from "@/shared/utils/machineId";

export const dynamic = "force-dynamic";

// GET /api/keys - List API keys
export async function GET() {
  try {
    const keys = await getApiKeys();
    return NextResponse.json({ keys });
  } catch (error) {
    console.log("Error fetching keys:", error);
    return NextResponse.json({ error: "Failed to fetch keys" }, { status: 500 });
  }
}

// POST /api/keys - Create new API key
export async function POST(request) {
  try {
    const body = await request.json();
    const { name, ...options } = body;

    if (!name) {
      return NextResponse.json({ error: "Name is required" }, { status: 400 });
    }

    const machineId = await getConsistentMachineId();
    const apiKey = await createApiKey(name, machineId, options);

    return NextResponse.json({
      key: apiKey.key,
      name: apiKey.name,
      id: apiKey.id,
      machineId: apiKey.machineId,
      rateLimitRequests: apiKey.rateLimitRequests,
      rateLimitWindowSeconds: apiKey.rateLimitWindowSeconds,
      tokenLimitDaily: apiKey.tokenLimitDaily,
      tokenLimitMonthly: apiKey.tokenLimitMonthly,
      allowedModels: apiKey.allowedModels,
      blockedModels: apiKey.blockedModels,
      allowedIPs: apiKey.allowedIPs,
      blockedIPs: apiKey.blockedIPs,
      expiresAt: apiKey.expiresAt,
      description: apiKey.description,
    }, { status: 201 });
  } catch (error) {
    console.log("Error creating key:", error);
    return NextResponse.json({ error: "Failed to create key" }, { status: 500 });
  }
}
