import { describe, expect, it } from "vitest";
import { parseFileReference } from "./filePathRef";

describe("parseFileReference", () => {
  it("parses workspace-relative and absolute POSIX / Windows paths", () => {
    expect(parseFileReference("src/ui/FilePreview.tsx")).toEqual({
      raw: "src/ui/FilePreview.tsx",
      path: "src/ui/FilePreview.tsx",
      lineStart: undefined,
      lineEnd: undefined,
      display: "src/ui/FilePreview.tsx",
    });

    expect(parseFileReference("./app/src/main.ts:42:8")).toEqual({
      raw: "./app/src/main.ts:42:8",
      path: "./app/src/main.ts",
      lineStart: 42,
      lineEnd: undefined,
      display: "./app/src/main.ts:42:8",
    });

    expect(parseFileReference("C:\\Users\\xie_f\\projects\\chat\\README.md#L10-L24")).toEqual({
      raw: "C:\\Users\\xie_f\\projects\\chat\\README.md#L10-L24",
      path: "C:\\Users\\xie_f\\projects\\chat\\README.md",
      lineStart: 10,
      lineEnd: 24,
      display: "C:\\Users\\xie_f\\projects\\chat\\README.md#L10-L24",
    });

    expect(parseFileReference("/home/dev/chat/server/src/main.rs:18")).toEqual({
      raw: "/home/dev/chat/server/src/main.rs:18",
      path: "/home/dev/chat/server/src/main.rs",
      lineStart: 18,
      lineEnd: undefined,
      display: "/home/dev/chat/server/src/main.rs:18",
    });
  });

  it("parses file:// URIs including Windows drives, percent encoding and line ranges", () => {
    expect(
      parseFileReference("file:///C:/Users/xie_f/projects/chat/app/src/App.tsx#L12-L20"),
    ).toEqual({
      raw: "file:///C:/Users/xie_f/projects/chat/app/src/App.tsx#L12-L20",
      path: "C:/Users/xie_f/projects/chat/app/src/App.tsx",
      lineStart: 12,
      lineEnd: 20,
      display: "file:///C:/Users/xie_f/projects/chat/app/src/App.tsx#L12-L20",
    });

    expect(
      parseFileReference("file:///home/dev/%E6%96%87%E6%A1%A3/notes.md"),
    ).toEqual({
      raw: "file:///home/dev/%E6%96%87%E6%A1%A3/notes.md",
      path: "/home/dev/文档/notes.md",
      lineStart: undefined,
      lineEnd: undefined,
      display: "file:///home/dev/%E6%96%87%E6%A1%A3/notes.md",
    });
  });

  it("parses unambiguous bare filenames and exact build/config files", () => {
    expect(parseFileReference("package.json")?.path).toBe("package.json");
    expect(parseFileReference("Cargo.toml:5")?.lineStart).toBe(5);
    expect(parseFileReference("Dockerfile")?.path).toBe("Dockerfile");
    expect(parseFileReference("server/Dockerfile")?.path).toBe("server/Dockerfile");
  });

  it("rejects web URLs, code expressions, shell commands, MIME types and versions", () => {
    expect(parseFileReference("https://example.com/app.ts")).toBeNull();
    expect(parseFileReference("http://localhost:5173/src/main.tsx")).toBeNull();
    expect(parseFileReference("npm run build")).toBeNull();
    expect(parseFileReference("console.log")).toBeNull();
    expect(parseFileReference("window.open")).toBeNull();
    expect(parseFileReference("process.env")).toBeNull();
    expect(parseFileReference("image/png")).toBeNull();
    expect(parseFileReference("text/plain")).toBeNull();
    expect(parseFileReference("v1.2.3")).toBeNull();
    expect(parseFileReference("fn(src/app.ts)")).toBeNull();
  });
});
