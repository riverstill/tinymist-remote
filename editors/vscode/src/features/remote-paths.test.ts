import { afterEach, describe, expect, test, vi } from "vitest";

vi.mock("vscode", () => ({}));

import { setRemotePathMappings, toLocalPath, toRemotePath } from "./remote-paths.js";

afterEach(() => {
  setRemotePathMappings(undefined);
});

describe("remote path mapping", () => {
  test("keeps paths when no mapping is configured", () => {
    expect(toRemotePath("/workspace/main.typ")).toBe("/workspace/main.typ");
    expect(toLocalPath("/workspace/main.typ")).toBe("/workspace/main.typ");
  });

  test("maps local paths to the remote path space", () => {
    setRemotePathMappings([{ local: "C:\\repos\\notes", remote: "/workspace/notes" }]);

    expect(toRemotePath("C:\\repos\\notes\\main.typ")).toBe("/workspace/notes/main.typ");
    expect(toRemotePath("C:/repos/notes")).toBe("/workspace/notes");
    expect(toRemotePath("C:\\repos\\other\\main.typ")).toBe("C:\\repos\\other\\main.typ");
  });

  test("maps remote paths to the local path space", () => {
    setRemotePathMappings([{ local: "C:\\repos\\notes", remote: "/workspace/notes" }]);

    expect(toLocalPath("/workspace/notes/main.typ")).toBe("C:/repos/notes/main.typ");
    expect(toLocalPath("/workspace/notes")).toBe("C:/repos/notes");
    expect(toLocalPath("/workspace/other/main.typ")).toBe("/workspace/other/main.typ");
  });

  test("ignores incomplete mappings", () => {
    setRemotePathMappings([{ local: "", remote: "/workspace" }]);

    expect(toLocalPath("/workspace/main.typ")).toBe("/workspace/main.typ");
  });
});
