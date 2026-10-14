import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Button, Badge } from "@/components/ui";
import { kbApi } from "./api";
import { Field, VerdictLine } from "./fields";
import { describeSkillTest } from "./messages";
import type { KbSkillStatus, KbSkillTarget } from "./types";
import { useKbStore } from "./useKbStore";
import { useTest } from "./useTest";

const TOOL_KEY: Record<KbSkillTarget, string> = {
  claude: "kb.skill.tool.claude",
  codex: "kb.skill.tool.codex",
};

const statusText = (s: KbSkillStatus, t: (key: string) => string): string => {
  if (!s.tool_found) return t("kb.skill.state.toolMissing");
  switch (s.state) {
    case "current":
      return t("kb.skill.state.current");
    case "stale":
      return t("kb.skill.state.stale");
    case "foreign":
      return t("kb.skill.state.foreign");
    default:
      return t("kb.skill.state.notInstalled");
  }
};

/** Install / update / remove the agent skill, and try it for real. */
export const SkillField = () => {
  const { t } = useTranslation();
  const skills = useKbStore((s) => s.skills);
  const [busy, setBusy] = useState<KbSkillTarget | null>(null);
  const { verdict, busy: testing, test } = useTest(
    () => kbApi.skillTest(),
    (r) => describeSkillTest(r, t),
  );

  useEffect(() => {
    void useKbStore.getState().loadSkills();
  }, []);

  const act = async (target: KbSkillTarget, kind: "install" | "uninstall") => {
    setBusy(target);
    const store = useKbStore.getState();
    const { error } =
      kind === "install" ? await store.installSkill(target) : await store.uninstallSkill(target);
    setBusy(null);
    if (error != null) toast.error(t("kb.skill.actionFailed", { message: error }));
    else toast.success(t(kind === "install" ? "kb.skill.installed" : "kb.skill.removed"));
  };

  const anyInstalled = skills?.some((s) => s.state === "current" || s.state === "stale") === true;

  return (
    <Field label={t("kb.skill.title")} hint={t("kb.skill.hint")}>
      <ul className="space-y-2">
        {(skills ?? []).map((s) => {
          const installed = s.state === "current" || s.state === "stale";
          return (
            <li
              key={s.target}
              className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-md border p-3"
            >
              <span className="w-28 font-medium">{t(TOOL_KEY[s.target])}</span>
              <Badge variant={s.state === "current" ? "outline" : s.state === "stale" ? "destructive" : "muted"}>
                {statusText(s, t)}
              </Badge>
              {s.state === "stale" && s.stale_reason != null && (
                <span className="text-xs text-muted-foreground">
                  {t(`kb.skill.stale.${s.stale_reason}`)}
                </span>
              )}
              <span className="ml-auto flex gap-2">
                {s.state !== "foreign" && s.tool_found && (
                  <Button
                    size="sm"
                    variant={s.state === "stale" ? "default" : "outline"}
                    disabled={busy != null || s.state === "current"}
                    onClick={() => void act(s.target, "install")}
                  >
                    {s.state === "stale" ? t("kb.skill.update") : t("kb.skill.install")}
                  </Button>
                )}
                {installed && (
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy != null}
                    onClick={() => void act(s.target, "uninstall")}
                  >
                    {t("kb.skill.uninstall")}
                  </Button>
                )}
              </span>
            </li>
          );
        })}
      </ul>
      <div className="flex flex-wrap items-center gap-3">
        <Button size="sm" variant="outline" onClick={() => void test()} disabled={testing || !anyInstalled}>
          {t("kb.skill.test")}
        </Button>
        <VerdictLine verdict={verdict} busy={testing && verdict == null} />
      </div>
      <p className="text-xs text-muted-foreground">{t("kb.skill.permissionNote")}</p>
    </Field>
  );
};
