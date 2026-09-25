import { verifyDashboardPassword } from "@/lib/auth/dashboardSession";

const DEFAULT_PASSWORD = "123456";

function constantTimeEqual(left, right) {
  const length = Math.max(left.length, right.length);
  let diff = left.length ^ right.length;
  for (let index = 0; index < length; index++) {
    diff |= (left.charCodeAt(index % Math.max(left.length, 1)) || 0) ^ (right.charCodeAt(index % Math.max(right.length, 1)) || 0);
  }
  return diff === 0;
}

export async function hasValidAdminApiToken(request) {
  const configuredToken = process.env.ADMIN_API_TOKEN;
  if (!configuredToken || await verifyDashboardPassword(DEFAULT_PASSWORD)) return false;

  const authorization = request.headers.get("authorization") || "";
  if (!authorization.startsWith("Bearer ")) return false;
  const suppliedToken = authorization.slice(7);
  return suppliedToken.length > 0 && constantTimeEqual(suppliedToken, configuredToken);
}

export const __test__ = { constantTimeEqual };
