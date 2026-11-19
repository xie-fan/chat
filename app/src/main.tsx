import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { isDesktopShell, osOfUa, watchIdentity } from "./api";
import { installFileDropGuard } from "./fileDrop";
import { flushDrafts } from "./store";
import { applyAccent, applyTheme, readAccentChoice, readThemeChoice } from "./theme";
import "./styles.css";

installFileDropGuard(window);

// 主题在 React 渲染前应用，避免先白后黑闪一下（system = 不设 data-theme，媒体查询接管）
applyTheme(readThemeChoice(localStorage));
applyAccent(readAccentChoice(localStorage));
// Web 端最小 app-shell：服务端停止时用浏览器缓存仍能打开（/v1 API 不缓存；Tauri 壳不注册）
if (
  import.meta.env.PROD &&
  "serviceWorker" in navigator &&
  !("__TAURI_INTERNALS__" in globalThis)
) {
  window.addEventListener("load", () => {
    navigator.serviceWorker.register("/sw.js").catch(() => {});
  });
}
// 页面卸载/隐藏时把在途防抖草稿立即落盘
window.addEventListener("pagehide", flushDrafts);
// 别的标签页换了账号 / 服务器 / 登出：本页的界面、缓存、WS 都属于旧身份，整页重载最干净
watchIdentity(() => location.reload());

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);

if (isDesktopShell() && osOfUa(navigator.userAgent) === "macOS") {
  requestAnimationFrame(() => {
    requestAnimationFrame(() => {
      void import("@tauri-apps/api/core").then(({ invoke }) => invoke("show_startup_window")).catch(() => {});
    });
  });
}
