# ADR-001：关键技术决策

## 1. 单 React 代码库 + Tauri 壳

**决策**：Web/Android/Windows 三端共用一套 React SPA；Tauri v2 打包桌面与安卓。

**理由**：避免三套 UI；Tauri 比 Electron 体积小一个数量级；登录页暴露「服务器地址」字段解决客户端→LAN 服务器寻址。

**代价**：Tauri Android 需要本机 SDK+NDK；iOS 不在范围。

## 2. REST + WebSocket 混合

**决策**：读写状态走 REST（好友、群管理、历史分页）；实时通道走 WS（消息收发、presence、typing、read、sync）。WS 用一次性 ticket（30s 有效、用后即焚）鉴权，不把长期 token 放进 query 之外的持久位置。

**理由**：消息高频双向 → WS 天然合适；好友/群操作低频且幂等语义强 → REST。ticket 隔离 WS 鉴权与 HTTP 鉴权，token 不出现在 URL 里被代理记录的风险最小化（ticket 是一次性的）。

## 3. 每会话单调 seq 做同步锚点

**决策**：`messages.seq` 由 Postgres `conversations.last_seq` 在事务内 `UPDATE ... RETURNING` 分配；断线重连用 `sync(last_seq)` 拉增量。

**理由**：比时间戳可靠（无并发/时钟问题）；客户端本地只需记住 last_seq；已读位置也用 seq（`memberships.last_read_seq`），未读数 = `last_seq - last_read_seq` 一条 SQL 算出。

## 4. 好友制而非开放 DM

**决策**：DM 只能发生在互为好友的用户间；删好友 → DM 会话保留但只读（WS 层在 `send_message` 时校验好友关系仍然成立）。

**理由**：符合「真实产品」的骚扰模型；只读而非删除保留了历史证据链。

## 5. 群组规则

- 只能拉自己的好友入群（同理反垃圾）。
- 群主退出 → 自动转让给最早加入的成员；最后一人退出 → 群解散（删会话）。
- 解散保留消息但全员移出（membership 删除，历史不可再写）。
- 踢人/改名/转让/解散仅群主（v1 无 admin 角色）。

## 6. 传输加密边界

**决策**：仅 TLS/WSS，不做 E2E 加密。本机/局域网部署时为 ws:// 明文——接受此风险，公网部署须挂 TLS（Caddy/nginx 反代或 axum rustls）。

## 7. postgresql + sqlx 编译期嵌入迁移

`sqlx::migrate!("./migrations")` 在编译期把 SQL 打进二进制，容器镜像无需拷贝 SQL 目录。开发库与测试库分离（开发库 `chat`；测试库自 MM5 起为每次运行自动建删的 `chat_it_run_<pid>_<rand>`，或 `TEST_DATABASE_NAME` 指定的用户自有库；`chat_t_*`/`chat_it_*` 每测试独立库由守卫自动删除）。

## 8. 多设备在线模型

`AppState.conns: user_id → {conn_id → sender}`。同一账号多连接并存；presence 只在「首连/末断」时翻转广播；read_sync 把已读状态推给同账号其他设备（清角标）。

## 9. 附件：文件系统存储 + 能力 URL

**决策**：附件二进制存服务端文件系统（`FILES_DIR`），元数据在 `attachments` 表（uploader/conv/mime/size/path）；消息经 `messages.kind` + `attachment_id` 引用；下载走 `GET /v1/files/{id}` 不鉴权。

**理由**：`GET /v1/files/{uuid}` 必须能用于 `<img>`/`<a>`（无法带 Authorization 头）；UUID 不可枚举，链接只在会话内流转——能力 URL 模型与主流 IM 的 CDN 链接一致。上传时校验「上传者是目标会话成员」+ 发送时校验「附件属本人、绑本会话、未被复用」，闭合权限边界。风险与升级路径：链接外泄即失密 → 公网部署可换签名 URL 或短时效 ticket。本地运行库为 `chat` + `./data/files`；测试库按上节自动建删（历史遗留的 `chat_test` 不再使用）。

## 10. @提及持久化 + 会话内搜索

**决策**：提及独立表 `message_mentions(message_id, user_id)`，发送时在服务端按群成员用户名解析 `@token`（去重、不含发送者、仅群聊）；`MessageView.mentions` 带 user_id 列表；`ConversationView.has_mention` = 存在 seq > last_read_seq 的 @我 记录，已读后自然消除。会话内搜索 `GET /conversations/{id}/messages/search?q=` 走 ILIKE 子串（通配符转义），复用 MsgViewRow 的 SELECT/JOIN。

**理由**：提及必须服务端解析——客户端自行猜测用户名会与改名/权限脱节；独立表比解析 content 更可查询（has_mention 一条 EXISTS 搞定）。搜索用 ILIKE 而非全文索引：消息量小、零运维成本，量上来再换 `tsvector` 或外部引擎。

## 11. 账号角色：首注册者引导 + 禁用即踢

**决策**：`users.role`（admin/user）+ `users.disabled_at`。最早注册用户自动成为管理员（Gitea 式零配置引导），之后管理员在后台互相任命/撤销；**末位保护**——最后一个管理员不可撤销；管理员账号不可被禁用。禁用 = 置标记 + 撤销全部 refresh token + `force_logout` 帧踢下线 + 每请求 `require_user` 查用户行拒绝（即时生效，不等 access token 过期）。

