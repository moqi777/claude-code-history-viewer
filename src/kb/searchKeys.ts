import type { KeyboardEvent } from "react";

/**
 * Enter that should start a search. The Enter that confirms an IME composition
 * (pinyin → text) is not one: `isComposing` is set, and WebKit reports it as
 * `keyCode` 229 instead.
 */
export const isSearchEnter = (e: KeyboardEvent): boolean =>
  e.key === "Enter" && !e.nativeEvent.isComposing && e.keyCode !== 229;
