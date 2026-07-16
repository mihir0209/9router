import { NextResponse } from "next/server";

export default async function proxy(request) {
  if (process.env.NINEROUTER_RUNTIME === "cloudflare") {
    return NextResponse.next();
  }
  const { proxy: dashboardProxy } = await import("./dashboardGuard");
  return dashboardProxy(request);
}

export const config = {
  matcher: ["/((?!_next/static|_next/image|favicon\\.ico).*)"],
};
