import { NextResponse } from "next/server";
import { cookies } from "next/headers";
import { regenerateApiKey } from "@/lib/db/index.js";
import { serializeApiKey } from "@/lib/apiKeys/serializers";
import { getDashboardAuthSession } from "@/lib/auth/dashboardSession";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken";

async function requireDashboardSession() {
  if (process.env.NINEROUTER_RUNTIME !== "cloudflare") return null;
  const cookieStore = await cookies();
  const session = await getDashboardAuthSession(cookieStore.get("auth_token")?.value);
  return session?.authenticated ? null : NextResponse.json({ error: "Unauthorized" }, { status: 401 });
}

// PUT /api/keys/[id]/regenerate - Revoke the current secret and mint a fresh one.
// Returns the new secret exactly once, so it is gated behind the same
// admin-token-or-dashboard-session check as the other sensitive endpoints.
export async function PUT(request, { params }) {
  try {
    const unauthorized = await hasValidAdminApiToken(request) ? null : await requireDashboardSession();
    if (unauthorized) return unauthorized;

    const { id } = await params;
    const key = await regenerateApiKey(id);

    if (!key) {
      return NextResponse.json({ error: "Key not found" }, { status: 404 });
    }

    return NextResponse.json(serializeApiKey(key, { includeSecret: true }));
  } catch (error) {
    console.error("Error regenerating key:", error);
    return NextResponse.json({ error: "Failed to regenerate key" }, { status: 500 });
  }
}
