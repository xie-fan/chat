import { useEffect, useRef, useState } from "react";
import { getServer, osOfUa } from "../../api";
import {
  apkProgressPercent,
  apkProgressText,
  apkUrl,
  assetFor,
  hasInstaller,
  assetLabel,
  checkInstallPermission,
  downloadUrl,
  downloadUpdateApk,
  apkDownloadProgress,
  errText,
  fetchLatest,
  installUpdateApk,
  isApkPermissionDenied,
  isApkSignatureError,
  isNewer,
  openInstallPermissionSettings,
  type ClientLatest,
} from "../../update";
import SetRow from "./SetRow";

/** 「我」页：Tauri 壳里是应用更新；浏览器里是下载客户端（浏览器端刷新就是新版，不需要更新）。 */
export default function UpdateSetting() {
  return "__TAURI_INTERNALS__" in window ? <ShellUpdate /> : <DownloadClient />;
}

/** 浏览器里：按当前系统给一个推荐安装包，「更多」里列全部平台。没配置在线更新 / 没有 Release → 不显示 */
function DownloadClient() {
  const [latest, setLatest] = useState<ClientLatest | null>(null);
  useEffect(() => {
    void fetchLatest().then(setLatest);
  }, []);
  if (!latest) return null;
  const os = osOfUa(navigator.userAgent);
  const names = latest.assets.map((a) => a.name);
  const mine = assetFor(os, names);
  const all = names.filter((n) => assetLabel(n));
  if (!all.length) return null;
  return (
    <SetRow
      label="下载客户端"
      desc={
        <>
          最新版本 {latest.version}
          {!mine && ` · 暂时没有 ${os} 版`}
          <details className="set-more">
            <summary>其他平台</summary>
            {all.map((n) => (
              <div key={n}>
                <a href={downloadUrl(n)} download>
                  {assetLabel(n)}
                </a>
                <span> · {n}</span>
              </div>
            ))}
          </details>
        </>
      }
    >
      {mine && (
        <a className="btn primary" href={downloadUrl(mine)} download>
          下载 {assetLabel(mine)} 版
        </a>
      )}
    </SetRow>
  );
}

/** In-app APK Installer 阶段：下载中（带进度）→ ready（已缓存，可反复拉起系统安装器）；
 *  ready.denied = Install Permission 被拒，先引导去系统设置 */
type ApkFlow =
  | { phase: "idle" }
  | { phase: "downloading"; received: number; total: number }
  | { phase: "ready"; name: string; denied: boolean };

/** 常驻一次性卸载提示：0.6.2 / 签名不一致的 0.7.0 设备 Overlay Install 装不上，只能卸一次旧版 */
const UNINSTALL_ONCE = "若安装失败请先卸载旧版再装（仅此一次）";
const OR_BROWSER = "；可重试，或用浏览器下载";

