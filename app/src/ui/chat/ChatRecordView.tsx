import type { AttachmentView, MessageView, RecordItem } from "../../types";
import RichText from "../../richText";
import Avatar from "../Avatar";
import AttachmentImage from "./AttachmentImage";
import AudioBubble from "./AudioBubble";
import { imageBubbleSize } from "../mediaFormat";
import {
  recordCardLines,
  recordItemTime,
  recordSections,
  recordTitle,
} from "./chatRecord";
import { Z } from "./layers";

// ===== MF2 聊天记录卡片 + 查看页 =====
// 快照（RecordItem/嵌套 record）只活在本组件 props/调用方 state 里：
// 不进全局 store、不落 localStorage/IndexedDB、不打日志。

/**
 * 会话气泡里的聊天记录卡片：粗体标题 + 至多 4 行「发送者：摘要」+ 分隔线 + 页脚。
 * 交互（MF2-C1）：左键/回车/触屏单点 → 打开查看页；右键 → 消息菜单；
 * 「⋯」按钮 → 动作面板（stopPropagation，避免落到卡片 onClick）。
 * 多选模式下点击被行级捕获层拦截成勾选，这里不会收到事件。
 */
export function ChatRecordCard({
  m,
  onOpen,
  onMore,
}: {
  m: MessageView;
  onOpen: () => void;
  onMore: () => void;
}) {
  const title = recordTitle(m);
  const lines = recordCardLines(m.record_preview);
  return (
    <div
      className="record-card"
      data-testid="record-card"
      role="button"
      tabIndex={0}
      data-own-action
      onClick={(e) => {
        e.stopPropagation();
        onOpen();
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          onOpen();
        }
      }}
    >
      <div className="rc-title" title={title}>
        {title}
      </div>
      <div className="rc-lines">
        {lines.map((l, i) => (
          <div key={i} className="rc-line">
            <span className="rc-sender">{l.sender}</span>
            <span className="rc-sep">：</span>
            <span className="rc-text">{l.text}</span>
          </div>
        ))}
      </div>
      <div className="rc-divider" />
      <div className="rc-foot">
        <span className="rc-tag">聊天记录</span>
        {/* 移动端没有长按菜单（MM2 留给系统文本选择）：卡片自带 ⋯ 入口开动作面板 */}
        <button
          type="button"
          className="record-more"
          data-testid="record-more"
          aria-label="更多操作"
          onClick={(e) => {
            e.stopPropagation();
            onMore();
          }}
        >
          ⋯
        </button>
      </div>
    </div>
  );
}

export interface RecordLevel {
  title: string;
  items: RecordItem[];
}

export interface RecordViewState {
  /** 最外层记录消息 id：拉快照与记录内附件转发都用它（嵌套层里也要回指最外层） */
  outerId: string;
  levels: RecordLevel[];
  loading: boolean;
  /** 403/404/网络错误/打开后被撤回 → 「聊天记录不可用」 */
  error: boolean;
}

