import { useEffect, useRef, useState } from "react";
import { files } from "../../api";
import type { AttachmentView } from "../../types";
import { useAttachmentAccess } from "./attachmentAccess";
import { audioBubbleWidth, formatVoiceDuration } from "./voice";

/** 同一时间只播放一条语音。 */
let current: HTMLAudioElement | null = null;

function pauseOthers(mine: HTMLAudioElement) {
  if (current && current !== mine) current.pause();
  current = mine;
}

export default function AudioBubble({ att, recalledUrl }: { att: AttachmentView; recalledUrl?: string }) {
  const audioRef = useRef<HTMLAudioElement>(null);
  const acquiring = useRef(false);
  const access = useAttachmentAccess(att.recalled ? undefined : att.id);
  const [playing, setPlaying] = useState(false);
  const [progress, setProgress] = useState(0);
  const [error, setError] = useState(false);
  const duration = att.duration_ms ?? 0;

  useEffect(() => {
    const el = audioRef.current;
    if (!el) return;
    const onTime = () => {
      const total = duration > 0 ? duration / 1000 : el.duration;
      setProgress(total > 0 && Number.isFinite(total) ? el.currentTime / total : 0);
    };
    const onEnd = () => {
      setPlaying(false);
      setProgress(0);
      if (current === el) current = null;
    };
    const onPause = () => setPlaying(false);
    el.addEventListener("timeupdate", onTime);
    el.addEventListener("ended", onEnd);
    el.addEventListener("pause", onPause);
    return () => {
      el.pause();
      el.removeEventListener("timeupdate", onTime);
      el.removeEventListener("ended", onEnd);
      el.removeEventListener("pause", onPause);
      if (current === el) current = null;
    };
  }, [duration, att.id]);

  const toggle = async () => {
    const el = audioRef.current;
    if (!el || acquiring.current) return;
    if (!el.paused) {
      el.pause();
      return;
    }
    const cached = access && access.expiresAt > Date.now() ? access.url : undefined;
    acquiring.current = true;
    setError(false);
    try {
      // 通常已有预取的票据，可在点击事件内直接调用 play。停留过久则临时续签。
      let url = att.recalled ? recalledUrl : cached;
      if (!att.recalled && !url) url = (await files.access(att.id)).url;
      if (!url || audioRef.current !== el) return;
      if (el.src !== url) {
        const time = el.currentTime;
        el.src = url;
        if (time > 0) el.currentTime = time;
      }
      pauseOthers(el);
      await el.play();
      if (audioRef.current === el) setPlaying(true);
    } catch {
      if (audioRef.current === el) {
        setPlaying(false);
        setError(true);
      }
    } finally {
      acquiring.current = false;
    }
  };

  return (
    <div className="voice-bubble" style={{ width: audioBubbleWidth(duration) }} data-voice="1">
      <button
        type="button"
        className="voice-play"
        aria-label={playing ? "暂停语音" : "播放语音"}
        title={error ? "语音暂时无法播放，请重试" : undefined}
        onClick={() => void toggle()}
      >
        {playing ? "暂停" : "播放"}
      </button>
      <span className="voice-track" aria-hidden>
        <i style={{ width: `${Math.min(100, progress * 100)}%` }} />
      </span>
      <span className="voice-dur">{formatVoiceDuration(duration, "bubble")}</span>
      <audio key={att.id} ref={audioRef} preload="none" />
    </div>
  );
}
