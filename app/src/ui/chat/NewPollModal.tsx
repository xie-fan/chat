import { newClientId } from "../../clientId";
import { useEffect, useRef, useState } from "react";
import { ApiError, polls, session } from "../../api";
import { applyLiveMessage, getState, setState } from "../../store";
import type { CreatePollReq } from "../../types";

export function preparePollSubmission(draft: CreatePollReq, previous: CreatePollReq | null): CreatePollReq {
  const payload = { question: draft.question.trim(), options: [...new Set(draft.options.map((o) => o.trim()).filter(Boolean))], allow_multiple: draft.allow_multiple, anonymous: draft.anonymous, expires_at: draft.expires_at ?? null };
  if (previous && JSON.stringify(payload) === JSON.stringify({ question: previous.question, options: previous.options, allow_multiple: previous.allow_multiple, anonymous: previous.anonymous, expires_at: previous.expires_at ?? null })) return previous;
  if (!payload.question || Array.from(payload.question).length > 200) throw new Error("问题需 1–200 字");
  if (payload.options.length < 2 || payload.options.length > 10) throw new Error("需 2–10 个不同选项");
  if (payload.options.some((o) => Array.from(o).length > 100)) throw new Error("选项每项 1–100 字");
  if (payload.expires_at && (!Number.isFinite(Date.parse(payload.expires_at)) || Date.parse(payload.expires_at) <= Date.now())) throw new Error("截止时间必须在未来");
  return { ...payload, client_msg_id: newClientId() };
}

/**
 * 发起投票弹窗（仅群聊入口）。创建走 REST，复用消息事务的
 * client_msg_id 幂等；message 广播帧负责全员同步。
 */
export default function NewPollModal({
  convId,
  onClose,
}: {
  convId: string;
  onClose: () => void;
}) {
  const [question, setQuestion] = useState("");
  const [options, setOptions] = useState<string[]>(["", ""]);
  const [multi, setMulti] = useState(false);
  const [anon, setAnon] = useState(false);
  const [deadline, setDeadline] = useState(""); // datetime-local，可空
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);
  const submitted = useRef<CreatePollReq | null>(null);
  const inflight = useRef(false);
  const owner = useRef<object | null>(null);
  const gen = session();
  useEffect(() => {
    const token = {};
    owner.current = token;
    submitted.current = null;
    inflight.current = false;
    return () => { if (owner.current === token) owner.current = null; };
  }, [convId, gen]);

  const setOpt = (i: number, v: string) =>
    setOptions((o) => o.map((x, j) => (j === i ? v : x)));

  const create = async () => {
    const token = owner.current;
    const current = () => token != null && token === owner.current && gen === session() && getState().activeConv === convId;
    if (inflight.current || !current()) return;
    let payload: CreatePollReq;
    try {
      payload = preparePollSubmission({ question, options, allow_multiple: multi, anonymous: anon,
        expires_at: deadline ? new Date(deadline).toISOString() : null }, submitted.current);
    } catch (e) {
      setErr(e instanceof Error ? e.message : "请检查投票内容");
      return;
    }
    submitted.current = payload;
    inflight.current = true;
    setBusy(true);
    setErr("");
    try {
      const m = await polls.create(convId, payload);
      if (!current()) return;
      setState((st) => applyLiveMessage(st, m));
      submitted.current = null;
      onClose();
    } catch (e) {
      if (!current()) return;
      const rejected = e instanceof ApiError && [400, 403, 404, 409, 422].includes(e.status);
      if (rejected) submitted.current = null;
      setErr(rejected ? e.message : "结果未确认，请重试原投票；修改内容将作为新投票提交。");
    } finally {
      if (current()) { inflight.current = false; setBusy(false); }
    }
  };

  return (
    <div className="modal-mask" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h3>发起投票</h3>
        <input
          autoFocus
          placeholder="投票问题（1–200 字）"
          value={question}
          disabled={busy}
          onChange={(e) => setQuestion(e.target.value)}
        />
        <div className="poll-opts-edit">
          {options.map((o, i) => (
            <div className="row poll-opt-row" key={i}>
              <input
                placeholder={`选项 ${i + 1}`}
                value={o}
                disabled={busy}
                onChange={(e) => setOpt(i, e.target.value)}
              />
              {options.length > 2 && (
                <button
                  className="icon-btn"
                  title="删除选项"
                  disabled={busy}
                  onClick={() =>
                    setOptions((x) => x.filter((_, j) => j !== i))
                  }
                >
                  ✕
                </button>
              )}
            </div>
          ))}
          {options.length < 10 && (
            <button
              className="poll-add"
              disabled={busy}
              onClick={() => setOptions((o) => [...o, ""])}
            >
              + 添加选项
            </button>
          )}
        </div>
        <label className="row pick">
          <input
            type="checkbox"
            disabled={busy}
            checked={multi}
            onChange={(e) => setMulti(e.target.checked)}
          />
          允许多选
        </label>
        <label className="row pick">
          <input
            type="checkbox"
            disabled={busy}
            checked={anon}
            onChange={(e) => setAnon(e.target.checked)}
          />
          匿名投票（不显示投票人）
        </label>
        <label className="row pick">
          <span className="muted">截止时间（可留空）</span>
          <input
            type="datetime-local"
            value={deadline}
            disabled={busy}
            onChange={(e) => setDeadline(e.target.value)}
          />
        </label>
        {err && <div className="auth-err">{err}</div>}
        <div className="row end">
          <button onClick={onClose}>取消</button>
          <button className="primary" onClick={create} disabled={busy}>
            {busy ? "创建中…" : "发起投票"}
          </button>
        </div>
      </div>
    </div>
  );
}
