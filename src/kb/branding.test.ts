import { describe, expect, it } from "vitest";
import rustKbMod from "../../src-tauri/src/kb/mod.rs?raw";
import { AGENTKB_VERSION } from "./branding";

describe("branding", () => {
  it("shows the same AgentKB version as the backend", () => {
    const match = /pub const AGENTKB_VERSION: &str = "([^"]+)";/.exec(rustKbMod);
    expect(match?.[1]).toBe(AGENTKB_VERSION);
  });
});
