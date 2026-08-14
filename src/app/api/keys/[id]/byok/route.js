import { NextResponse } from "next/server";
import { cookies } from "next/headers";
import { getApiKeyById, updateApiKey } from "@/lib/db/index.js";
import { getDashboardAuthSession } from "@/lib/auth/dashboardSession";
import { BYOK_ENVELOPE_VERSION, encryptByokCredential } from "@/lib/byokCrypto.js";
import { AI_PROVIDERS, resolveProviderId } from "@/shared/constants/providers.js";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken";

async function requireDashboardSession() {
  if (process.env.NINEROUTER_RUNTIME !== "cloudflare") return null;
  const cookieStore = await cookies();
  const session = await getDashboardAuthSession(cookieStore.get("auth_token")?.value);
  return session?.authenticated ? null : NextResponse.json({ error: "Unauthorized" }, { status: 401 });
}

// PUT /api/keys/[id]/byok - Replace or clear the key's provider credential.
// The app never touches the raw credential after this call: WebClaw passes
// { provider, apiKey } here and the fork encrypts it at rest with BYOK_ENC_KEY.
export async function PUT(request, { params }) {
  try {
    const unauthorized = await hasValidAdminApiToken(request) ? null : await requireDashboardSession();
    if (unauthorized) return unauthorized;

    const { id } = await params;
    const existing = await getApiKeyById(id);
    if (!existing) return NextResponse.json({ error: "Key not found" }, { status: 404 });

    const body = await request.json();
    if (body.apiKey === null || body.apiKey === undefined) {
      await updateApiKey(id, {
        billingMode: "credits",
        byokProvider: null,
        byokCredsEncVersion: null,
        byokCredentialsCiphertext: null,
      });
      return NextResponse.json({ billingMode: "credits" });
    }
    if (typeof body.provider !== "string" || !body.provider.trim()) {
      return NextResponse.json({ error: "provider is required" }, { status: 400 });
    }
    if (typeof body.apiKey !== "string" || !body.apiKey.trim()) {
      return NextResponse.json({ error: "apiKey is required" }, { status: 400 });
    }

    const provider = resolveProviderId(body.provider.trim());
    if (!AI_PROVIDERS[provider]) {
      return NextResponse.json({ error: "unknown provider" }, { status: 400 });
    }

    const credentialsCiphertext = encryptByokCredential(body.apiKey.trim(), provider, BYOK_ENVELOPE_VERSION);
    const updated = await updateApiKey(id, {
      billingMode: "byok",
      byokProvider: provider,
      byokCredsEncVersion: BYOK_ENVELOPE_VERSION,
      byokCredentialsCiphertext: credentialsCiphertext,
    });

    return NextResponse.json({ billingMode: updated.billingMode, provider: updated.byokProvider });
  } catch (error) {
    const status = /BYOK_ENC_KEY/.test(error.message) ? 503 : 500;
    return NextResponse.json({
      error: status === 503 ? error.message : "Failed to store BYOK credential",
    }, { status });
  }
}
