import { newClientId } from "../../clientId";
import { useEffect, useRef, useState } from "react";
import { ApiError, msgs, polls, session } from "../../api";
import { applyRefreshed, applyUpdatedMessage, getState, setState, useApp } from "../../store";
import type { ConversationView, MemberView, MessageView } from "../../types";

export function schedulePollDeadline(expiresAt: string, onExpire: () => void): () => void {
  let timer: ReturnType<typeof setTimeout>;
  const tick = () => {
    const remaining = Date.parse(expiresAt) - Date.now();
    if (!Number.isFinite(remaining)) return;
    if (remaining <= 0) { onExpire(); return; }
    timer = setTimeout(tick, Math.min(remaining, 2_147_483_647));
  };
  tick();
  return () => clearTimeout(timer);
}

/**
 * 投票卡片（kind='poll' 消息体）。
 * 投票 = 替换本人选择集合：单选点其他项换票、点已选项撤票；多选逐项勾选；空集合撤票。
 * mutation 全走 REST 服务端确认后整视图合并；同步由 message_updated 广播帧完成。
 */
export default function PollCard({
  m,
  conv,
  members,
  busy,
  setBusy,
  onErr,
}: {
  m: MessageView;
  conv: ConversationView;
  members: MemberView[];
  busy: boolean;
  setBusy: (v: boolean) => void;
  onErr: (msg: string | null) => void;
}) {
  const s = useApp();
  const p = m.poll;
  const gen = session();
  const owner = useRef<object | null>(null);
  const inflight = useRef(false);
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const token = {};
    owner.current = token;
    inflight.current = false;
    return () => { if (owner.current === token) owner.current = null; };
  }, [m.id, conv.id, gen]);

  const current = (token: object | null) => token != null && owner.current === token && gen === session() && getState().activeConv === conv.id;
  const refresh = async (token: object | null) => {
    const page = await msgs.refresh(conv.id, [m.id]);
    if (current(token)) setState((st) => applyRefreshed(st, conv.id, page.messages));
  };
  useEffect(() => {
    if (!p?.expires_at || p.expired || p.closed_at) return;
    const token = owner.current;
    return schedulePollDeadline(p.expires_at, () => {
      if (!current(token)) return;
      setNow(Date.now());
      void refresh(token).catch(() => {});
    });
  }, [m.id, conv.id, gen, p?.expires_at, p?.expired, p?.closed_at]);

  if (!p) return null;
  const meId = s.me?.id;
  const expired = p.expired || (!!p.expires_at && Date.parse(p.expires_at) <= Math.max(now, Date.now()));
  const closed = !!p.closed_at || expired;
  const mySet = new Set(p.my_option_ids);
  const canClose = !closed && (m.sender_id === meId || conv.owner_id === meId);

  const nameOf = (id: string) =>
    id === meId
      ? (s.me?.display_name ?? "我")
      : (members.find((x) => x.user.id === id)?.user.display_name ?? "成员");

  const submit = async (ids: string[]) => {
    const token = owner.current;
    if (busy || inflight.current || closed || !current(token)) return;
    if (p.expires_at && Date.parse(p.expires_at) <= Date.now()) { setNow(Date.now()); return; }
    inflight.current = true;
    setBusy(true);
    onErr(null);
    try {
      const v = await polls.vote(m.id, ids, newClientId());
      if (current(token)) setState((st) => applyUpdatedMessage(st, v));
    } catch (e) {
      if (!current(token)) return;
      if (e instanceof ApiError && e.status === 409) {
        await refresh(token).catch(() => {});
      }
      if (current(token)) onErr(e instanceof Error ? e.message : "投票失败");
    } finally {
      if (current(token)) { inflight.current = false; setBusy(false); }
    }
  };

  const toggle = (optId: string) => {
    const next = p.allow_multiple
      ? mySet.has(optId)
        ? p.my_option_ids.filter((x) => x !== optId)
        : [...p.my_option_ids, optId]
      : mySet.has(optId)
        ? []
        : [optId];
    void submit(next);
  };

  const close = async () => {
    const token = owner.current;
    if (busy || inflight.current || closed || !current(token)) return;
    if (!window.confirm("关闭投票？关闭后不可再投，结果仍可查看")) return;
    inflight.current = true;
    setBusy(true);
    onErr(null);
    try {
      const v = await polls.close(m.id, newClientId());
      if (current(token)) setState((st) => applyUpdatedMessage(st, v));
    } catch (e) {
      if (!current(token)) return;
      if (e instanceof ApiError && e.status === 409) await refresh(token).catch(() => {});
      if (current(token)) onErr(e instanceof Error ? e.message : "关闭失败");
    } finally {
      if (current(token)) { inflight.current = false; setBusy(false); }
    }
  };

  return (
    <div className="poll-card">
      <div className="poll-q">
        <span className="poll-ic">🗳</span>
        {p.question}
      </div>
      <div className="poll-meta">
        {p.allow_multiple ? "多选" : "单选"}
        {p.anonymous && " · 匿名"}
        {p.expires_at && ` · 截止 ${new Date(p.expires_at).toLocaleString()}`}
      </div>
      {p.options.map((o) => {
        const mine = mySet.has(o.id);
        const pct =
          p.total_voters > 0 ? Math.round((o.count / p.total_voters) * 100) : 0;
        return (
          <button
            key={o.id}
            className={`poll-opt ${mine ? "mine" : ""}`}
            disabled={busy || closed}
            title={
              !p.anonymous && o.voters.length > 0
                ? o.voters.map(nameOf).join("、")
                : undefined
            }
            onClick={() => toggle(o.id)}
          >
            <span className="poll-fill" style={{ width: `${pct}%` }} />
            <span className="poll-label">
              {p.allow_multiple ? (mine ? "☑" : "☐") : mine ? "●" : "○"} {o.label}
            </span>
            <span className="poll-count">
              {o.count} 票{p.total_voters > 0 ? ` ${pct}%` : ""}
            </span>
          </button>
        );
      })}
      <div className="poll-foot">
        <span>{p.total_voters} 人已投</span>
        {p.closed_at ? (
          <span className="muted">已关闭</span>
        ) : expired ? (
          <span className="muted">已截止</span>
        ) : (
          <>
            {mySet.size > 0 && (
              <button
                className="poll-act"
                disabled={busy}
                onClick={() => void submit([])}
              >
                撤票
              </button>
            )}
            {canClose && (
              <button
                className="poll-act"
                disabled={busy}
                onClick={() => void close()}
              >
                关闭投票
              </button>
            )}
          </>
        )}
      </div>
    </div>
  );
}