function ShellUpdate() {
  const os = osOfUa(navigator.userAgent);
  const android = os === "Android";
  const [current, setCurrent] = useState("");
  const [latest, setLatest] = useState<ClientLatest | null>(null);
  const [busy, setBusy] = useState(false);
  const [checking, setChecking] = useState(true);
  const [checked, setChecked] = useState(false);
  const [hint, setHint] = useState("");
  const [flow, setFlow] = useState<ApkFlow>({ phase: "idle" });
  const pollRef = useRef<number | undefined>(undefined);

  useEffect(() => {
    void import("@tauri-apps/api/app").then((m) => m.getVersion()).then(setCurrent);
    void fetchLatest().then((release) => {
      setLatest(release);
      setChecking(false);
    });
    return () => window.clearInterval(pollRef.current);
  }, []);

  if (!current) return null;
  const newer = !!latest && isNewer(latest.version, current);
  const names = latest?.assets.map((a) => a.name) ?? [];
  const macPackage = os === "macOS" ? assetFor(os, names) : undefined;
  const hasNew = newer && (os === "macOS" ? !!macPackage : hasInstaller(os, names));
  // tag 说明第一行常就是版本号，不重复显示
  const notes = latest?.notes.replace(new RegExp(`^v?${latest.version}\\s*\\n`), "").trim();

  const checkUpdate = async () => {
    setChecking(true);
    setChecked(false);
    setHint("");
    setLatest(await fetchLatest());
    setChecked(true);
    setChecking(false);
  };

  /** Browser Download Fallback：同一个 APK 地址交给系统浏览器 */
  const openInBrowser = async () => {
    if (!latest) return;
    const url = apkUrl(latest);
    if (!url) return;
    const { openUrl } = await import("@tauri-apps/plugin-opener");
    await openUrl(url);
    setHint("下载完成后点开安装包即可覆盖安装");
  };

  /** 拉起系统安装器。用户在系统界面取消不算失败：缓存文件还在，「安装」按钮可再次拉起 */
  const launchInstall = async (name: string) => {
    setBusy(true);
    try {
      await installUpdateApk(name);
      setFlow({ phase: "ready", name, denied: false });
      setHint("已打开系统安装器，按提示完成安装；若取消了可再点「安装」");
    } catch (e) {
      if (isApkPermissionDenied(e)) {
        setFlow({ phase: "ready", name, denied: true });
        setHint("");
      } else if (isApkSignatureError(e)) {
        // Signature Mismatch（-7）：只有卸载旧版一次才能装上
        setFlow({ phase: "ready", name, denied: false });
        setHint(`签名不一致：${UNINSTALL_ONCE}`);
      } else {
        const msg = errText(e);
        // 缓存安装包被清掉（Kotlin 文案含「不存在」）→ 回到可重新下载的状态
        setFlow(
          msg.includes("不存在")
            ? { phase: "idle" }
            : { phase: "ready", name, denied: false },
        );
        setHint(`${msg}${OR_BROWSER}`);
      }
    } finally {
      setBusy(false);
    }
  };

  /** 应用内下载（轮询进度）→ 完成后自动拉起系统安装器 */
  const startApkUpdate = async () => {
    if (!latest || busy) return;
    const url = apkUrl(latest);
    if (!url) {
      setHint("这个版本没有 Android 安装包");
      return;
    }
    setBusy(true);
    setHint("");
    setFlow({ phase: "downloading", received: 0, total: -1 });
    pollRef.current = window.setInterval(() => {
      void apkDownloadProgress()
        .then((p) =>
          setFlow((f) =>
            f.phase === "downloading"
              ? { phase: "downloading", received: p.receivedBytes, total: p.totalBytes }
              : f,
          ),
        )
        .catch(() => {});
    }, 300);
    try {
      const dl = await downloadUpdateApk(url);
      // 权限先问一次：被拒直接给引导，省得 install 白拒一次（查询失败就当有权限，走 install 自己判）
      if (!(await checkInstallPermission().catch(() => true))) {
        setFlow({ phase: "ready", name: dl.name, denied: true });
        return;
      }
      await launchInstall(dl.name);
    } catch (e) {
      setFlow({ phase: "idle" });
      setHint(`${errText(e)}${OR_BROWSER}`);
    } finally {
      window.clearInterval(pollRef.current);
      setBusy(false);
    }
  };

  /** 桌面：tauri-plugin-updater 下载 → 校验签名 → 安装重启 */
  const installDesktop = async () => {
    if (!latest || busy) return;
    setBusy(true);
    setHint("");
    try {
      setHint("正在下载，完成后会自动安装并重启…");
      const { invoke } = await import("@tauri-apps/api/core");
      const updated = await invoke<boolean>("install_update", {
        server: getServer(),
      });
      if (!updated) setHint("这个平台暂时没有可用的安装包");
    } catch (e) {
      setHint(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const pct = flow.phase === "downloading" ? apkProgressPercent(flow.received, flow.total) : null;

  return (
    <SetRow
      label="应用更新"
      desc={
        <>
          当前版本 {current}
          {latest && ` · 最新版本 ${latest.version}`}
          {hasNew && notes && <div style={{ whiteSpace: "pre-wrap" }}>{notes}</div>}
          {checked && !hasNew && (
            <div>{!latest ? "暂时无法获取更新信息" : newer ? "这个平台暂时没有可用的安装包" : isNewer(current, latest.version) ? "当前版本高于最新发布版本" : "已是最新版本"}</div>
          )}
          {android && flow.phase === "downloading" && (
            <div>
              {pct !== null && (
                <div className="bk-progress">
                  <div className="bk-progress-bar" style={{ width: `${pct}%` }} />
                </div>
              )}
              {apkProgressText(flow.received, flow.total)}
            </div>
          )}
          {hint && <div>{hint}</div>}
          {android && flow.phase === "ready" && flow.denied && (
            <div>
              未开启「安装未知应用」权限：
              <button onClick={() => void openInstallPermissionSettings().catch((e) => setHint(errText(e)))}>
                去开启
              </button>
              ，在系统设置中允许本应用安装应用，返回后点「安装」继续。
            </div>
          )}
          {/* 一次性卸载提示常驻（0.6.2 / 签名不一致的 0.7.0 设备要用）；浏览器下载是兜底 */}
          {android && hasNew && (
            <div>
              <a
                href="#"
                onClick={(e) => {
                  e.preventDefault();
                  void openInBrowser();
                }}
              >
                用浏览器下载
              </a>
              {flow.phase === "ready" && (
                <>
                  {" · "}
                  <a
                    href="#"
                    onClick={(e) => {
                      e.preventDefault();
                      void startApkUpdate();
                    }}
                  >
                    重新下载
                  </a>
                </>
              )}
              <span>{` · ${UNINSTALL_ONCE}`}</span>
            </div>
          )}
        </>
      }
    >
      {!android && (
        <button disabled={checking || busy} onClick={() => void checkUpdate()}>
          {checking ? "正在检查…" : "检查更新"}
        </button>
      )}
      {hasNew && macPackage && (
        <button className="primary" onClick={() => {
          void import("@tauri-apps/plugin-opener")
            .then(({ openUrl }) => openUrl(downloadUrl(macPackage)))
            .catch((e) => setHint(errText(e)));
        }}>
          下载 macOS 版
        </button>
      )}
      {hasNew &&
        !macPackage &&
        (android ? (
          flow.phase === "downloading" ? (
            <button className="primary" disabled>
              下载中{pct === null ? "…" : ` ${pct}%`}
            </button>
          ) : flow.phase === "ready" ? (
            <button
              className="primary"
              disabled={busy}
              onClick={() => void launchInstall(flow.name)}
            >
              安装
            </button>
          ) : (
            <button className="primary" disabled={busy} onClick={() => void startApkUpdate()}>
              下载安装
            </button>
          )
        ) : (
          <button className="primary" disabled={busy} onClick={() => void installDesktop()}>
            更新并重启
          </button>
        ))}
    </SetRow>
  );
}
