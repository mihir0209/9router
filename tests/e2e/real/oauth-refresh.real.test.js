import { describe, expect, it } from "vitest";
import { checkAndRefreshToken } from "../../../src/sse/services/tokenRefresh.js";
import {
  REAL_TIMEOUT_MS,
  RUN_REAL,
  invokeChat,
  observedRefreshDecision,
  oauthInventory,
  prepareTarget,
  requireChatSuccess,
} from "./_helpers.js";

const suite = RUN_REAL ? describe : describe.skip;
const FORCE_REFRESH = process.env.E2E_FORCE_REFRESH === "1";

suite("REAL OAuth inventory and refresh", () => {
  it("reports a read-only refresh decision and inventory", async (context) => {
    const inventory = await oauthInventory();
    const active = inventory.filter((entry) => entry.isActive);
    if (!active.length) context.skip("no active OAuth credentials in the local database");
    expect(active.every((entry) => entry.provider && entry.authType)).toBe(true);
    const providers = [...new Set(active.map((entry) => entry.provider))];
    const target = await prepareTarget({ providers, requiredCapabilities: {}, refreshCredentials: false });
    if (!target) context.skip("no OAuth provider has an eligible chat model and usable credential");
    expect(["observe-without-forcing", "present", "expired"]).toContain(observedRefreshDecision(target.credentials));
    expect(target.credentials.connectionId).toBeTruthy();
  }, REAL_TIMEOUT_MS);

  it("forces OAuth refresh only with the second gate", async (context) => {
    if (!FORCE_REFRESH) context.skip("set E2E_FORCE_REFRESH=1 with RUN_REAL=1 to force refresh");
    const inventory = await oauthInventory();
    const providers = [...new Set(inventory.filter((entry) => entry.isActive).map((entry) => entry.provider))];
    const target = await prepareTarget({ providers, requiredCapabilities: {}, refreshCredentials: false });
    if (!target) context.skip("no OAuth provider has an eligible chat model and usable credential");
    const refreshed = await checkAndRefreshToken(target.provider, target.credentials, { force: true });
    target.refreshedCredentials = refreshed;
    const result = await invokeChat(target, {
      stream: false,
      max_tokens: 32,
      messages: [{ role: "user", content: "Reply exactly REFRESH_OK." }],
    });
    requireChatSuccess(context, target, result);
    expect(result.json.choices?.[0]?.message?.content || "").toContain("REFRESH_OK");
  }, REAL_TIMEOUT_MS);
});
