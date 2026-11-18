import { useState } from "react";
import {
  readAccentChoice,
  readThemeChoice,
  setAccentChoice,
  setThemeChoice,
  type AccentChoice,
  type ThemeChoice,
} from "../../theme";
import SetRow from "./SetRow";

const THEME_OPTIONS: { v: ThemeChoice; label: string }[] = [
  { v: "system", label: "跟随系统" },
  { v: "light", label: "浅色" },
  { v: "dark", label: "深色" },
];

const ACCENT_OPTIONS: { v: AccentChoice; label: string; color: string }[] = [
  { v: "indigo", label: "靛蓝", color: "#4F46E5" },
  { v: "emerald", label: "翡翠绿", color: "#059669" },
  { v: "ocean", label: "深海蓝", color: "#0284C7" },
];

/** 外观与主题色设置：亮/暗/跟随系统 + 3 套强调色方案 */
export default function ThemeSetting() {
  const [choice, setChoice] = useState<ThemeChoice>(() => readThemeChoice(localStorage));
  const [accent, setAccent] = useState<AccentChoice>(() => readAccentChoice(localStorage));

  const pickTheme = (v: ThemeChoice) => {
    setChoice(v);
    setThemeChoice(v);
  };

  const pickAccent = (a: AccentChoice) => {
    setAccent(a);
    setAccentChoice(a);
  };

  return (
    <>
      <SetRow label="外观模式">
        <div className="seg" role="radiogroup" aria-label="外观模式">
          {THEME_OPTIONS.map((o) => (
            <label key={o.v} className={`seg-opt${choice === o.v ? " on" : ""}`}>
              <input
                type="radio"
                name="theme"
                checked={choice === o.v}
                onChange={() => pickTheme(o.v)}
              />
              {o.label}
            </label>
          ))}
        </div>
      </SetRow>

      <SetRow label="主题强调色" desc="影响按钮、激活状态与我方聊天气泡">
        <div className="accent-pills" role="radiogroup" aria-label="主题强调色">
          {ACCENT_OPTIONS.map((a) => (
            <button
              key={a.v}
              type="button"
              role="radio"
              aria-checked={accent === a.v}
              className={`accent-pill${accent === a.v ? " on" : ""}`}
              onClick={() => pickAccent(a.v)}
            >
              <span className="accent-dot" style={{ backgroundColor: a.color }} />
              <span>{a.label}</span>
            </button>
          ))}
        </div>
      </SetRow>
    </>
  );
}
