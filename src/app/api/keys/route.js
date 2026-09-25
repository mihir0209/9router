import { NextResponse } from "next/server";
import { getApiKeys, getApiKeyByKey, createApiKey } from "@/lib/localDb";
import { getConsistentMachineId } from "@/shared/utils/machineId";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken";
import { serializeApiKey, serializeApiKeys } from "@/lib/apiKeys/serializers";

export const dynamic = "force-dynamic";

const CREATE_FIELDS = [
  "ownerRef", "rateLimitRequests", "rateLimitWindowSeconds",
  "tokenLimitDaily", "tokenLimitMonthly", "allowedModels", "blockedModels",
  "allowedIPs", "blockedIPs", "expiresAt", "description",
  "budgetUsd", "budgetResetHours", "budgetWindowStartedAt", "billingMode",
  "maxConcurrentRequests", "allowedProviders", "blockedProviders",
  "allowedEndpoints", "blockedEndpoints", "metadata",
];

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

// GET /api/keys - List API keys
export async function GET(request) {
  try {
    const ownerRef = new URL(request.url).searchParams.get("ownerRef");
    const keys = await getApiKeys(ownerRef);
    return NextResponse.json({
      keys: serializeApiKeys(keys),
    });
  } catch (error) {
    console.log("Error fetching keys:", error);
    return NextResponse.json({ error: "Failed to fetch keys" }, { status: 500 });
  }
}

// POST /api/keys - Create new API key
export async function POST(request) {
  try {
    const body = await request.json();
    const { name, machineId: requestedMachineId, key: requestedKey } = body;
    const isAdminApi = await hasValidAdminApiToken(request);
    const options = {};
    for (const field of CREATE_FIELDS) {
      if (body[field] !== undefined) options[field] = body[field];
    }
    options.allowedModels = normalizeList(options.allowedModels);
    options.blockedModels = normalizeList(options.blockedModels);
    options.allowedIPs = normalizeList(options.allowedIPs);
    options.blockedIPs = normalizeList(options.blockedIPs);
    for (const field of ["allowedProviders", "blockedProviders", "allowedEndpoints", "blockedEndpoints"]) {
      options[field] = normalizeList(options[field]);
    }

    if (!name) {
      return NextResponse.json({ error: "Name is required" }, { status: 400 });
    }
    // Custom key value (e.g. an existing license key to register as an API
    // key). Empty/missing falls back to auto-generation.
    let customKey = null;
    if (requestedKey !== undefined && requestedKey !== null && String(requestedKey).trim() !== "") {
      customKey = String(requestedKey).trim();
      if (customKey.length > 512) {
        return NextResponse.json({ error: "Custom key must be 512 characters or fewer" }, { status: 400 });
      }
      if (await getApiKeyByKey(customKey)) {
        return NextResponse.json({ error: "API key already in use" }, { status: 409 });
      }
    }
    if (isAdminApi && !options.ownerRef) {
      return NextResponse.json({ error: "ownerRef is required" }, { status: 400 });
    }
    if (options.billingMode && !["credits", "byok"].includes(options.billingMode)) {
      return NextResponse.json({ error: "Invalid billingMode" }, { status: 400 });
    }
    if (options.billingMode === "byok") {
      return NextResponse.json({ error: "Configure BYOK credentials after creating the key" }, { status: 400 });
    }
    const validationError = validatePolicy(options);
    if (validationError) return NextResponse.json({ error: validationError }, { status: 400 });

    const machineId = (isAdminApi && requestedMachineId) || await getConsistentMachineId();
    const apiKey = await createApiKey(name, machineId, customKey ? { ...options, key: customKey } : options);

    return NextResponse.json(serializeApiKey(apiKey, { includeSecret: true }), { status: 201 });
  } catch (error) {
    if (error?.code === "DUPLICATE_API_KEY") {
      return NextResponse.json({ error: "API key already in use" }, { status: 409 });
    }
    console.log("Error creating key:", error);
    return NextResponse.json({ error: "Failed to create key" }, { status: 500 });
  }
}
