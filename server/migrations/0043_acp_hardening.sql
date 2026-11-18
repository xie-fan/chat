-- 0043: 补充 ACP 外键/清理查询索引，移除与 UNIQUE(acp_session_id, seq) 重复的索引
CREATE INDEX IF NOT EXISTS idx_acp_sessions_agent ON acp_sessions(acp_agent_id);
CREATE INDEX IF NOT EXISTS idx_acp_turns_user ON acp_turns(user_id);
CREATE INDEX IF NOT EXISTS idx_acp_attachments_session ON acp_attachments(acp_session_id);
DROP INDEX IF EXISTS idx_acp_turns_session_seq;
