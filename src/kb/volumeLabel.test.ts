import { describe, expect, it } from "vitest";
import { shortVolumeId } from "./volumeLabel";

describe("shortVolumeId", () => {
  it("shows the first 8 characters of the GUID on Windows, not `?\\Volume`", () => {
    expect(shortVolumeId("\\\\?\\Volume{9c0ffee1-1234-4abc-8def-0123456789ab}\\")).toBe("9c0ffee1");
  });

  it("keeps the first 8 characters of a macOS volume uuid", () => {
    expect(shortVolumeId("A1B2C3D4-0000-1111-2222-333344445555")).toBe("A1B2C3D4");
  });
});
