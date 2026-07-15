import { NextResponse } from "next/server";
import { getSettings, updateSettings } from "@/lib/localDb";

export const dynamic = "force-dynamic";

// GET /api/settings/blocked-ips - List blocked IPs
export async function GET() {
  try {
    const settings = await getSettings();
    const blockedIPs = settings.autoBlockedIPs || [];
    return NextResponse.json({ blockedIPs });
  } catch (error) {
    console.error("[API] Failed to get blocked IPs:", error);
    return NextResponse.json({ error: "Failed to fetch blocked IPs" }, { status: 500 });
  }
}

// POST /api/settings/blocked-ips - Add IPs to block list
export async function POST(request) {
  try {
    const body = await request.json();
    const { ips } = body;

    if (!Array.isArray(ips)) {
      return NextResponse.json({ error: "ips must be an array" }, { status: 400 });
    }

    const settings = await getSettings();
    const blockedIPs = settings.autoBlockedIPs || [];

    for (const ip of ips) {
      if (typeof ip === "string" && ip.trim() && !blockedIPs.includes(ip.trim())) {
        blockedIPs.push(ip.trim());
      }
    }

    await updateSettings({ autoBlockedIPs: blockedIPs });
    return NextResponse.json({ blockedIPs });
  } catch (error) {
    console.error("[API] Failed to add blocked IPs:", error);
    return NextResponse.json({ error: "Failed to add blocked IPs" }, { status: 500 });
  }
}

// DELETE /api/settings/blocked-ips - Remove IPs from block list
export async function DELETE(request) {
  try {
    const { searchParams } = new URL(request.url);
    const ip = searchParams.get("ip");

    if (!ip) {
      return NextResponse.json({ error: "ip parameter required" }, { status: 400 });
    }

    const settings = await getSettings();
    let blockedIPs = settings.autoBlockedIPs || [];
    blockedIPs = blockedIPs.filter(i => i !== ip);

    await updateSettings({ autoBlockedIPs: blockedIPs });
    return NextResponse.json({ blockedIPs });
  } catch (error) {
    console.error("[API] Failed to remove blocked IP:", error);
    return NextResponse.json({ error: "Failed to remove blocked IP" }, { status: 500 });
  }
}
