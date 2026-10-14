import { describe, expect, it } from "vitest";
import { overviewFixture } from "./fixtures";
import { describeBackup, scheduleHealth } from "./homeStatus";
import { describeDataDir, describeMigrateCheck, describeMirror, describeScheduleEnv } from "./messages";
import { fakeT, volumeFixture } from "./testkit";

const NOW = Date.parse("2026-10-14T12:00:00Z");
const mirror = { enabled: true, target_dir: "/Volumes/B/AgentKB-backup", volume_name: "B" };
const backup = (patch = {}) => ({
  last_at: null, last_result: null, last_skipped_at: null, last_bytes: null, progress: null, ...patch,
});
const connected = { state: "connected", volume: volumeFixture } as const;

describe("describeBackup", () => {
  it("is 'not set up' without a disk", () => {
    expect(describeBackup(overviewFixture(), null, fakeT, NOW)?.text).toBe("kb.status.backup.off");
  });

  it("shows percent while backing up", () => {
    const v = describeBackup(
      overviewFixture({ mirror, activity: "backing_up", backup: backup({ progress: [1, 4] }) }),
      connected, fakeT, NOW,
    );
    expect(v?.text).toContain('"percent":25');
  });

  it("recent backup on a connected disk is fine, on an absent one a quiet warning", () => {
    const recent = backup({ last_at: "2026-10-14T08:00:00Z", last_result: "ok" });
    expect(describeBackup(overviewFixture({ mirror, backup: recent }), connected, fakeT, NOW)).toMatchObject({
      tone: "ok",
    });
    const away = describeBackup(
      overviewFixture({ mirror, backup: recent }),
      { state: "not_connected", reason: "" }, fakeT, NOW,
    );
    expect(away).toMatchObject({ tone: "warn" });
    expect(away?.text).toContain("kb.status.backup.notConnected");
  });

  it("nags after seven days but not before", () => {
    const at = (days: number) =>
      backup({ last_at: new Date(NOW - days * 86_400_000).toISOString(), last_result: "ok" });
    expect(describeBackup(overviewFixture({ mirror, backup: at(6) }), connected, fakeT, NOW)?.text)
      .toContain("kb.status.backup.connected");
    const stale = describeBackup(overviewFixture({ mirror, backup: at(7) }), connected, fakeT, NOW);
    expect(stale).toMatchObject({ tone: "warn" });
    expect(stale?.text).toContain("kb.status.backup.stale");
    expect(stale?.text).toContain('"days":7');
  });

  it("never backed up, and failed backups, are said plainly", () => {
    expect(describeBackup(overviewFixture({ mirror }), connected, fakeT, NOW)?.text).toContain("kb.status.backup.never");
    const failed = describeBackup(
      overviewFixture({ mirror, backup: backup({ last_result: "error:disk went away" }) }),
      connected, fakeT, NOW,
    );
    expect(failed).toMatchObject({ tone: "error" });
    expect(failed?.text).toContain("disk went away");
  });
});

describe("scheduleHealth", () => {
  const base = overviewFixture().schedule;
  const health = (patch: Partial<typeof base>) =>
    scheduleHealth(overviewFixture({ schedule: { ...base, ...patch } }));
  it("classifies the task", () => {
    expect(health({})).toBe("active");
    expect(health({ supported: false })).toBe("unsupported");
    expect(health({ installed: false, loaded: false })).toBe("off");
    expect(health({ loaded: false })).toBe("installedNotLoaded");
    expect(health({ program_exists: false })).toBe("dead");
    expect(health({ installed: false, loaded: true, program_exists: null })).toBe("active");
  });
});

describe("test-button sentences", () => {
  const dir = {
    path: "/d", exists: true, will_create: false, writable: true, free_bytes: 120e9,
    existing: null, db_too_new: false, overlaps_source: null, error: null,
  };
  it("data folder", () => {
    expect(describeDataDir(dir, fakeT)).toMatchObject({ tone: "ok" });
    expect(describeDataDir({ ...dir, writable: false, error: "denied" }, fakeT)).toMatchObject({ tone: "error" });
    expect(describeDataDir({ ...dir, db_too_new: true }, fakeT).text).toBe("kb.test.dataDir.dbTooNew");
    expect(
      describeDataDir({ ...dir, existing: { sessions: 3, bytes: 5, has_db: true, has_raw: true } }, fakeT).text,
    ).toContain("kb.test.dataDir.existing");
  });
  it("mirror", () => {
    expect(describeMirror({ state: "not_connected", reason: "" }, fakeT).tone).toBe("warn");
    expect(describeMirror({ state: "unsupported", reason: { kind: "network", fs_type: "smbfs" } }, fakeT).text)
      .toBe("kb.test.mirror.network");
    expect(describeMirror({ state: "unsupported", reason: { kind: "cloud" } }, fakeT).text).toBe("kb.test.mirror.cloud");
    expect(describeMirror({ state: "not_writable", volume: volumeFixture, message: "ro" }, fakeT).tone).toBe("error");
  });
  it("schedule environment", () => {
    const env = {
      supported: true, launchctl_available: true, launch_agents_writable: true,
      exe: "/x", stability: "stable" as const, ok: true,
    };
    expect(describeScheduleEnv(env, fakeT).tone).toBe("ok");
    expect(describeScheduleEnv({ ...env, stability: "translocated" }, fakeT).text).toBe("kb.test.schedule.translocated");
    expect(describeScheduleEnv({ ...env, stability: "disk_image" }, fakeT).tone).toBe("warn");
    expect(describeScheduleEnv({ ...env, launchctl_available: false }, fakeT).tone).toBe("error");
  });
  it("move check", () => {
    expect(describeMigrateCheck({ target_state: "empty", needed_bytes: 1, free_bytes: 2, problem: "nested" }, fakeT).text)
      .toBe("kb.move.problem.nested");
    expect(describeMigrateCheck({ target_state: "resume", needed_bytes: 1, free_bytes: 2, problem: null }, fakeT).tone)
      .toBe("warn");
  });
});
