export const formatBytes = (bytes: number): string => {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
};

/** Whole days since `iso`, or `null` when it is missing or unparsable. */
export const daysSince = (iso: string | null | undefined, now = Date.now()): number | null => {
  if (iso == null) return null;
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return null;
  return Math.floor((now - then) / (24 * 60 * 60 * 1000));
};

export const BACKUP_STALE_DAYS = 7;
