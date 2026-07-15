import { NextResponse } from "next/server";
import { getModelDisplayNames, setModelDisplayName, deleteModelDisplayName } from "@/lib/localDb";

export const dynamic = "force-dynamic";

// GET /api/models/display-names - List all model display names
export async function GET() {
  try {
    const displayNames = await getModelDisplayNames();
    return NextResponse.json({ displayNames });
  } catch (error) {
    console.error("[API] Failed to get display names:", error);
    return NextResponse.json({ error: "Failed to fetch display names" }, { status: 500 });
  }
}

// PUT /api/models/display-names - Set a model display name
export async function PUT(request) {
  try {
    const body = await request.json();
    const { modelId, displayName } = body;

    if (!modelId || typeof modelId !== "string") {
      return NextResponse.json({ error: "modelId is required" }, { status: 400 });
    }

    // Validate modelId characters (alphanumeric, hyphens, underscores, dots, slashes)
    if (!/^[a-zA-Z0-9._\-\/]+$/.test(modelId)) {
      return NextResponse.json({ error: "modelId contains invalid characters" }, { status: 400 });
    }

    if (!displayName || typeof displayName !== "string") {
      return NextResponse.json({ error: "displayName is required" }, { status: 400 });
    }

    // Validate displayName (no slashes, reasonable length)
    if (displayName.includes("/")) {
      return NextResponse.json({ error: "displayName cannot contain slashes" }, { status: 400 });
    }

    if (displayName.length > 100) {
      return NextResponse.json({ error: "displayName too long (max 100 chars)" }, { status: 400 });
    }

    await setModelDisplayName(modelId, displayName.trim());
    return NextResponse.json({ success: true });
  } catch (error) {
    console.error("[API] Failed to set display name:", error);
    return NextResponse.json({ error: "Failed to set display name" }, { status: 500 });
  }
}

// DELETE /api/models/display-names - Delete a model display name
export async function DELETE(request) {
  try {
    const { searchParams } = new URL(request.url);
    const modelId = searchParams.get("modelId");

    if (!modelId) {
      return NextResponse.json({ error: "modelId parameter required" }, { status: 400 });
    }

    await deleteModelDisplayName(modelId);
    return NextResponse.json({ success: true });
  } catch (error) {
    console.error("[API] Failed to delete display name:", error);
    return NextResponse.json({ error: "Failed to delete display name" }, { status: 500 });
  }
}
