import { useEffect, useRef, useState } from "react";
import { files, searchApi } from "../../api";
import { saveAttachment } from "../../download";
import type { AttachmentView, ConversationView, MessageView } from "../../types";
import AudioBubble from "./AudioBubble";

// ===== V2 P3-1 =====
// 会话「文件与图片」面板：复用 /v1/search?conversation_id=&kind= 拉附件消息。
// 图片网格点缩略图走外层预览弹窗；文件行内下载；语音复用 AudioBubble。
// 滚到底部用 before/before_id 游标翻页。

function fmtSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

type Tab = "image" | "file" | "audio";

const TABS: { v: Tab; label: string }[] = [
  { v: "image", label: "图片" },
  { v: "file", label: "文件" },
  { v: "audio", label: "语音" },
];

export default function ConvMedia({
  conv,
  onPreview,
}: {
  conv: ConversationView;
  /** 点图片缩略图或文件行：交给外层（ChatView）打开预览 */
  onPreview?: (att: AttachmentView, m?: MessageView) => void;
}) {
  const [tab, setTab] = useState<Tab>("image");
  const [items, setItems] = useState<MessageView[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const reqRef = useRef(0);
  const scrollRef = useRef<HTMLDivElement>(null);

  const load = (t: Tab, page?: { before: string; before_id: string }) => {
    const req = ++reqRef.current;
    setLoading(true);
    setErr(null);
    searchApi
      .messages({ conversation_id: conv.id, kind: t, limit: 30, ...page })
      .then((p) => {
        if (req !== reqRef.current) return;
        setItems((cur) => {
          const seen = new Set(cur.map((m) => m.id));
          const base = page ? cur : [];
          return [...base, ...p.messages.filter((m) => !seen.has(m.id))];
        });
        setHasMore(p.has_more);
      })
      .catch((e) => {
        if (req !== reqRef.current) return;
        setErr(e instanceof Error ? e.message : "加载失败");
      })
      .finally(() => {
        if (req === reqRef.current) setLoading(false);
      });
  };

  // 切会话/切页签都回到第一页
  useEffect(() => {
    setItems([]);
    setHasMore(false);
    scrollRef.current?.scrollTo({ top: 0 });
    load(tab);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [conv.id, tab]);

  const loadMore = () => {
    const last = items[items.length - 1];
    if (!last || !hasMore || loading) return;
    load(tab, { before: last.created_at, before_id: last.id });
  };

  const onScroll = () => {
    const el = scrollRef.current;
    if (el && el.scrollHeight - el.scrollTop - el.clientHeight < 30) loadMore();
  };

  const withAtt = items.filter((m) => m.attachment);

  return (
    <div className="cm-panel">
      <div className="cm-tabs" role="tablist">
        {TABS.map((t) => (
          <button
            key={t.v}
            role="tab"
            aria-selected={tab === t.v}
            className={`gs-chip ${tab === t.v ? "on" : ""}`}
            // 立即清空：旧页签的数据在新 kind 请求返回前不串台显示
            onClick={() => {
              setItems([]);
              setHasMore(false);
              setTab(t.v);
            }}
          >
            {t.label}
          </button>
        ))}
      </div>
      <div className="cm-body" ref={scrollRef} onScroll={onScroll}>
        {err && <div className="ctx-err">{err}</div>}
        {tab === "image" && (
          <div className="cm-grid">
            {withAtt.map((m) => (
              <img
                key={m.id}
                className="cm-thumb"
                src={files.url(m.attachment!.id, {
                  thumb: !!m.attachment!.has_thumb,
                })}
                alt={m.attachment!.file_name}
                title={m.attachment!.file_name}
                onClick={() => onPreview?.(m.attachment!, m)}
              />
            ))}
          </div>
        )}
        {tab === "file" &&
          withAtt.map((m) => (
            <div
              key={m.id}
              className="cm-file"
              style={{ cursor: onPreview ? "pointer" : undefined }}
              onClick={() => onPreview?.(m.attachment!, m)}
            >
              {onPreview ? (
                <button
                  type="button"
                  className="cm-file-open"
                  onClick={(e) => {
                    e.stopPropagation();
                    onPreview(m.attachment!, m);
                  }}
                >
                  <span className="file-ic">📄</span>
                  <span className="file-meta">
                    <b title={m.attachment!.file_name}>{m.attachment!.file_name}</b>
                    <i>
                      {fmtSize(m.attachment!.size)} · {m.sender_name} ·{" "}
                      {new Date(m.created_at).toLocaleDateString()}
                    </i>
                  </span>
                </button>
              ) : (
                <>
                  <span className="file-ic">📄</span>
                  <span className="file-meta">
                    <b title={m.attachment!.file_name}>{m.attachment!.file_name}</b>
                    <i>
                      {fmtSize(m.attachment!.size)} · {m.sender_name} ·{" "}
                      {new Date(m.created_at).toLocaleDateString()}
                    </i>
                  </span>
                </>
              )}
              <button
                type="button"
                className="cm-dl"
                title="下载"
                onClick={(e) => {
                  e.stopPropagation();
                  void saveAttachment(m.attachment!);
                }}
              >
                ⤓
              </button>
            </div>
          ))}
        {tab === "audio" &&
          withAtt.map((m) => (
            <div key={m.id} className="cm-audio">
              <div className="cm-audio-meta">
                {m.sender_name} · {new Date(m.created_at).toLocaleString()}
              </div>
              <AudioBubble att={m.attachment!} />
            </div>
          ))}
        {!loading && !err && withAtt.length === 0 && (
          <div className="cm-empty">暂无内容</div>
        )}
        {loading && <div className="msg-sys">加载中…</div>}
        {!loading && hasMore && <div className="msg-sys">下拉加载更多</div>}
      </div>
    </div>
  );
}
