import { NextResponse } from "next/server";
import { jwtVerify } from "jose";

const PUBLIC_CLOUDFLARE_PATHS = [
  "/api/health",
  "/api/auth/login",
  "/api/auth/logout",
  "/api/auth/status",
  "/api/auth/oidc/callback",
  "/api/settings/require-login",
  "/api/version",
  "/callback",
  "/login",
  "/landing",
  "/manifest.webmanifest",
];

const PUBLIC_LLM_PREFIXES = ["/v1", "/v1beta", "/api/v1", "/api/v1beta"];

function pathMatches(pathname, path) {
  return pathname === path || pathname.startsWith(`${path}/`);
}

function isPublicCloudflarePath(pathname) {
  return PUBLIC_CLOUDFLARE_PATHS.some((path) => pathMatches(pathname, path));
}

function isPublicLlmApi(pathname) {
  return PUBLIC_LLM_PREFIXES.some((path) => pathMatches(pathname, path));
}

function hasApiKeyHeader(request) {
  const authHeader = request.headers.get("authorization");
  return Boolean(
    authHeader?.startsWith("Bearer ") ||
    request.headers.get("x-api-key") ||
    request.headers.get("x-goog-api-key") ||
    request.nextUrl.searchParams?.get("key")
  );
}

async function hasValidDashboardCookie(request) {
  const token = request.cookies.get("auth_token")?.value;
  const secret = process.env.JWT_SECRET;
  if (!token || !secret) return false;
  try {
    const { payload } = await jwtVerify(token, new TextEncoder().encode(secret));
    return payload?.authenticated === true;
  } catch {
    return false;
  }
}

export default async function proxy(request) {
  const { pathname } = request.nextUrl;

  if (pathname === "/") {
    return NextResponse.redirect(new URL("/dashboard", request.url));
  }

  if (isPublicCloudflarePath(pathname)) {
    return NextResponse.next();
  }

  if (isPublicLlmApi(pathname)) {
    if (hasApiKeyHeader(request)) return NextResponse.next();
    return NextResponse.json({ error: "API key required" }, { status: 401 });
  }

  if (pathname.startsWith("/api/") || pathname.startsWith("/dashboard")) {
    if (await hasValidDashboardCookie(request)) return NextResponse.next();
    if (pathname.startsWith("/dashboard")) {
      return NextResponse.redirect(new URL("/login", request.url));
    }
    return NextResponse.json({ error: "Unauthorized" }, { status: 401 });
  }

  return NextResponse.next();
}

export const config = {
  matcher: ["/((?!_next/static|_next/image|favicon\\.ico).*)"],
};
