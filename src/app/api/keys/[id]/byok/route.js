import { NextResponse } from "next/server";
import { cookies } from "next/headers";
import { getApiKeyById, updateApiKey } from "@/lib/db/index.js";
import { getDashboardAuthSession } from "@/lib/auth/dashboardSession";
import { BYOK_ENVELOPE_VERSION } from "@/lib/byokCrypto.js";
import { AI_PROVIDERS, resolveProviderId } from "@/shared/constants/providers.js";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken.js";

async function requireDashboardSession() {
  if (process.env.NINEROUTER_RUNTIME !== "cloudflare") return null;
  const cookieStore = await cookies();
  const session = await getDashboardAuthSession(cookieStore.get("auth_token")?.value);
  return session?.authenticated ? null : NextResponse.json({ error: "Unauthorized" }, { status: 401 });
}

function validateEnvelope(raw) {
  if (typeof raw !== "string" || !raw.trim()) return "credentialsCiphertext must be a non-empty string";
  let env;
  try { env = JSON.parse(raw); } catch { return "credentialsCiphertext is not valid JSON"; }
  if (!env || env.v !== BYOK_ENVELOPE_VERSION || env.alg !== "A256GCM") return "invalid envelope version or algorithm";
  if (typeof env.iv !== "string" || typeof env.ciphertext !== "string" || typeof env.tag !== "string")
    return "envelope missing required fields (iv, ciphertext, tag)";
  return null;
}

// PUT /api/keys/[id]/byok - Replace or clear the key's BYOK credential.
// Accepts { provider, credentialsCiphertext } — never plaintext apiKey.
// credentialsCiphertext is a JSON AES-256-GCM envelope produced by byokCrypto.js.
export async function PUT(request, { params }) {
  try {
    const unauthorized = await hasValidAdminApiToken(request) ? null : await requireDashboardSession();
    if (unauthorized) return unauthorized;

    const { id } = await params;
    const existing = await getApiKeyById(id);
    if (!existing) return NextResponse.json({ error: "Key not found" }, { status: 404 });

    const body = await request.json();

    // Clear path: credentialsCiphertext is null → restore credits mode
    if (body.credentialsCiphertext === null || body.credentialsCiphertext === undefined) {
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

    const provider = resolveProviderId(body.provider.trim());
    if (!AI_PROVIDERS[provider]) {
      return NextResponse.json({ error: "unknown provider" }, { status: 400 });
    }

    const envError = validateEnvelope(body.credentialsCiphertext);
    if (envError) {
      return NextResponse.json({ error: envError }, { status: 400 });
    }

    const updated = await updateApiKey(id, {
      billingMode: "byok",
      byokProvider: provider,
      byokCredsEncVersion: BYOK_ENVELOPE_VERSION,
      byokCredentialsCiphertext: body.credentialsCiphertext,
    });

    return NextResponse.json({ billingMode: updated.billingMode });
  } catch (error) {
    const status = /BYOK_ENC_KEY/.test(error.message) ? 503 : 500;
    return NextResponse.json({
      error: status === 503 ? error.message : "Failed to store BYOK credential",
    }, { status });
  }
}
