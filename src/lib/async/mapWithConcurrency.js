/**
 * Map async work with a fixed concurrency cap while preserving input order.
 * Rejected and timed-out tasks resolve to the caller-provided fallback.
 */
export async function mapWithConcurrency(
  items,
  mapper,
  { concurrency = 4, timeoutMs = 5000, onError = () => undefined } = {},
) {
  if (!Array.isArray(items) || items.length === 0) return [];

  const workerCount = Math.min(items.length, Math.max(1, Math.floor(concurrency)));
  const results = new Array(items.length);
  let nextIndex = 0;

  async function worker() {
    while (nextIndex < items.length) {
      const index = nextIndex++;
      let timeoutId;

      try {
        const controller = new AbortController();
        const task = Promise.resolve().then(() => mapper(items[index], index, controller.signal));
        const timeout = new Promise((_, reject) => {
          timeoutId = setTimeout(() => {
            const error = new Error(`Async task timed out after ${timeoutMs}ms`);
            error.name = "TimeoutError";
            controller.abort(error);
            reject(error);
          }, timeoutMs);
        });
        results[index] = await Promise.race([task, timeout]);
      } catch (error) {
        results[index] = await onError(error, items[index], index);
      } finally {
        clearTimeout(timeoutId);
      }
    }
  }

  await Promise.all(Array.from({ length: workerCount }, () => worker()));
  return results;
}
