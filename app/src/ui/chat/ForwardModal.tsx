import { newClientId } from "../../clientId";
import { useEffect, useRef, useState } from "react";
import { ApiError, msgs as msgsApi, session } from "../../api";
import { applyLiveMessage, convTitle, getState, setState, useApp } from "../../store";
import type { MessageView } from "../../types";
import { forwardTargets } from "./forwardTargets";
import { Z } from "./layers";

const MAX_TARGETS = 10;

type ForwardSubmission = {
  messageId: string;
  targets: string[];
  clientOpId: string;
};

/** 幂等提交载荷：目标集合规范化（排序）后未变 → 复用同一 client_op_id，
 * 结果未确认的重试不会产生第二条转发消息；变了才换键算新提交。 */
export function prepareForwardSubmission(
  previous: ForwardSubmission | null,
  messageId: string,
  targets: string[],
): ForwardSubmission {
  const sorted = [...targets].sort();
  if (
    previous?.messageId === messageId &&
    JSON.stringify(previous.targets) === JSON.stringify(sorted)
  ) {
    return previous;
  }
  if (sorted.length === 0 || sorted.length > MAX_TARGETS) {
    throw new Error(`请选择 1–${MAX_TARGETS} 个目标会话`);
  }
  return { messageId, targets: sorted, clientOpId: newClientId() };
}

type ForwardDone = {
  ok: number;
  fails: { title: string; text: string }[];
};

/**
 * 转发弹窗（V2 P2-4）：搜索 + 会话多选（文件传输助手置顶，其余按列表顺序，
 * 只读私聊已在 forwardTargets 排除），最多 10 个。逐目标结果在弹窗内汇总；
 * 同一次提交的重试复用同一 client_op_id。收藏不走这里（直接调接口到 self）。
 *
 * MF2：另支持「委托提交」模式（props.submit）——批量逐条/合并转发与记录内附件转发。
 * 此时弹窗只负责挑目标和确认按钮的在飞锁定（双击只发一次），确认后调用方执行请求并
 * toast 结果（幂等键在调用方的 payload 里管理）；Promise 落定即关弹窗。
 */
