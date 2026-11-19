// @vitest-environment happy-dom
import { expect, it, vi } from "vitest";
import html from "../index.html?raw";
import config from "../src-tauri/tauri.conf.json";
import macConfig from "../src-tauri/tauri.macos.conf.json";

it("reveals the desktop window from the loading HTML without the React entry point", () => {
  const page = new DOMParser().parseFromString(html, "text/html");
  const placeholder = page.querySelector("#startup");
  const reveal = placeholder?.parentElement?.nextElementSibling;
  expect(placeholder?.textContent).toBe("正在启动 Chat…");
  expect(reveal?.tagName).toBe("SCRIPT");
  expect(reveal?.nextElementSibling?.getAttribute("type")).toBe("module");

  const run = new Function("window", "navigator", reveal!.textContent!);
  const invoke = vi.fn().mockResolvedValue(undefined);
  const native = { __TAURI_INTERNALS__: { invoke } };
  run({}, { userAgent: "Linux" });
  run(native, { userAgent: "Android" });
  expect(invoke).not.toHaveBeenCalled();
  for (const userAgent of ["Windows NT", "Linux", "Mac OS X"]) {
    run(native, { userAgent });
  }
  expect(invoke.mock.calls).toEqual(Array.from({ length: 3 }, () => ["show_startup_window"]));
});

it("uses the macOS window background before the loading HTML paints", () => {
  expect(config.app.macOSPrivateApi).toBe(true);
  expect(macConfig.app.windows[0]).toMatchObject({
    visible: false,
    backgroundColor: "#f8fafc",
  });
});
