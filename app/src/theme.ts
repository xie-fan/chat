export type ThemeChoice = "system" | "light" | "dark";
export type AccentChoice = "indigo" | "emerald" | "ocean";

export const THEME_KEY = "chat_theme";
export const ACCENT_KEY = "chat_accent";

export interface ThemeStorage {
  getItem(k: string): string | null;
}

export function resolveDataTheme(choice: string | null): "light" | "dark" | undefined {
  return choice === "light" || choice === "dark" ? choice : undefined;
}

export function readThemeChoice(storage: ThemeStorage): ThemeChoice {
  try {
    const raw = storage.getItem(THEME_KEY);
    if (raw === "light" || raw === "dark" || raw === "system") return raw;
  } catch {}
  return "system";
}

export function readAccentChoice(storage: ThemeStorage): AccentChoice {
  try {
    const raw = storage.getItem(ACCENT_KEY);
    if (raw === "indigo" || raw === "emerald" || raw === "ocean") return raw;
  } catch {}
  return "indigo";
}

/** 判别当前实际渲染出的深浅色状态（system 跟随 matchMedia）。 */
export function resolveRenderedDark(
  choice: ThemeChoice,
  prefersDark = typeof window !== "undefined" &&
    !!window.matchMedia?.("(prefers-color-scheme: dark)").matches,
): boolean {
  if (choice === "dark") return true;
  if (choice === "light") return false;
  return prefersDark;
}

/** 根据当前已渲染模式反转目标模式（MWG dark-mode-toggle：反转 rendered scheme）。 */
export function nextQuickTheme(choice: ThemeChoice, prefersDark: boolean): "light" | "dark" {
  return resolveRenderedDark(choice, prefersDark) ? "light" : "dark";
}

let systemUnsub: (() => void) | null = null;

function syncDomTheme(choice: ThemeChoice) {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  const fixed = resolveDataTheme(choice);
  if (fixed) root.dataset.theme = fixed;
  else delete root.dataset.theme;

  const isDark = resolveRenderedDark(choice);
  root.classList.toggle("dark", isDark);
  // MWG dark-mode：显式声明 html color-scheme，让原生滚动条/表单/控件始终匹配渲染主题
  root.style.colorScheme = fixed ?? "light dark";
  const csMeta = document.querySelector('meta[name="color-scheme"]');
  if (csMeta) csMeta.setAttribute("content", fixed ?? "light dark");
  const tcMeta = document.querySelector('meta[name="theme-color"]');
  if (tcMeta) tcMeta.setAttribute("content", isDark ? "#090d16" : "#f3f5f9");
}

export function applyTheme(choice: ThemeChoice): void {
  if (systemUnsub) {
    systemUnsub();
    systemUnsub = null;
  }
  syncDomTheme(choice);
  if (choice === "system" && typeof window !== "undefined" && window.matchMedia) {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => syncDomTheme("system");
    mq.addEventListener("change", onChange);
    systemUnsub = () => mq.removeEventListener("change", onChange);
  }
}

export function applyAccent(accent: AccentChoice): void {
  if (typeof document === "undefined") return;
  document.documentElement.dataset.accent = accent;
}

export function setThemeChoice(choice: ThemeChoice): void {
  try {
    localStorage.setItem(THEME_KEY, choice);
  } catch {}
  applyTheme(choice);
}

export function setAccentChoice(accent: AccentChoice): void {
  try {
    localStorage.setItem(ACCENT_KEY, accent);
  } catch {}
  applyAccent(accent);
}

export function toggleQuickTheme(): "light" | "dark" {
  const cur = readThemeChoice(localStorage);
  const prefersDark =
    typeof window !== "undefined" && !!window.matchMedia?.("(prefers-color-scheme: dark)").matches;
  const next = nextQuickTheme(cur, prefersDark);
  setThemeChoice(next);
  return next;
}
