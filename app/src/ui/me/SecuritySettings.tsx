import { useCallback, useEffect, useState } from "react";
import { auth, saveSession } from "../../api";
import type { SessionView } from "../../types";
import { startWs, stopWs } from "../../ws";
import { confirmDialog } from "../Modal";
import SetRow from "./SetRow";

const fmtTime = (s: string) => new Date(s).toLocaleString();

/** 安全设置（V2 P1-5）：自助改密码 + 登录设备管理 */
export default function SecuritySettings() {
  const [oldPw, setOldPw] = useState("");
  const [newPw, setNewPw] = useState("");
  const [newPw2, setNewPw2] = useState("");
  const [pwOpen, setPwOpen] = useState(false);
  const [pwBusy, setPwBusy] = useState(false);
  const [pwMsg, setPwMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const [sessions, setSessions] = useState<SessionView[] | null>(null);
  const [listMsg, setListMsg] = useState("");
  const [busyId, setBusyId] = useState<string | null>(null);

  const loadSessions = useCallback(async () => {
    try {
      setSessions(await auth.sessions());
      setListMsg("");
    } catch (e) {
      setListMsg(e instanceof Error ? e.message : "加载失败");
    }
  }, []);

  useEffect(() => {
    void loadSessions();
  }, [loadSessions]);

  const changePw = async (e: React.FormEvent) => {
    e.preventDefault();
    setPwMsg(null);
    if (newPw !== newPw2) {
      setPwMsg({ ok: false, text: "两次输入的新密码不一致" });
      return;
    }
    if (newPw.length < 6 || newPw.length > 72) {
      setPwMsg({ ok: false, text: "新密码长度须为 6-72 位" });
      return;
    }
    setPwBusy(true);
    try {
      const r = await auth.changePassword(oldPw, newPw);
      // 改密成功：换新凭据、重建 WS（旧连接的凭据版本已失效）；不跳登录页
      saveSession(r);
      stopWs();
      void startWs();
      setOldPw("");
      setNewPw("");
      setNewPw2("");
      setPwOpen(false);
      setPwMsg({ ok: true, text: "密码已修改，其他设备已退出" });
      void loadSessions();
    } catch (ex) {
      setPwMsg({
        ok: false,
        text: ex instanceof Error ? ex.message : "修改失败",
      });
    } finally {
      setPwBusy(false);
    }
  };

  const revoke = async (id: string) => {
    setBusyId(id);
    setListMsg("");
    try {
      await auth.revokeSession(id);
      await loadSessions();
    } catch (e) {
      setListMsg(e instanceof Error ? e.message : "操作失败");
    } finally {
      setBusyId(null);
    }
  };

  const revokeOthers = async () => {
    if (
      !(await confirmDialog("退出其他所有设备？它们的登录将立即失效。", {
        danger: true,
        okText: "退出其他设备",
      }))
    )
      return;
    setBusyId("others");
    setListMsg("");
    try {
      await auth.revokeOtherSessions();
      await loadSessions();
    } catch (e) {
      setListMsg(e instanceof Error ? e.message : "操作失败");
    } finally {
      setBusyId(null);
    }
  };

  return (
    <>
      <SetRow
        label="密码"
        desc={
          pwMsg?.ok ? <span className="ok-hint">{pwMsg.text}</span> : "修改后其他设备会退出登录"
        }
      >
        <button
          onClick={() => {
            setPwOpen((o) => !o);
            setPwMsg(null);
          }}
        >
          {pwOpen ? "取消" : "修改密码"}
        </button>
      </SetRow>
      {pwOpen && (
        <form className="set-block set-form" onSubmit={changePw}>
          <input
            type="password"
            placeholder="当前密码"
            value={oldPw}
            onChange={(e) => setOldPw(e.target.value)}
            autoComplete="current-password"
            required
          />
          <input
            type="password"
            placeholder="新密码（6-72 位）"
            value={newPw}
            onChange={(e) => setNewPw(e.target.value)}
            autoComplete="new-password"
            required
          />
          <input
            type="password"
            placeholder="再输一遍新密码"
            value={newPw2}
            onChange={(e) => setNewPw2(e.target.value)}
            autoComplete="new-password"
            required
          />
          {pwMsg && !pwMsg.ok && <div className="auth-err">{pwMsg.text}</div>}
          <button className="primary" disabled={pwBusy}>
            {pwBusy ? "提交中…" : "确认修改"}
          </button>
        </form>
      )}

      <SetRow
        label="登录设备"
        desc={listMsg ? <span className="auth-err">{listMsg}</span> : !sessions && "加载中…"}
      >
        {sessions?.some((sv) => !sv.current) && (
          <button
            className="danger"
            disabled={busyId === "others"}
            onClick={() => void revokeOthers()}
          >
            {busyId === "others" ? "处理中…" : "退出其他设备"}
          </button>
        )}
      </SetRow>
      {sessions?.map((sv) => (
        <div className="set-row sess-row" key={sv.id}>
          <div className="set-main">
            <div className="sess-head">
              <span className={`dot${sv.online ? " on" : ""}`} />
              <b>{sv.device_label || "未命名设备"}</b>
              {sv.current && <span className="sess-current">本设备</span>}
            </div>
            <div className="muted sess-ua" title={sv.user_agent}>
              {sv.user_agent || "未知 UA"}
            </div>
            <div className="muted">最近活动 {fmtTime(sv.last_used_at)}</div>
          </div>
          {!sv.current && (
            <button
              className="danger"
              disabled={busyId === sv.id}
              onClick={() => void revoke(sv.id)}
            >
              {busyId === sv.id ? "移出中…" : "移出"}
            </button>
          )}
        </div>
      ))}
    </>
  );
}
