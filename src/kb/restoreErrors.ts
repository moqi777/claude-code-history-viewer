import type { TFunction } from "i18next";
import { splitTransferError } from "./transferErrors";

const KNOWN = new Set([
  "backup_incomplete",
  "corrupt",
  "too_new",
  "bad_source",
  "busy",
  "digest_running",
  "embed_running",
  "no_space",
  "cancelled",
]);

/** Rust sends `"<code>: <message>"`; the code picks the wording. */
export const restoreErrorText = (e: unknown, t: TFunction): string => {
  const { code, message } = splitTransferError(e);
  return KNOWN.has(code)
    ? t(`kb.restore.error.${code}`, { message })
    : t("kb.restore.error.generic", { message });
};
