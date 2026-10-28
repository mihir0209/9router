import { NextResponse } from "next/server";
import { deleteApiKey, getApiKeyById, getApiKeyByKey, updateApiKey } from "@/lib/localDb";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken";
import { serializeApiKey } from "@/lib/apiKeys/serializers";
import { getBudgetState } from "@/lib/budgetService.js";
import { validateForcePins, forcePairChanged } from "@/lib/apiKeys/forcePins";
import { findForcePairMismatch } from "@/lib/apiKeys/forcePair";

function normalizeList(value) {
  return Array.isArray(value) ? value.join(",") : value;
}

function validatePolicy(data) {
  const nonNegative = ["rateLimitRequests", "maxConcurrentRequests", "tokenLimitDaily", "tokenLimitMonthly", "budgetUsd"];
  const positive = ["rateLimitWindowSeconds", "budgetResetHours"];
  for (const field of nonNegative) {
    if (data[field] != null && (!Number.isFinite(Number(data[field])) || Number(data[field]) < 0)) {
      return `${field} must be a non-negative number`;
    }
  }
  for (const field of positive) {
    if (data[field] != null && (!Number.isFinite(Number(data[field])) || Number(data[field]) <= 0)) {
      return `${field} must be a positive number`;
    }
  }
  if (data.budgetWindowStartedAt != null && Number.isNaN(new Date(data.budgetWindowStartedAt).getTime())) {
    return "budgetWindowStartedAt must be a valid date";
  }
  return null;
}

// GET /api/keys/[id] - Get single key
export async function GET(request, { params }) {
  try {
    const { id } = await params;
    const key = await getApiKeyById(id);
    if (!key) {
      return NextResponse.json({ error: "Key not found" }, { status: 404 });
    }
    const isAdminApi = await hasValidAdminApiToken(request);
    const serialized = serializeApiKey(key);
    if (isAdminApi) {
      const budget = await getBudgetState(key.key);
      return NextResponse.json({
        ...serialized,
        budgetRemainingUsd: budget.remaining,
        budgetResetAt: budget.resetsAt,
        budgetSpendWindowUsd: budget.used,
      });
    }
    return NextResponse.json({ key: serialized });
  } catch (error) {
    console.log("Error fetching key:", error);
    return NextResponse.json({ error: "Failed to fetch key" }, { status: 500 });
  }
}

// PUT /api/keys/[id] - Update key
export async function PUT(request, { params }) {
  try {
    const { id } = await params;
    const body = await request.json();

    const existing = await getApiKeyById(id);
    if (!existing) {
      return NextResponse.json({ error: "Key not found" }, { status: 404 });
    }

    const updateData = {};
    const allowedFields = [
      "name", "isActive", "rateLimitRequests", "rateLimitWindowSeconds",
      "tokenLimitDaily", "tokenLimitMonthly",
      "allowedModels", "blockedModels",
      "allowedIPs", "blockedIPs",
      "expiresAt", "description",
      "budgetUsd", "budgetResetHours", "budgetWindowStartedAt", "billingMode",
      "maxConcurrentRequests", "allowedProviders", "blockedProviders",
      "forceProvider", "forceModel",
      "allowedEndpoints", "blockedEndpoints", "metadata",
    ];
    for (const field of allowedFields) {
      if (body[field] !== undefined) {
        updateData[field] = body[field];
      }
    }
    // Custom key value (e.g. register an existing license key). Empty means
    // "keep the current key" — only a non-empty value replaces it.
    if (body.key !== undefined && body.key !== null && String(body.key).trim() !== "") {
      const customKey = String(body.key).trim();
      if (customKey.length > 512) {
        return NextResponse.json({ error: "Custom key must be 512 characters or fewer" }, { status: 400 });
      }
      const clash = await getApiKeyByKey(customKey);
      if (clash && clash.id !== id) {
        return NextResponse.json({ error: "API key already in use" }, { status: 409 });
      }
      updateData.key = customKey;
    }
    for (const field of ["allowedModels", "blockedModels", "allowedIPs", "blockedIPs", "allowedProviders", "blockedProviders", "allowedEndpoints", "blockedEndpoints"]) {
      if (updateData[field] !== undefined) updateData[field] = normalizeList(updateData[field]);
    }
    if (updateData.billingMode && !["credits", "byok"].includes(updateData.billingMode)) {
      return NextResponse.json({ error: "Invalid billingMode" }, { status: 400 });
    }
    const validationError = validatePolicy(updateData);
    if (validationError) return NextResponse.json({ error: validationError }, { status: 400 });
    // Validate pins against the post-update state: a supplied value wins over
    // the stored one, so a pin is also re-checked when only a blocked list moves.
    const pinFieldsTouched = ["forceProvider", "forceModel", "blockedModels", "blockedProviders"].some((field) => updateData[field] !== undefined);
    if (pinFieldsTouched) {
      const pins = validateForcePins(updateData, {
        forceProvider: updateData.forceProvider !== undefined ? updateData.forceProvider : existing.forceProvider,
        forceModel: updateData.forceModel !== undefined ? updateData.forceModel : existing.forceModel,
        blockedModels: updateData.blockedModels !== undefined ? updateData.blockedModels : existing.blockedModels,
        blockedProviders: updateData.blockedProviders !== undefined ? updateData.blockedProviders : existing.blockedProviders,
      }, existing);
      if (pins.error) return NextResponse.json({ error: pins.error }, { status: 400 });
      Object.assign(updateData, pins.values);
      // An unchanged pair is not re-checked, so a key that already holds a
      // stale pair can still be edited (and fixed).
      if (forcePairChanged(pins.values, existing)) {
        const pairError = await findForcePairMismatch({
          forceProvider: pins.values.forceProvider !== undefined ? pins.values.forceProvider : existing.forceProvider,
          forceModel: pins.values.forceModel !== undefined ? pins.values.forceModel : existing.forceModel,
        });
        if (pairError) return NextResponse.json({ error: pairError }, { status: 400 });
      }
    }
    if (updateData.billingMode === "byok" && !existing.byokCredentialsCiphertext) {
      return NextResponse.json({ error: "Configure BYOK credentials before enabling byok billing" }, { status: 400 });
    }
    if (updateData.billingMode === "credits") {
      Object.assign(updateData, {
        byokProvider: null,
        byokCredentialsCiphertext: null,
        byokCredsEncVersion: null,
      });
    }

    const updated = await updateApiKey(id, updateData);
    const isAdminApi = await hasValidAdminApiToken(request);
    const serialized = serializeApiKey(updated);
    return NextResponse.json(isAdminApi ? serialized : { key: serialized });
  } catch (error) {
    if (error?.code === "DUPLICATE_API_KEY") {
      return NextResponse.json({ error: "API key already in use" }, { status: 409 });
    }
    console.log("Error updating key:", error);
    return NextResponse.json({ error: "Failed to update key" }, { status: 500 });
  }
}

// DELETE /api/keys/[id] - Delete API key
export async function DELETE(request, { params }) {
  try {
    const { id } = await params;
    const deleted = await deleteApiKey(id);
    if (!deleted) {
      return NextResponse.json({ error: "Key not found" }, { status: 404 });
    }
    return NextResponse.json({ message: "Key deleted successfully" });
  } catch (error) {
    console.log("Error deleting key:", error);
    return NextResponse.json({ error: "Failed to delete key" }, { status: 500 });
  }
}
