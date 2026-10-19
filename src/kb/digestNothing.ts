import type { TFunction } from "i18next";
import type { KbDigestRunStarted } from "./types";

/** The sentence for "retry did nothing": the code is translated, never the backend's English. */
export function nothingText(t: TFunction, started: Extract<KbDigestRunStarted, { state: "nothing" }>): string {
  return t(`kb.digest.toast.nothing.${started.reason}`);
}
