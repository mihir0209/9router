import { describe, expect, it } from "vitest";
import { stripRequestUser } from "@/lib/requestPrivacy.js";

describe("stripRequestUser", () => {
  it("removes only the top-level user field when enabled", () => {
    const body = {
      model: "openai/gpt-5",
      user: "client-user-id",
      messages: [{ role: "user", content: "hello" }],
      metadata: { user_id: "session-id" },
    };

    const result = stripRequestUser(body, true);

    expect(result).not.toBe(body);
    expect(result.user).toBeUndefined();
    expect(result.messages).toEqual(body.messages);
    expect(result.metadata).toEqual({ user_id: "session-id" });
    expect(body.user).toBe("client-user-id");
  });

  it("returns the original body when disabled or no user field exists", () => {
    const withUser = { user: "client-user-id" };
    const withoutUser = { model: "openai/gpt-5" };

    expect(stripRequestUser(withUser, false)).toBe(withUser);
    expect(stripRequestUser(withoutUser, true)).toBe(withoutUser);
  });
});
