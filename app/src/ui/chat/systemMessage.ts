/**
 * 群系统消息（UI3）：kind=system，content 为 JSON
 * `{ code, text, actor_name?, group_name?, invitee_names?, target_name? }`。
 * 客户端按 code 映射灰条文案；解析失败时回落 text / 原文。
 */

export const SYSTEM_CODES = [
  "group_created",
  "group_renamed",
  "group_announcement",
  "group_invited",
  "group_left",
  "group_removed",
  "group_dissolved",
] as const;

export type SystemCode = (typeof SYSTEM_CODES)[number];

export interface SystemPayload {
  code: string;
  text?: string;
  actor_name?: string;
  group_name?: string;
  invitee_names?: string[];
  target_name?: string;
}

export function isSystemCode(c: string): c is SystemCode {
  return (SYSTEM_CODES as readonly string[]).includes(c);
}

/** 解析 content；非 JSON 或缺 code 返回 null */
export function parseSystemPayload(content: string): SystemPayload | null {
  const raw = content.trim();
  if (!raw.startsWith("{")) return null;
  try {
    const v = JSON.parse(raw) as unknown;
    if (!v || typeof v !== "object") return null;
    const o = v as Record<string, unknown>;
    if (typeof o.code !== "string" || !o.code) return null;
    const invitee_names = Array.isArray(o.invitee_names)
      ? o.invitee_names.filter((x): x is string => typeof x === "string")
      : undefined;
    return {
      code: o.code,
      text: typeof o.text === "string" ? o.text : undefined,
      actor_name: typeof o.actor_name === "string" ? o.actor_name : undefined,
      group_name: typeof o.group_name === "string" ? o.group_name : undefined,
      invitee_names,
      target_name: typeof o.target_name === "string" ? o.target_name : undefined,
    };
  } catch {
    return null;
  }
}

/** 灰条展示文案（优先按 code + params 本地化，再回落 text） */
export function systemBarText(content: string): string {
  const p = parseSystemPayload(content);
  if (!p) return content;
  const actor = (p.actor_name ?? "").trim() || "有人";
  switch (p.code) {
    case "group_created":
      return p.group_name
        ? `${actor} 创建了群聊`
        : `${actor} 创建了群聊`;
    case "group_renamed":
      return p.group_name
        ? `${actor} 将群名改为「${p.group_name}」`
        : `${actor} 修改了群名`;
    case "group_announcement":
      return `${actor} 更新了群公告`;
    case "group_invited": {
      const names = (p.invitee_names ?? []).filter(Boolean);
      return names.length
        ? `${actor} 邀请 ${names.join("、")} 加入了群聊`
        : `${actor} 邀请成员加入了群聊`;
    }
    case "group_left":
      return `${actor} 退出了群聊`;
    case "group_removed": {
      const target = (p.target_name ?? "").trim();
      return target ? `${actor} 将 ${target} 移出了群聊` : `${actor} 移出了一名成员`;
    }
    case "group_dissolved":
      return `${actor} 解散了群聊`;
    default:
      return p.text?.trim() || content;
  }
}

/** 会话列表副标题：系统条不带「发送者：」前缀 */
export function systemPreviewText(content: string): string {
  return systemBarText(content);
}
