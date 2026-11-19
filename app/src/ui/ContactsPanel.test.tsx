import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { FoundUserRow, reloadContacts } from "./ContactsPanel";
import { friends } from "../api";
import { getState, isReadonlyDm, resetSessionState, setState } from "../store";
import type { ConversationView, FriendView, UserPublic } from "../types";

afterEach(() => {
  vi.restoreAllMocks();
  setState(resetSessionState());
});

const user = (id: string): UserPublic => ({
  id,
  username: id,
  display_name: `用户${id}`,
  role: "user",
  last_seen_at: null,
});

const friend = (id: string): FriendView => ({
  user: user(id),
  since: "2026-01-01T00:00:00Z",
});

const noop = () => {};

const renderRow = (overrides: Partial<Parameters<typeof FoundUserRow>[0]> = {}) =>
  renderToStaticMarkup(
    <FoundUserRow
      u={user("u1")}
      friendList={[]}
      picked={false}
      msg=""
      onPick={noop}
      onMsg={noop}
      onSend={noop}
      onDm={noop}
      {...overrides}
    />,
  );

describe("好友搜索结果行", () => {
  it("已是好友：显示「已是好友」和「发消息」，不显示「加好友」", () => {
    const html = renderRow({ u: user("u1"), friendList: [friend("u1"), friend("u2")] });
    expect(html).toContain("已是好友");
    expect(html).toContain("发消息");
    expect(html).not.toContain("加好友");
  });

  it("非好友：仍显示「加好友」，不出现已好友态", () => {
    const html = renderRow({ u: user("u3"), friendList: [friend("u1")] });
    expect(html).toContain("加好友");
    expect(html).not.toContain("发消息");
    expect(html).not.toContain("已是好友");
  });

  it("非好友已选中：显示验证消息输入与「发送」", () => {
    const html = renderRow({ u: user("u3"), picked: true });
    expect(html).toContain("发送");
    expect(html).not.toContain("加好友");
  });
});

describe("contacts friendship authority", () => {
  const dm: ConversationView = { id: "c", kind: "dm", peer: user("u1"), name: null, owner_id: null, member_count: 2, last_seq: 0, unread_count: 0, has_mention: false, last_message: null, created_at: "2026-01-01" };

  it.each([[], [friend("u1")]])("makes a successful contacts refresh authoritative: %j", async (list) => {
    setState({ friends: [friend("u1")], friendsLoaded: false });
    vi.spyOn(friends, "list").mockResolvedValue(list);
    vi.spyOn(friends, "requests").mockResolvedValue({ incoming: [], outgoing: [] });
    await reloadContacts();
    expect(getState().friendsLoaded).toBe(true);
    expect(isReadonlyDm(dm, getState())).toBe(list.length === 0);
  });

  it("keeps failed contacts loads unknown and cached DMs writable", async () => {
    setState({ friends: [friend("u1")], friendsLoaded: false });
    vi.spyOn(friends, "list").mockRejectedValue(new TypeError("offline"));
    vi.spyOn(friends, "requests").mockResolvedValue({ incoming: [], outgoing: [] });
    await expect(reloadContacts()).rejects.toThrow("offline");
    expect(getState().friendsLoaded).toBe(false);
    expect(getState().friends).toEqual([friend("u1")]);
    expect(isReadonlyDm(dm, getState())).toBe(false);
  });
});
