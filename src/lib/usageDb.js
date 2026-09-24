// Shim → re-export from new SQLite-based DB layer (src/lib/db/)
export {
  statsEmitter, trackPendingRequest, getActiveRequests,
  saveRequestUsage, getUsageHistory, getApiKeyDailyUsage, getUsageStats, getChartData,
  appendRequestLog, getRecentLogs,
  flushAnalyticsBuffer, resetAnalyticsBuffer, getAnalyticsBufferSize, isAnalyticsBufferingEnabled,
  flushRequestDetailsBuffer,
  saveRequestDetail, getRequestDetails, getRequestDetailById,
} from "@/lib/db/index.js";
