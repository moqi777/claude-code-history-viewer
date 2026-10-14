import { useCallback, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Verdict } from "./messages";

const errorText = (e: unknown): string => (e instanceof Error ? e.message : String(e));

/** A test that can fail in two ways: the check says no, or the check itself broke. */
export function useTest<T>(run: () => Promise<T>, describe: (result: T) => Verdict) {
  const { t } = useTranslation();
  const [verdict, setVerdict] = useState<Verdict | null>(null);
  const [result, setResult] = useState<T | null>(null);
  const [busy, setBusy] = useState(false);
  const runRef = useRef(run);
  runRef.current = run;
  const describeRef = useRef(describe);
  describeRef.current = describe;

  const test = useCallback(async () => {
    setBusy(true);
    try {
      const r = await runRef.current();
      setResult(r);
      setVerdict(describeRef.current(r));
      return r;
    } catch (e) {
      setResult(null);
      setVerdict({ tone: "error", text: t("kb.test.failed", { message: errorText(e) }) });
      return null;
    } finally {
      setBusy(false);
    }
  }, [t]);

  const reset = useCallback(() => {
    setVerdict(null);
    setResult(null);
  }, []);

  return { verdict, result, busy, test, reset };
}
