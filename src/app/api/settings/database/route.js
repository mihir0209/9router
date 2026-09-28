import { NextResponse } from "next/server";
import { exportDb, getSettings, importDb } from "@/lib/localDb";
import { applyOutboundProxyEnv } from "@/lib/network/outboundProxy";
import { verifyDashboardPassword } from "@/lib/auth/dashboardSession";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken";

const CLI_TOKEN_HEADER = "x-9r-cli-token";
const PASSWORD_HEADER = "x-9r-password";

// CLI token requests are already trusted (local machine); skip password re-auth.
function isCliRequest(request) {
  if (process.env.NINEROUTER_RUNTIME === "cloudflare") return false;
  return Boolean(request.headers.get(CLI_TOKEN_HEADER));
}

/**
 * Whether this request may read or replace the whole database.
 *
 * The admin API token is accepted as well as the dashboard password. A hosted
 * deployment has no CLI token to present — that is derived from the machine
 * identity, which a container does not have — so on Antideploy the password was
 * the only way in, and automation had no route at all. ADMIN_API_TOKEN exists
 * for exactly this kind of operator action.
 *
 * Note for whoever hits a failure here: a restore that does not authorise comes
 * back 401 and changes nothing. It cannot half-apply.
 */
async function isAuthorized(request, password) {
  if (isCliRequest(request)) return true;
  if (await hasValidAdminApiToken(request)) return true;
  return verifyDashboardPassword(password);
}

function preserveProtectedSettings(importedSettings, currentSettings) {
  if (!importedSettings || typeof importedSettings !== "object") return importedSettings;
  const next = { ...importedSettings };

  if (currentSettings?.password) next.password = currentSettings.password;
  else delete next.password;

  if (currentSettings?.oidcClientSecret) next.oidcClientSecret = currentSettings.oidcClientSecret;
  else delete next.oidcClientSecret;

  return next;
}

export async function GET(request) {
  try {
    if (!(await isAuthorized(request, request.headers.get(PASSWORD_HEADER)))) {
      return NextResponse.json({ error: "Invalid password or admin token" }, { status: 401 });
    }
    const payload = await exportDb();
    return NextResponse.json(payload);
  } catch (error) {
    console.log("Error exporting database:", error);
    return NextResponse.json({ error: "Failed to export database" }, { status: 500 });
  }
}

export async function POST(request) {
  try {
    const { password, ...payload } = await request.json();
    const currentSettings = await getSettings();
    if (!(await isAuthorized(request, password))) {
      return NextResponse.json({ error: "Invalid password or admin token" }, { status: 401 });
    }

    payload.settings = preserveProtectedSettings(payload.settings, currentSettings);
    await importDb(payload);

    // Ensure proxy settings take effect immediately after a DB import.
    try {
      const settings = await getSettings();
      applyOutboundProxyEnv(settings);
    } catch (err) {
      console.warn("[Settings][DatabaseImport] Failed to re-apply outbound proxy env:", err);
    }

    return NextResponse.json({ success: true });
  } catch (error) {
    console.log("Error importing database:", error);
    return NextResponse.json(
      { error: error?.message || "Failed to import database" },
      { status: 400 }
    );
  }
}
