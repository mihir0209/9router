import { getApiKeyById } from "@/lib/db/index.js";
import { checkBudget } from "@/lib/budgetService.js";

const BUDGET_ERROR = "Budget exceeded for workspace key. Add credits to continue.";

export async function isApiKeyStillActive(id, key) {
  const current = await getApiKeyById(id);
  return current?.isActive === true && current.key === key;
}

export function guardApiKeyResponse(response, keyRecord, release) {
  let released = false;
  const releaseOnce = async () => {
    if (released) return;
    released = true;
    await release?.();
  };
  if (!response?.body || !keyRecord?.id || !keyRecord?.key) {
    void releaseOnce();
    return response;
  }
  const isEventStream = response.headers.get("content-type")?.includes("text/event-stream");
  const encoder = new TextEncoder();
  let lastPolicyCheck = 0;

  const guardedBody = response.body.pipeThrough(new TransformStream({
    async transform(chunk, controller) {
      if (!await isApiKeyStillActive(keyRecord.id, keyRecord.key)) {
        await releaseOnce();
        controller.terminate();
        return;
      }
      const now = Date.now();
      const shouldCheckBudget = now - lastPolicyCheck >= 1_000;
      if (shouldCheckBudget) lastPolicyCheck = now;
      const budget = shouldCheckBudget ? await checkBudget(keyRecord.key) : { exceeded: false };
      if (budget.exceeded) {
        if (isEventStream) {
          controller.enqueue(encoder.encode(`data: ${JSON.stringify({
            error: { message: BUDGET_ERROR, type: "budget_exceeded", param: null, code: 402 },
            choices: [{ delta: {}, finish_reason: "error", index: 0 }],
          })}\n\n`));
        }
        await releaseOnce();
        controller.terminate();
        return;
      }
      controller.enqueue(chunk);
    },
    async flush() { await releaseOnce(); },
    async cancel() { await releaseOnce(); },
  }));

  return new Response(guardedBody, {
    status: response.status,
    statusText: response.statusText,
    headers: response.headers,
  });
}
