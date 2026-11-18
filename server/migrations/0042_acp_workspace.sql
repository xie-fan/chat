-- 1. 移除对话归档入口：将历史归档状态清空，所有会话回到主列表
UPDATE conversation_preferences SET archived_at = NULL WHERE archived_at IS NOT NULL;

-- 2. ACP 全局用户权限配置（默认策略：自动放行只读）
CREATE TABLE acp_user_settings (
    user_id            uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    default_permission text NOT NULL DEFAULT 'auto_read'
        CHECK (default_permission IN ('ask', 'auto_read', 'allow_all')),
    updated_at         timestamptz NOT NULL DEFAULT now()
);

-- 3. 受控机器节点（桌面端内置或 CLI --acp-host 接入，按 user_id + install_id 稳定识别）
CREATE TABLE acp_hosts (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    install_id uuid NOT NULL,
    name       text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 80),
    os         text NOT NULL DEFAULT '',
    ip         text NOT NULL DEFAULT '',
    roots_json jsonb NOT NULL DEFAULT '[]'::jsonb,
    online     boolean NOT NULL DEFAULT false,
    latency_ms integer NOT NULL DEFAULT 0,
    last_seen  timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (user_id, install_id)
);
CREATE INDEX idx_acp_hosts_user ON acp_hosts(user_id);

-- 4. 各机器上配置的 ACP Agent（支持预设与自定义启动命令、默认模型与独立权限覆盖）
CREATE TABLE acp_agents (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id             uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    acp_host_id         uuid NOT NULL REFERENCES acp_hosts(id) ON DELETE CASCADE,
    preset              text NOT NULL DEFAULT 'custom',
    name                text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 80),
    adapter             text NOT NULL DEFAULT '',
    cmd                 text NOT NULL CHECK (char_length(cmd) BETWEEN 1 AND 500),
    env_json            jsonb NOT NULL DEFAULT '{}'::jsonb,
    short_tag           text NOT NULL DEFAULT 'AG' CHECK (char_length(short_tag) BETWEEN 1 AND 6),
    badge_bg            text NOT NULL DEFAULT 'from-indigo-500 to-sky-500',
    default_model       text NOT NULL DEFAULT '',
    model_options       jsonb NOT NULL DEFAULT '[]'::jsonb,
    permission_override text NOT NULL DEFAULT 'inherit'
        CHECK (permission_override IN ('inherit', 'ask', 'auto_read', 'allow_all')),
    capabilities        jsonb NOT NULL DEFAULT '[]'::jsonb,
    status              text NOT NULL DEFAULT 'standby',
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX idx_acp_agents_host ON acp_agents(user_id, acp_host_id);

-- 5. 工作目录（Machine -> Working Directory，一等公民）
CREATE TABLE acp_workspaces (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    acp_host_id uuid NOT NULL REFERENCES acp_hosts(id) ON DELETE CASCADE,
    cwd         text NOT NULL CHECK (char_length(cwd) BETWEEN 1 AND 500),
    name        text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 120),
    branch      text NOT NULL DEFAULT '',
    branches    jsonb NOT NULL DEFAULT '[]'::jsonb,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (user_id, acp_host_id, cwd)
);
CREATE INDEX idx_acp_workspaces_host ON acp_workspaces(user_id, acp_host_id);

-- 6. 工作目录下的 Agent 多会话（Working Directory -> Session）
CREATE TABLE acp_sessions (
    id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id           uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    acp_workspace_id  uuid NOT NULL REFERENCES acp_workspaces(id) ON DELETE CASCADE,
    acp_agent_id      uuid NOT NULL REFERENCES acp_agents(id) ON DELETE CASCADE,
    title             text NOT NULL CHECK (char_length(title) BETWEEN 1 AND 160),
    selected_model    text NOT NULL DEFAULT '',
    thinking_level    text NOT NULL DEFAULT '中'
        CHECK (thinking_level IN ('关', '低', '中', '高')),
    permission_policy text NOT NULL DEFAULT 'auto_read'
        CHECK (permission_policy IN ('ask', 'auto_read', 'allow_all')),
    acp_session_key   text,
    running           boolean NOT NULL DEFAULT false,
    diffs_json        jsonb NOT NULL DEFAULT '[]'::jsonb,
    rpc_logs_json     jsonb NOT NULL DEFAULT '[]'::jsonb,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX idx_acp_sessions_ws ON acp_sessions(user_id, acp_workspace_id, updated_at DESC);

-- 7. 会话事件流（user / agent_plan / permission / terminal_out / agent_text）
CREATE TABLE acp_turns (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    acp_session_id uuid NOT NULL REFERENCES acp_sessions(id) ON DELETE CASCADE,
    user_id        uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    seq            bigint NOT NULL,
    role           text NOT NULL CHECK (role IN ('user', 'agent_plan', 'permission', 'terminal_out', 'agent_text')),
    status         text NOT NULL DEFAULT 'done',
    payload        jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (acp_session_id, seq)
);
CREATE INDEX idx_acp_turns_session ON acp_turns(acp_session_id, seq);

-- 8. ACP 会话附带的本地上传文件 / 截图
CREATE TABLE acp_attachments (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id        uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    acp_session_id uuid REFERENCES acp_sessions(id) ON DELETE CASCADE,
    file_name      text NOT NULL CHECK (char_length(file_name) BETWEEN 1 AND 255),
    mime           text NOT NULL DEFAULT 'application/octet-stream',
    size           bigint NOT NULL CHECK (size >= 0),
    storage_path   text NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX idx_acp_attachments_user ON acp_attachments(user_id, created_at DESC);
