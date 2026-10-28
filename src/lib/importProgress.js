// Pure helpers for the dashboard's Import / Merge Backup progress display.
// Kept free of React and browser APIs so they can be unit tested directly.

export const IMPORT_STAGES = {
  reading: "reading",
  validating: "validating",
  loadingCurrent: "loadingCurrent",
  merging: "merging",
  uploading: "uploading",
  restoring: "restoring",
  refreshing: "refreshing",
};

/** Format a byte count as "512 B", "12 KB" or "1.5 MB". */
export function formatBytes(bytes) {
  const n = Number(bytes);
  if (!Number.isFinite(n) || n < 0) return "0 B";
  if (n < 1024) return `${Math.round(n)} B`;
  const kb = n / 1024;
  if (kb < 1024) return `${kb >= 100 ? Math.round(kb) : Math.round(kb * 10) / 10} KB`;
  const mb = kb / 1024;
  return `${mb >= 100 ? Math.round(mb) : Math.round(mb * 10) / 10} MB`;
}

/** Format elapsed milliseconds as "5s" or "1m 05s". */
export function formatElapsed(ms) {
  const n = Number(ms);
  const totalSeconds = Number.isFinite(n) && n > 0 ? Math.floor(n / 1000) : 0;
  if (totalSeconds < 60) return `${totalSeconds}s`;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}m ${String(seconds).padStart(2, "0")}s`;
}

/** Human readable label for a stage. `detail` is used by the reading stage (file size in bytes). */
export function getStageLabel(stage, detail) {
  switch (stage) {
    case IMPORT_STAGES.reading:
      return detail != null && detail !== ""
        ? `Reading backup file (${formatBytes(detail)})`
        : "Reading backup file";
    case IMPORT_STAGES.validating:
      return "Validating backup";
    case IMPORT_STAGES.loadingCurrent:
      return "Loading current database";
    case IMPORT_STAGES.merging:
      return "Merging";
    case IMPORT_STAGES.uploading:
      return "Uploading to server";
    case IMPORT_STAGES.restoring:
      return "Restoring database (this can take a while for large backups)";
    case IMPORT_STAGES.refreshing:
      return "Refreshing settings";
    default:
      return "";
  }
}

/** Full progress line, e.g. "Uploading to server (3s)". The ticker only shows once a second has passed. */
export function formatProgressMessage(progress, now = Date.now()) {
  if (!progress || !progress.stage) return "";
  const label = getStageLabel(progress.stage, progress.detail);
  if (!label) return "";
  const startedAt = Number(progress.startedAt);
  if (!Number.isFinite(startedAt)) return label;
  const elapsedMs = Math.max(0, now - startedAt);
  return elapsedMs >= 1000 ? `${label} - ${formatElapsed(elapsedMs)} elapsed` : label;
}

/** Append the total duration to a success message, e.g. "Done (took 4s)". */
export function withDuration(message, ms) {
  return `${message} (took ${formatElapsed(ms)})`;
}