/** 单条快照条目：头像 + 名字 + 时间 + 按 kind 渲染正文 */
function RecordRow({
  it,
  onPreview,
  onNested,
}: {
  it: RecordItem;
  onPreview: (att: AttachmentView) => void;
  onNested: (rec: NonNullable<RecordItem["record"]>) => void;
}) {
  const att = it.attachment;
  let body: React.ReactNode = null;
  let clickable = false;
  let onOpen: (() => void) | undefined;

  if (it.kind === "chat_record" && it.record) {
    body = (
      <div className="rv-nested">
        <b>{it.record.title}</b>
        <i>聊天记录 · {it.record.count} 条</i>
      </div>
    );
    clickable = true;
    onOpen = () => onNested(it.record!);
  } else if (att && it.kind === "image") {
    const dim =
      att.width != null && att.height != null ? imageBubbleSize(att.width, att.height) : null;
    body = (
      <AttachmentImage
        className="rv-img"
        attachment={att}
        style={dim && dim.width ? { width: dim.width, height: dim.height } : undefined}
        fallback={<span className="rv-att-ph">🖼 {att.file_name}</span>}
      />
    );
    clickable = true;
    onOpen = () => onPreview(att);
  } else if (att && it.kind === "audio") {
    // 语音：行内播放（本期无下载/转发工具栏项）
    body = <AudioBubble att={att} />;
  } else if (att && it.kind === "file") {
    body = (
      <span className="rv-file">
        <span className="file-ic">📄</span>
        <span className="file-meta">
          <b>{att.file_name}</b>
          <i>{att.mime}</i>
        </span>
      </span>
    );
    clickable = true;
    onOpen = () => onPreview(att);
  } else if (att) {
    // 其它带附件 kind：展示文件名入口（按预览打开）
    body = <span className="rv-file">📎 {att.file_name}</span>;
    clickable = true;
    onOpen = () => onPreview(att);
  } else {
    // attachment: null（含 text/附件缺失）只渲染文字，不可点
    body = it.content ? <RichText text={it.content} mentions={false} /> : null;
  }

  return (
    <div
      className={`rv-item ${clickable ? "clickable" : ""}`}
      onClick={onOpen}
      {...(clickable ? { "data-own-action": true } : {})}
    >
      <Avatar
        userId={it.sender_id}
        name={it.sender_name}
        avatarVersion={it.avatar_version}
        hasAvatar={it.has_avatar}
        size="sm"
      />
      <div className="rv-item-main">
        <div className="rv-item-meta">
          <span className="rv-sender">{it.sender_name}</span>
          <span className="rv-time">{recordItemTime(it.created_at)}</span>
        </div>
        <div className="rv-item-body">{body}</div>
      </div>
    </div>
  );
}

/**
 * 记录查看页：移动端整屏 / 桌面端居中面板。levels 逐层下钻；
 * record-close 关整页，record-back 弹一层。Esc/返回键层级在 ChatView 编排。
 */
export default function ChatRecordView({
  view,
  onClose,
  onPopLevel,
  onPushLevel,
  onPreview,
}: {
  view: RecordViewState;
  onClose: () => void;
  onPopLevel: () => void;
  onPushLevel: (rec: NonNullable<RecordItem["record"]>) => void;
  /** 打开记录内附件的查看器/预览（调用方给它挂 record 上下文工具栏） */
  onPreview: (att: AttachmentView) => void;
}) {
  const top = view.levels[view.levels.length - 1];
  return (
    // Z.recordView：查看页在一切弹窗之下（记录内预览/转发弹窗都要压过它）
    <div className="rv-mask" style={{ zIndex: Z.recordView }} onClick={onClose}>
      <div className="record-view" data-testid="record-view" onClick={(e) => e.stopPropagation()}>
        <div className="rv-head">
          {view.levels.length > 1 && (
            <button type="button" className="icon-btn rv-back" data-testid="record-back" onClick={onPopLevel} aria-label="返回上一层">
              ‹
            </button>
          )}
          <span className="rv-title">{view.error ? "聊天记录" : (top?.title ?? "聊天记录")}</span>
          <button
            type="button"
            className="icon-btn"
            data-testid="record-close"
            aria-label="关闭"
            onClick={onClose}
          >
            ✕
          </button>
        </div>
        <div className="rv-body">
          {view.error ? (
            <div className="rv-err" data-testid="record-unavailable">
              聊天记录不可用
            </div>
          ) : view.loading ? (
            <div className="rv-err">加载中…</div>
          ) : (
            top &&
            recordSections(top.items).map((sec) => (
              <div key={sec.dayKey}>
                {sec.dayLabel && <div className="rv-day">{sec.dayLabel}</div>}
                {sec.items.map((it, i) => (
                  <RecordRow
                    key={i}
                    it={it}
                    onPreview={onPreview}
                    onNested={onPushLevel}
                  />
                ))}
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
