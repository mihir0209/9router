import { getApiKeyById } from "@/lib/db/index.js";
import { checkBudget } from "@/lib/budgetService.js";

const BUDGET_ERROR = "Budget exceeded for workspace key. Add credits to continue.";

export async function isApiKeyStillActive(id, key) {
  const current = await getApiKeyById(id);
  return current?.isActive === true && current.key === key;
}

export function guardApiKeyResponse(response, keyRecord) {
  if (!response?.body || !keyRecord?.id || !keyRecord?.key) return response;
  const isEventStream = response.headers.get("content-type")?.includes("text/event-stream");
  const encoder = new TextEncoder();

  const guardedBody = response.body.pipeThrough(new TransformStream({
    async transform(chunk, controller) {
      if (!await isApiKeyStillActive(keyRecord.id, keyRecord.key)) {
        controller.terminate();
        return;
      }
      const budget = await checkBudget(keyRecord.key);
      if (budget.exceeded) {
        if (isEventStream) {
          controller.enqueue(encoder.encode(`data: ${JSON.stringify({
            error: { message: BUDGET_ERROR, type: "budget_exceeded", param: null, code: 402 },
            choices: [{ delta: {}, finish_reason: "error", index: 0 }],
          })}\n\n`));
        }
        controller.terminate();
        return;
      }
      controller.enqueue(chunk);
    },
  }));

  return new Response(guardedBody, {
    status: response.status,
    statusText: response.statusText,
    headers: response.headers,
  });
}
