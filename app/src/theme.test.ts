import { describe, expect, it } from "vitest";
import {
  nextQuickTheme,
  readAccentChoice,
  readThemeChoice,
  resolveDataTheme,
  resolveRenderedDark,
  ACCENT_KEY,
  THEME_KEY,
} from "./theme";

function memStorage(initial: Record<string, string> = {}) {
  const m = new Map(Object.entries(initial));
  return { getItem: (k: string) => (m.has(k) ? m.get(k)! : null) };
}

// ===== V2 P1-3 + UI Redesign =====
describe("主题解析（存储值 → data-theme / accent）", () => {
  it.each([
    ["light", "light"],
    ["dark", "dark"],
    ["system", undefined],
    [null, undefined],
    ["", undefined],
    ["blue", undefined],
  ])("resolveDataTheme(%s) → %s", (input, expected) => {
    expect(resolveDataTheme(input)).toBe(expected);
  });

  it("readThemeChoice：合法值原样返回，缺失/非法/异常 → system", () => {
    expect(readThemeChoice(memStorage({ [THEME_KEY]: "dark" }))).toBe("dark");
    expect(readThemeChoice(memStorage({ [THEME_KEY]: "light" }))).toBe("light");
    expect(readThemeChoice(memStorage({ [THEME_KEY]: "system" }))).toBe("system");
    expect(readThemeChoice(memStorage())).toBe("system");
    expect(readThemeChoice(memStorage({ [THEME_KEY]: "junk" }))).toBe("system");
    expect(
      readThemeChoice({
        getItem: () => { throw new Error("denied"); },
      }),
    ).toBe("system");
  });

  it("readAccentChoice：支持 indigo / emerald / ocean，其余回退 indigo", () => {
    expect(readAccentChoice(memStorage({ [ACCENT_KEY]: "emerald" }))).toBe("emerald");
    expect(readAccentChoice(memStorage({ [ACCENT_KEY]: "ocean" }))).toBe("ocean");
    expect(readAccentChoice(memStorage({ [ACCENT_KEY]: "unknown" }))).toBe("indigo");
    expect(readAccentChoice(memStorage())).toBe("indigo");
  });

  it("nextQuickTheme：反转当前实际渲染的深浅状态", () => {
    expect(resolveRenderedDark("system", true)).toBe(true);
    expect(nextQuickTheme("system", true)).toBe("light");
    expect(nextQuickTheme("system", false)).toBe("dark");
    expect(nextQuickTheme("dark", false)).toBe("light");
    expect(nextQuickTheme("light", true)).toBe("dark");
  });
});
