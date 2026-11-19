// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { installFileDropGuard } from "../../fileDrop";
import { resetSessionState, setState } from "../../store";
import type { ConversationView } from "../../types";
import ChatView from "./ChatView";

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

it("cancels URI-only drags on Linux Tauri but not in a web browser", () => {
  vi.stubGlobal("navigator", { userAgent: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15" });
  const uninstall = installFileDropGuard(window);
  const data = new DataTransfer();
  data.setData("text/uri-list", "");
  const input = document.createElement("input");
  input.type = "file";
  document.body.appendChild(input);
  try {
    for (const type of ["dragover", "drop"]) {
      expect(document.body.dispatchEvent(dragEvent(type, data))).toBe(true);
    }
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    for (const type of ["dragover", "drop"]) {
      expect(document.body.dispatchEvent(dragEvent(type, data))).toBe(false);
      expect(input.dispatchEvent(dragEvent(type, data))).toBe(true);
    }
  } finally {
    uninstall();
    input.remove();
    vi.unstubAllGlobals();
  }
});

it("stages files from the chat header and composer without sending them", async () => {
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
  try {
    await act(async () => {
      setState({ activeConv: conv.id, conversations: [conv], historyLoaded: { [conv.id]: true } });
      root.render(<ChatView />);
    });
    const names = ["header.txt", "composer.txt"];
    const targets = [".chat-head .chat-head-main", ".composer textarea"];
    act(() => {
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
    expect(Array.from(host.querySelectorAll(".pending-files .pf-name"), (el) => el.textContent))
      .toEqual(names);
    vi.stubGlobal("navigator", { userAgent: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15" });
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    const nativeInput = host.querySelector<HTMLInputElement>(".chat-drop-input")!;
    const uriDrag = new DataTransfer();
    Object.defineProperty(uriDrag, "types", { value: ["text/uri-list", "text/html"] });
    const body = host.querySelector(".msg-scroll")!;
    expect(nativeInput.hidden).toBe(true);
    body.dispatchEvent(dragEvent("dragenter", uriDrag));
    expect(nativeInput.hidden).toBe(false);
    body.dispatchEvent(dragEvent("dragleave", uriDrag));
    expect(nativeInput.hidden).toBe(true);
    body.dispatchEvent(dragEvent("dragenter", uriDrag));
    nativeInput.dispatchEvent(dragEvent("dragenter", uriDrag));
    body.dispatchEvent(dragEvent("dragleave", uriDrag));
    expect(nativeInput.hidden).toBe(false);
    nativeInput.dispatchEvent(dragEvent("dragleave", uriDrag));
    expect(nativeInput.hidden).toBe(true);
    body.dispatchEvent(dragEvent("dragenter", uriDrag));
    expect(nativeInput.dispatchEvent(dragEvent("drop", uriDrag))).toBe(true);
    const nativeFiles = new DataTransfer();
    nativeFiles.items.add(new File(["hi"], "dolphin.txt"));
    Object.defineProperty(nativeInput, "files", { value: nativeFiles.files });
    act(() => nativeInput.dispatchEvent(new Event("change", { bubbles: true })));
    expect(nativeInput.hidden).toBe(true);
    expect(Array.from(host.querySelectorAll(".pending-files .pf-name"), (el) => el.textContent))
      .toEqual([...names, "dolphin.txt"]);
    expect(vi.mocked(fetch).mock.calls.some(([url]) => String(url).endsWith("/v1/files"))).toBe(false);
  } finally {
    act(() => root.unmount());
    host.remove();
    uninstall();
    setState(resetSessionState());
    vi.unstubAllGlobals();
  }
});
