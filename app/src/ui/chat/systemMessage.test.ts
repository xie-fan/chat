import { describe, expect, it } from "vitest";
import { parseSystemPayload, systemBarText } from "./systemMessage";

describe("systemMessage", () => {
  it("parses machine-readable payload", () => {
    const p = parseSystemPayload(
      JSON.stringify({
        code: "group_left",
        text: "Bob 退出了群聊",
        actor_name: "Bob",
      }),
    );
    expect(p).toEqual({
      code: "group_left",
      text: "Bob 退出了群聊",
      actor_name: "Bob",
      group_name: undefined,
      invitee_names: undefined,
    });
  });

  it("maps seven codes to centered-bar Chinese copy", () => {
    expect(
      systemBarText(JSON.stringify({ code: "group_created", actor_name: "A", group_name: "周末" })),
    ).toBe("A 创建了群聊");
    expect(
      systemBarText(JSON.stringify({ code: "group_renamed", actor_name: "A", group_name: "新名" })),
    ).toBe("A 将群名改为「新名」");
    expect(
      systemBarText(JSON.stringify({ code: "group_announcement", actor_name: "A" })),
    ).toBe("A 更新了群公告");
    expect(
      systemBarText(
        JSON.stringify({ code: "group_invited", actor_name: "A", invitee_names: ["B", "C"] }),
      ),
    ).toBe("A 邀请 B、C 加入了群聊");
    expect(systemBarText(JSON.stringify({ code: "group_left", actor_name: "B" }))).toBe(
      "B 退出了群聊",
    );
    expect(
      systemBarText(JSON.stringify({ code: "group_removed", actor_name: "A", target_name: "B" })),
    ).toBe("A 将 B 移出了群聊");
    expect(systemBarText(JSON.stringify({ code: "group_dissolved", actor_name: "A" }))).toBe(
      "A 解散了群聊",
    );
  });

  it("falls back to text / raw when unknown or non-json", () => {
    expect(systemBarText(JSON.stringify({ code: "x", text: "兜底" }))).toBe("兜底");
    expect(systemBarText("not-json")).toBe("not-json");
  });
});
