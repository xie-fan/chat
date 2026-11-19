// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { installFileDropGuard } from "../../fileDrop";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview, type DragDropEvent } from "@tauri-apps/api/webview";
import { resetSessionState, setState } from "../../store";
import type { ConversationView } from "../../types";
import ChatView from "./ChatView";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/webview", () => ({ getCurrentWebview: vi.fn() }));

function dragEvent(type: string, dataTransfer: DataTransfer) {
  return Object.assign(
    new Event(type, { bubbles: true, cancelable: true }),
    { dataTransfer },
  );
}

it("prevents browser file handling outside chat before mounting React, without canceling text drags", () => {
  const uninstall = installFileDropGuard(window);
  const fileDrag = new DataTransfer();
  Object.defineProperty(fileDrag, "types", { value: ["Files"] });
  const fileDrop = new DataTransfer();
  fileDrop.items.add(new File(["hi"], "outside.txt"));
  const textDrag = new DataTransfer();
  textDrag.setData("text/plain", "text");
  try {
    for (const [type, data] of [["dragover", fileDrag], ["drop", fileDrop]] as const) {
      expect(document.body.dispatchEvent(dragEvent(type, data))).toBe(false);
      expect(document.body.dispatchEvent(dragEvent(type, textDrag))).toBe(true);
    }
  } finally {
    uninstall();
  }
});

it("leaves native file selection enabled for a file input", () => {
  const uninstall = installFileDropGuard(window);
  const input = document.createElement("input");
  input.type = "file";
  input.accept = ".tar.gz,application/gzip";
  document.body.appendChild(input);
  const data = new DataTransfer();
  Object.defineProperty(data, "types", { value: ["Files"] });
  try {
    expect(input.dispatchEvent(dragEvent("dragover", data))).toBe(true);
    data.items.add(new File(["backup"], "backup.tar.gz", { type: "application/gzip" }));
    expect(input.dispatchEvent(dragEvent("drop", data))).toBe(true);
  } finally {
    input.remove();
    uninstall();
  }
});

it("does not intercept HTML5 URI text drags", () => {
  const uninstall = installFileDropGuard(window);
  const data = new DataTransfer();
  data.setData("text/uri-list", "");
  data.setData("text/plain", "https://example.org/chat");
  try {
    for (const type of ["dragover", "drop"]) {
      expect(document.body.dispatchEvent(dragEvent(type, data))).toBe(true);
    }
  } finally {
    uninstall();
  }
});

it.each(["html", "linux"] as const)("stages files without sending them (%s)", async (platform) => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("fetch", vi.fn(async (url: RequestInfo | URL) =>
    Response.json(String(url).endsWith("/pins") ? [] : {}),
  ));
  const conv: ConversationView = {
    id: "self", kind: "self", name: "文件传输助手", owner_id: null, peer: null, member_count: 1,
    last_seq: 0, unread_count: 0, has_mention: false, last_message: null, created_at: "",
  };
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  const uninstall = installFileDropGuard(window);
  const unlisten = vi.fn();
  let emit: (payload: DragDropEvent) => void = () => {};
  if (platform === "linux") {
    vi.stubGlobal("navigator", { userAgent: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15" });
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    vi.mocked(getCurrentWebview, { partial: true }).mockReturnValue({
      onDragDropEvent: async (callback) => {
        emit = (payload) => callback({ event: "tauri://drag-drop", id: 1, payload });
        return unlisten;
      },
    });
    vi.mocked(invoke).mockImplementation(async (_cmd, args) => {
      if ((args as { path?: string }).path?.endsWith("large.txt")) throw "文件大小不能超过 25 MB";
      return new TextEncoder().encode("text/plain\0hi").buffer;
    });
  }
  try {
    await act(async () => {
      setState({ activeConv: conv.id, conversations: [conv], historyLoaded: { [conv.id]: true } });
      root.render(<ChatView />);
    });
    const names = ["header.txt", "composer.txt"];
    const targets = [".chat-head .chat-head-main", ".composer textarea"];
    if (platform === "html") act(() => {
      for (const [i, selector] of targets.entries()) {
        const target = host.querySelector(selector)!;
        const data = new DataTransfer();
        const liveFiles = [new File(["hi"], names[i])];
        Object.defineProperties(data, {
          types: { value: ["Files"] },
          files: { value: liveFiles },
        });
        expect(target.dispatchEvent(dragEvent("dragover", data))).toBe(false);
        expect(data.dropEffect).toBe("copy");
        expect(target.dispatchEvent(dragEvent("drop", data))).toBe(false);
        liveFiles.length = 0;
      }
    });
    else {
      await vi.waitFor(() => expect(getCurrentWebview).toHaveBeenCalled());
      const dropTargets = [document.body, ...targets.map((selector) => host.querySelector(selector)!)];
      vi.spyOn(document, "elementFromPoint").mockImplementation((x) => dropTargets[x]);
      const drop = (x: number, ...paths: string[]) => emit({
        type: "drop", paths, position: { x, y: 10 },
      } as DragDropEvent);
      await act(async () => drop(0, "/tmp/outside.txt"));
      expect(invoke).not.toHaveBeenCalled();
      for (const [i, name] of names.entries()) {
        await act(async () => drop(i + 1, `/tmp/${name}`));
      }
      await act(async () => drop(1, "/tmp/large.txt"));
      expect(host.textContent).not.toContain("large.txt");
      await act(async () => drop(2, "https://example.org/chat"));
      expect(host.querySelector("textarea")?.value).toBe("https://example.org/chat");
      expect(invoke).toHaveBeenCalledTimes(3);
    }
    expect(Array.from(host.querySelectorAll(".pending-files .pf-name"), (el) => el.textContent))
      .toEqual(names);
    expect(vi.mocked(fetch).mock.calls.some(([url]) => String(url).endsWith("/v1/files"))).toBe(false);
  } finally {
    await act(async () => root.unmount());
    if (platform === "linux") expect(unlisten).toHaveBeenCalled();
    host.remove();
    uninstall();
    setState(resetSessionState());
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  }
});
