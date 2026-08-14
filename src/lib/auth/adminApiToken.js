import crypto from "node:crypto";
import { verifyDashboardPassword } from "@/lib/auth/dashboardSession";

const DEFAULT_PASSWORD = "123456";

function constantTimeEqual(left, right) {
  const leftDigest = crypto.createHash("sha256").update(left).digest();
  const rightDigest = crypto.createHash("sha256").update(right).digest();
  return crypto.timingSafeEqual(leftDigest, rightDigest);
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
