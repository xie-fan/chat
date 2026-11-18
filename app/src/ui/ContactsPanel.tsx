import { useMemo, useState } from "react";
import { auth, friends } from "../api";
import { setState, useApp } from "../store";
import type { FriendView, UserPublic } from "../types";
import { openConversation } from "../ws";
import Avatar from "./Avatar";
import EmptyState from "./EmptyState";
import { confirmDialog } from "./Modal";
import { SkeletonLines } from "./Skeleton";
import { AZ_INDEX, groupFriendsByLetter } from "./contactsAz";
import { SearchIcon } from "./icons";

/** 搜索结果行：已是好友的给「发消息」，非好友走加好友流程 */
export function FoundUserRow({
  u,
  friendList,
  picked,
  msg,
  onPick,
  onMsg,
  onSend,
  onDm,
}: {
  u: UserPublic;
  friendList: FriendView[];
  picked: boolean;
  msg: string;
  onPick: () => void;
  onMsg: (v: string) => void;
  onSend: () => void;
  onDm: () => void;
}) {
  const isFriend = friendList.some((f) => f.user.id === u.id);
  return (
    <div className="row found contact-row-card">
      <Avatar
        userId={u.id}
        name={u.display_name}
        avatarVersion={u.avatar_version}
        hasAvatar={u.has_avatar}
      />
      <div className="grow">
        <div>{u.display_name}</div>
        <div className="muted">@{u.username}</div>
      </div>
      {isFriend ? (
        <>
          <span className="muted">已是好友</span>
          <button className="primary" onClick={onDm}>
            发消息
          </button>
        </>
      ) : picked ? (
        <>
          <input
            placeholder="验证消息（可选）"
            value={msg}
            onChange={(e) => onMsg(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && onSend()}
          />
          <button className="primary" onClick={onSend}>
            发送
          </button>
        </>
      ) : (
        <button className="primary" onClick={onPick}>
          加好友
        </button>
      )}
    </div>
  );
}

export default function ContactsPanel() {
  const s = useApp();
  const [q, setQ] = useState("");
  const [results, setResults] = useState<UserPublic[] | null>(null);
  const [picked, setPicked] = useState<UserPublic | null>(null);
  const [msg, setMsg] = useState("");
  const [hint, setHint] = useState("");
  const [searching, setSearching] = useState(false);
  const [activeLetter, setActiveLetter] = useState<string | null>(null);

  const reload = async () => {
    const [f, r] = await Promise.all([friends.list(), friends.requests()]);
    setState({ friends: f, requests: r });
  };

  const search = async () => {
    setResults(null);
    setPicked(null);
    setHint("");
    if (!q.trim()) return;
    setSearching(true);
    try {
      const r = await auth.searchUser(q.trim());
      setResults(r);
      if (r.length === 0) setHint("没有匹配的用户");
    } catch {
      setHint("搜索失败");
    } finally {
      setSearching(false);
    }
  };

  const sendReq = async () => {
    if (!picked) return;
    try {
      await friends.sendRequest(picked.username, msg);
      setHint("已发送好友请求");
      setPicked(null);
      setResults(null);
      setQ("");
      setMsg("");
      void reload();
    } catch (e) {
      setHint(e instanceof Error ? e.message : "发送失败");
    }
  };

  const openDm = (u: UserPublic) => {
    const c = s.conversations.find((x) => x.kind === "dm" && x.peer?.id === u.id);
    if (c) openConversation(c.id);
    else setState({ tab: "chats", activeConv: null });
  };

  const removeFriend = async (u: UserPublic) => {
    if (
      !(await confirmDialog(`删除好友 ${u.display_name}？会话将变为只读。`, {
        danger: true,
        okText: "删除",
      }))
    )
      return;
    await friends.remove(u.id);
    await reload();
  };

  const groups = useMemo(() => groupFriendsByLetter(s.friends), [s.friends]);
  const present = useMemo(() => new Set(groups.map((g) => g.letter)), [groups]);

  const jump = (letter: string) => {
    setActiveLetter(letter);
    document.getElementById(`contact-letter-${letter}`)?.scrollIntoView({
      block: "start",
      behavior: "smooth",
    });
  };

  return (
    <main className="contacts">
      <div className="contacts-layout contacts-inner">
        <div className="contacts-main" style={{ display: "flex", flexDirection: "column", gap: 18 }}>
          <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
            <h2 style={{ margin: 0 }}>联系人与团队成员</h2>
            <span className="status-pill">共 {s.friends.length} 位好友</span>
          </div>

          <div className="bento-grid">
            <section className="bento-card">
              <h3>
                <span>添加好友</span>
              </h3>
              <div className="row" style={{ gap: 8 }}>
                <div className="conv-search-bar" style={{ flex: 1 }}>
                  <span className="conv-search-icon">
                    <SearchIcon size={14} />
                  </span>
                  <input
                    className="search"
                    placeholder="搜索用户名或昵称"
                    value={q}
                    onChange={(e) => setQ(e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && search()}
                  />
                </div>
                <button className="btn-accent" onClick={search}>
                  搜索
                </button>
              </div>
              {searching && <SkeletonLines rows={2} />}
              {!searching && results?.length === 0 && (
                <EmptyState
                  title="没有匹配的用户"
                  hint="换个关键词试试"
                  testId="empty-search"
                />
              )}
              {results?.map((u) => (
                <FoundUserRow
                  key={u.id}
                  u={u}
                  friendList={s.friends}
                  picked={picked?.id === u.id}
                  msg={msg}
                  onPick={() => setPicked(u)}
                  onMsg={setMsg}
                  onSend={sendReq}
                  onDm={() => openDm(u)}
                />
              ))}
              {hint && <div className="muted">{hint}</div>}
            </section>

            <section className="bento-card">
              <h3>
                <span>
                  新的朋友{" "}
                  {s.requests.incoming.length > 0 && `(${s.requests.incoming.length})`}
                </span>
              </h3>
              {s.requests.incoming.length === 0 && s.requests.outgoing.length === 0 && (
                <EmptyState
                  title="暂无好友请求"
                  hint="去添加好友开始聊天"
                  testId="empty-requests"
                />
              )}
              {s.requests.incoming.map((r) => (
                <div className="row contact-row-card" key={r.id}>
                  <Avatar
                    userId={r.user.id}
                    name={r.user.display_name}
                    avatarVersion={r.user.avatar_version}
                    hasAvatar={r.user.has_avatar}
                  />
                  <div className="grow">
                    <div>{r.user.display_name}</div>
                    <div className="muted">{r.message || "请求加你为好友"}</div>
                  </div>
                  <button
                    className="primary"
                    onClick={() => friends.accept(r.id).then(reload)}
                  >
                    接受
                  </button>
                  <button onClick={() => friends.decline(r.id).then(reload)}>拒绝</button>
                </div>
              ))}
              {s.requests.outgoing.map((r) => (
                <div className="row contact-row-card" key={r.id}>
                  <Avatar
                    userId={r.user.id}
                    name={r.user.display_name}
                    avatarVersion={r.user.avatar_version}
                    hasAvatar={r.user.has_avatar}
                  />
                  <div className="grow">
                    <div>{r.user.display_name}</div>
                    <div className="muted">等待对方通过</div>
                  </div>
                </div>
              ))}
            </section>
          </div>

          <section className="bento-card" data-testid="contacts-book">
            <h3>我的好友（{s.friends.length}）</h3>
            {s.friends.length === 0 ? (
              <EmptyState
                title="还没有好友"
                hint="搜索用户名添加一位朋友"
                testId="empty-friends"
              />
            ) : (
              groups.map((g) => (
                <div key={g.letter}>
                  <div className="contact-letter" id={`contact-letter-${g.letter}`}>
                    {g.letter}
                  </div>
                  {g.friends.map((f) => {
                    const u = f.user;
                    return (
                      <div className="row contact-row-card" key={u.id}>
                        <Avatar
                          userId={u.id}
                          name={u.display_name}
                          avatarVersion={u.avatar_version}
                          hasAvatar={u.has_avatar}
                        >
                          {s.online[u.id] && <i className="online-dot" />}
                        </Avatar>
                        <div className="grow">
                          <div>{u.display_name}</div>
                          <div className="muted">
                            @{u.username} ·{" "}
                            {u.is_bot ? "机器人" : s.online[u.id] ? "在线" : "离线"}
                          </div>
                        </div>
                        <button className="primary" onClick={() => openDm(u)}>
                          发消息
                        </button>
                        <button className="danger" onClick={() => removeFriend(u)}>
                          删除
                        </button>
                      </div>
                    );
                  })}
                </div>
              ))
            )}
          </section>
        </div>

        {s.friends.length > 0 && (
          <nav className="az-index" aria-label="通讯录字母索引" data-testid="contacts-az">
            {AZ_INDEX.map((L) => (
              <button
                key={L}
                type="button"
                className={activeLetter === L ? "on" : ""}
                disabled={!present.has(L)}
                onClick={() => present.has(L) && jump(L)}
                title={L}
              >
                {L}
              </button>
            ))}
          </nav>
        )}
      </div>
    </main>
  );
}
