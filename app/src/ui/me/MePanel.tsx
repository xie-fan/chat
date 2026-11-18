import { useState } from "react";
import { auth } from "../../api";
import { clearRecent } from "../../localCache";
import { pushToast, setState, useApp } from "../../store";
import { stopWs } from "../../ws";
import SettingsShell from "../SettingsShell";
import { BellIcon, BotIcon, CpuIcon, SlidersIcon, UserIcon } from "../icons";
import AccountSwitcherCard from "./AccountSwitcherCard";
import AgentSettingsSection from "./AgentSettingsSection";
import AutostartSetting from "./AutostartSetting";
import AvatarSetting from "./AvatarSetting";
import BotSettings from "./BotSettings";
import NotifySettings from "./NotifySettings";
import PushSettings from "./PushSettings";
import SecuritySettings from "./SecuritySettings";
import ServerSetting from "./ServerSetting";
import SetRow, { SetGroup } from "./SetRow";
import StorageUsage from "./StorageUsage";
import ThemeSetting from "./ThemeSetting";
import UpdateSetting from "./UpdateSetting";

/** 设置中心：通用设置、账号与安全（含多账号切换）、消息通知、机器人接入、Agent 配置 */
export default function MePanel() {
  return (
    <SettingsShell
      sections={[
        { id: "general", label: "通用设置", icon: SlidersIcon, render: () => <GeneralSection /> },
        { id: "account", label: "账号与安全", icon: UserIcon, render: () => <AccountSection /> },
        {
          id: "notify",
          label: "消息通知",
          icon: BellIcon,
          render: () => (
            <SetGroup>
              <NotifySettings />
              <PushSettings />
            </SetGroup>
          ),
        },
        {
          id: "bots",
          label: "机器人接入",
          icon: BotIcon,
          render: () => (
            <SetGroup>
              <BotSettings />
            </SetGroup>
          ),
        },
        {
          id: "agents",
          label: "Agent 配置",
          icon: () => <CpuIcon size={18} />,
          wide: true,
          render: () => <AgentSettingsSection />,
        },
      ]}
    />
  );
}

function AccountSection() {
  const s = useApp();
  const [name, setName] = useState(s.me?.display_name ?? "");
  const [hint, setHint] = useState("");

  const save = async () => {
    try {
      const me = await auth.updateMe(name.trim());
      setState({ me });
      setHint("已保存");
    } catch (e) {
      setHint(e instanceof Error ? e.message : "保存失败");
    }
  };

  const logout = () => {
    auth.logout();
    stopWs();
    window.dispatchEvent(new Event("chat:logout"));
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      <section className="card me-profile">
        <AvatarSetting />
        <div className="grow">
          <div className="me-name">{s.me?.display_name}</div>
          <div className="muted">@{s.me?.username}</div>
          <div className="row">
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="昵称"
              maxLength={50}
            />
            <button
              className="primary"
              disabled={!name.trim() || name.trim() === s.me?.display_name}
              onClick={save}
            >
              保存昵称
            </button>
          </div>
          {hint && <div className="muted">{hint}</div>}
        </div>
      </section>

      <AccountSwitcherCard />

      <SetGroup>
        <SecuritySettings />
      </SetGroup>

      <section className="card set-group set-logout-card">
        <button className="set-logout" onClick={logout}>
          退出当前账号
        </button>
      </section>
    </div>
  );
}

function GeneralSection() {
  return (
    <SetGroup>
      <ThemeSetting />
      <StorageUsage />
      <SetRow label="本地消息缓存" desc="清理此设备上保存的最近会话消息缓存">
        <button
          type="button"
          className="btn-ghost"
          onClick={() => {
            clearRecent();
            pushToast({ title: "本地缓存已清理", text: "已移除本设备上的会话缓存数据" });
          }}
        >
          清理缓存
        </button>
      </SetRow>
      <ServerSetting />
      <UpdateSetting />
      <AutostartSetting />
    </SetGroup>
  );
}