**理由**：自部署产品不想引入"谁是第一个 admin"的配置项；角色判断走 DB 而非 JWT claim——admin 撤销/禁用必须即时生效，JWT 15 分钟 TTL 内仍放行不可接受，代价是每请求一次主键查询（本规模可忽略）。踢人经 `conns` 表发 `force_logout` 帧后移除发送端：客户端收到即主动断开回登录页，服务端写路径同时被掐断。已知取舍：并发首注册理论上可产双 admin（无害）；`UserPublic.role` 对所有人可见（谁是管理员非秘密）。

## 12. Agent 接入：ACP 为主、原生兜底（已废弃，见 13）

**决策**：宿主运行时定义一个 Agent 抽象（开始/恢复上下文、发指令、事件流、权限请求、取消），首个实现是 ACP（Agent Client Protocol）客户端。只有当某个 CLI 的关键能力 ACP 适配器没暴露时，才给它写原生对接。将来的内置 agent 也按 ACP agent 实现。

**理由**：ACP 官方登记表约 50 个 agent（Claude Code、Codex、Gemini CLI、Grok Build、Qoder CLI、Copilot 等），对接一次全部可用。协议自带本功能要的四样东西：`session/new` 的 mcpServers 用来注入聊天工具，`session/request_permission` 对应审批，`session/update` 对应三种输出方式，`session/load` / `session/resume` 对应上下文延续。每个 CLI 单独对接（Claude Code stream-json、Codex app-server 等）意味着每加一种 agent 写一套，只支持写过的那几个。

**代价**：功能受各适配器完整度限制；Claude Code 的适配器是 npm 包，宿主机要装 Node；多一层适配进程。原生兜底路径让抽象层多一个实现要维护。

**升级路径**：遇到 ACP 缺口时，为该 CLI 在同一抽象下加原生实现，上层（调用分发、审批、输出）不变。

## 13. 去掉 Agent，改用 webhook bot

**决策**：删除全部 Agent 功能（桌面宿主、ACP、内置 agent、审批、定时、工作区）。迁移 0035 删表删列，Agent 专属会话整个删掉，Agent 在私聊 / 群里的消息硬删。改为 webhook bot：bot 是 `users` 里带 `bot_owner_id` 的一行，任何用户可建；入站 `POST /v1/hooks`（`Authorization: Bearer <令牌>`，可带 `client_msg_id` 幂等）以 bot 身份发消息，出站在 bot 被私聊或在群里被 @ 时把消息 POST 到它的回调地址，响应里的 `content` 作为回复。

**理由**：Agent 接入要桌面端常驻宿主、逐个适配 CLI、审批和权限档位，用起来门槛高、代码量占了一半。bot 复用现有的账号、好友、成员、@ 与消息管线，服务端只多一张表和一个异步回调；接什么能力由 bot 自己的 HTTP 服务决定。bot 当普通账号存，发言、提及、推送、搜索都不用改。

**代价**：服务端会向用户填写的任意地址发请求（局域网也能打到），自部署场景接受；bot 的令牌明文存库，管理员和备份可见（界面每次都显示令牌；改哈希存储就得改成只在创建时显示一次）。Agent 历史消息不保留。

**升级路径**：要限制回调目标时加管理员白名单；要更丰富的 bot 能力（附件、卡片）时扩展入站请求体与回调负载，账号模型不变。

## 14. Bot API：OpenAPI 描述、令牌换身份、服务端交付游标

**决策**：给 agent 开放 bot 令牌能调的一组接口（发消息含附件和回复、读会话和历史、拉取新消息、改和撤回自己的消息）。规格手写成一份 OpenAPI 3.1（`server/src/api/bot_openapi.json`），服务端在 `GET /v1/bot/openapi.json` 提供，`servers` 按 public_url 或请求 Host 填。鉴权层 `bot_auth` 把 bot 令牌换成该 bot 的短时 access token，再交给和用户接口共用的处理函数；bot 能调什么，就是 `api::router` 里挂了这一层的那张路由表。拉取新消息用 `memberships.delivered_seq` 记交付游标，取到即前移（至多一次）；没有新消息时按 bot 长轮询，新消息落库后在回调分发处叫醒。bot 上传的附件算进主人的存储配额；主人被禁用期间 bot 的令牌（403）和回调都停用，启用后恢复。

**理由**：agent 平台大多能直接把 OpenAPI 导入成工具；很多 agent 没有公网地址、收不了回调，拉取补上这条路。复用用户接口的处理函数，读历史、上传、改、撤的规则和人完全一致，不用另写一套。游标不能复用 last_read_seq：bot 发言会把它推到自己那条，夹在中间还没交付的消息就丢了。至多一次对 agent 最省事（不用回 ack），漏处理的从历史补。bot 能上传以后，配额要是按 bot 各算各的，每建一个 bot 就多一份空间；bot 能读群以后，主人被禁用时令牌还能用，被禁用的人就能借 bot 继续读群。

**代价**：规格是手写的，改接口要同步改 JSON；集成测试会逐个请求描述里的接口，路径、方法、鉴权对不上就失败，字段说明靠人对。bot 读得到所在群的全部历史，主人退群或被踢后 bot 还留在群里照样能读，目前靠群主把 bot 踢掉。长轮询的叫醒是单实例内存状态（和 WS 连接表一样）。
