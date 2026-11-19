// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { resetSessionState, setState } from "../../store";
import type { ConversationView } from "../../types";
import ChatView from "./ChatView";

it("stages files dropped onto an open chat without sending them", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("fetch", vi.fn(async (url: RequestInfo | URL) =>
    Response.json(String(url).endsWith("/pins") ? [] : {}),
  ));
  const conv: ConversationView = {
    id: "self", kind: "self", name: "文件传输助手", owner_id: null, peer: null, member_count: 1,
    last_seq: 0, unread_count: 0, has_mention: false, last_message: null, created_at: "",
  };
  const host = document.createElement("div");
  const root = createRoot(host);
  try {
    await act(async () => {
      setState({ activeConv: conv.id, conversations: [conv], historyLoaded: { [conv.id]: true } });
      root.render(<ChatView />);
    });
    const chat = host.querySelector(".chat-view")!;
    const data = new DataTransfer();
    data.items.add(new File(["hi"], "dropped.txt", { type: "text/plain" }));
    Object.defineProperty(data, "types", { value: ["Files"] });
    const dragEvent = (type: string) => Object.assign(
      new Event(type, { bubbles: true, cancelable: true }),
      { dataTransfer: data },
    );
    const dragOver = dragEvent("dragover");
    const drop = dragEvent("drop");
    act(() => {
      chat.dispatchEvent(dragOver);
      chat.dispatchEvent(drop);
    });
    expect(dragOver.defaultPrevented).toBe(true);
    expect(drop.defaultPrevented).toBe(true);
    expect(host.querySelector(".pending-files .pf-name")?.textContent).toBe("dropped.txt");
    expect(vi.mocked(fetch).mock.calls.some(([url]) => String(url).endsWith("/v1/files"))).toBe(false);
  } finally {
    act(() => root.unmount());
    setState(resetSessionState());
    vi.unstubAllGlobals();
  }
});
