import type { TFunction } from "i18next";

/** Rust sends `"<code>: <message>"`; the code picks the wording. */
export const splitTransferError = (e: unknown): { code: string; message: string } => {
  const text = e instanceof Error ? e.message : String(e);
  const match = /^([a-z_]+): ([\s\S]*)$/.exec(text);
  return match != null ? { code: match[1] ?? "", message: match[2] ?? "" } : { code: "", message: text };
};

const KNOWN_ERRORS = new Set([
  "corrupt",
  "too_new",
  "digest_running",
  "embed_running",
  "busy",
  "no_space",
  "exists",
  "bad_destination",
  "nothing_to_export",
  "settings",
  "not_prepared",
  "not_empty",
]);

export const transferErrorText = (e: unknown, t: TFunction): string => {
  const { code, message } = splitTransferError(e);
  if (code === "cancelled") return t("kb.transfer.error.cancelled");
  return KNOWN_ERRORS.has(code)
    ? t(`kb.transfer.error.${code}`, { message })
    : t("kb.transfer.error.generic", { message });
};