export default function ForwardModal({
  message,
  submit,
  onClose,
}: {
  message?: MessageView;
  /** MF2 委托提交：{ title, run(targets) }；run 内部负责全部错误处理与 toast */
  submit?: { title: string; run: (targets: string[]) => Promise<void> };
  onClose: () => void;
}) {
  const s = useApp();
  const [q, setQ] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<ForwardDone | null>(null);
  const submitted = useRef<ForwardSubmission | null>(null);
  const inflight = useRef(false);
  const owner = useRef<object | null>(null);
  const gen = session();
  useEffect(() => {
    const token = {};
    owner.current = token;
    submitted.current = null;
    inflight.current = false;
    return () => {
      if (owner.current === token) owner.current = null;
    };
  }, [message?.id, gen]);

  const list = forwardTargets(s.conversations, s.friends);
  const kw = q.trim().toLowerCase();
  const shown = kw
    ? list.filter((c) => convTitle(c).toLowerCase().includes(kw))
    : list;

  const toggle = (id: string) => {
    setErr("");
    setSelected((cur) =>
      cur.includes(id)
        ? cur.filter((x) => x !== id)
        : cur.length >= MAX_TARGETS
          ? cur
          : [...cur, id],
    );
  };

  const submitSingle = async () => {
    const token = owner.current;
    const current = () => token != null && token === owner.current && gen === session();
    if (inflight.current || !current()) return;
    let payload: ForwardSubmission;
    try {
      payload = prepareForwardSubmission(submitted.current, message!.id, selected);
    } catch (e) {
      setErr(e instanceof Error ? e.message : "请选择目标");
      return;
    }
    submitted.current = payload;
    inflight.current = true;
    setBusy(true);
    setErr("");
    try {
      const resp = await msgsApi.forward(
        payload.messageId,
        payload.targets,
        payload.clientOpId,
      );
      if (!current()) return;
      // 成功目标立即并入本地（广播帧到达时按 id 去重，无闪烁）
      const oks = resp.results.filter((r) => r.ok && r.message);
      if (oks.length > 0) {
        setState((st) => {
          let cur = st;
          for (const r of oks) cur = { ...cur, ...applyLiveMessage(cur, r.message!) };
          return cur;
        });
      }
      const byId = new Map(s.conversations.map((c) => [c.id, c]));
      const fails = resp.results
        .filter((r) => !r.ok)
        .map((r) => ({
          title: byId.has(r.conversation_id)
            ? convTitle(byId.get(r.conversation_id)!)
            : "未知会话",
          text: r.message_text ?? r.error ?? "发送失败",
        }));
      setDone({ ok: oks.length, fails });
      submitted.current = null;
    } catch (e) {
      if (!current()) return;
      // 明确拒绝 → 换键重来；结果未确认（网络/5xx）→ 保留 client_op_id 幂等重试
      const rejected = e instanceof ApiError && [400, 403, 404, 409, 422].includes(e.status);
      if (rejected) submitted.current = null;
      setErr(
        rejected
          ? e.message
          : "结果未确认，请点重试（同一批目标幂等，不会重复发送）。",
      );
    } finally {
      if (current()) {
        inflight.current = false;
        setBusy(false);
      }
    }
  };

  // MF2 委托提交：结果统一走 toast（C5 文案/旧服务端兜底/重试动作都在调用方）；
  // 弹窗只管 in-flight 锁——resolve 关窗，reject（不该发生，防御）留窗显示错误
  const submitDelegated = async () => {
    const token = owner.current;
    const current = () => token != null && token === owner.current && gen === session();
    if (inflight.current || !current() || !submit) return;
    if (selected.length === 0 || selected.length > MAX_TARGETS) {
      setErr(`请选择 1–${MAX_TARGETS} 个目标会话`);
      return;
    }
    inflight.current = true;
    setBusy(true);
    setErr("");
    try {
      await submit.run([...selected].sort());
      if (current()) onClose();
    } catch (e) {
      if (current()) setErr(e instanceof Error ? e.message : "转发失败");
    } finally {
      if (current()) {
        inflight.current = false;
        setBusy(false);
      }
    }
  };

  const onConfirm = submit ? submitDelegated : submitSingle;

  return (
    // Z.forwardModal：高过查看器/预览（记录内打开时预览不关，同层 DOM 序会盖住弹窗）
    <div className="modal-mask" style={{ zIndex: Z.forwardModal }} onClick={onClose}>
      <div className="modal fwd-modal" onClick={(e) => e.stopPropagation()}>
        <h3>{submit?.title ?? "转发消息"}</h3>
        {done ? (
          <div className="fwd-result">
            {done.ok > 0 && (
              <div className="fwd-ok">已转发到 {done.ok} 个会话</div>
            )}
            {done.fails.map((f, i) => (
              <div className="fwd-fail" key={i}>
                {f.title}：{f.text}
              </div>
            ))}
          </div>
        ) : (
          <>
            <input
              autoFocus
              className="search"
              placeholder="搜索会话"
              value={q}
              disabled={busy}
              onChange={(e) => setQ(e.target.value)}
            />
            <div className="fwd-list">
              {shown.length === 0 && <div className="msg-sys">无可转发会话</div>}
              {shown.map((c) => {
                const on = selected.includes(c.id);
                const full = !on && selected.length >= MAX_TARGETS;
                return (
                  <label
                    key={c.id}
                    className={`fwd-item ${on ? "on" : ""} ${full ? "dim" : ""}`}
                  >
                    <input
                      type="checkbox"
                      checked={on}
                      disabled={busy || full}
                      onChange={() => toggle(c.id)}
                    />
                    <span className={`avatar ${c.kind === "self" ? "self-avatar" : ""}`}>
                      {c.kind === "self" ? "🗂" : convTitle(c)[0]?.toUpperCase()}
                    </span>
                    <span className="fwd-name">{convTitle(c)}</span>
                  </label>
                );
              })}
            </div>
            <div className="muted fwd-count">
              已选 {selected.length} / {MAX_TARGETS}
            </div>
            {err && <div className="auth-err">{err}</div>}
          </>
        )}
        <div className="row end">
          {done ? (
            <button className="primary" onClick={onClose}>
              完成
            </button>
          ) : (
            <>
              <button onClick={onClose}>取消</button>
              <button
                className="primary"
                onClick={onConfirm}
                disabled={busy || selected.length === 0}
              >
                {busy ? "转发中…" : `转发${selected.length ? `（${selected.length}）` : ""}`}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
