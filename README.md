> **⚠️ 修改代码前（包括 AI agent）必须先阅读 [AGENTS.md](AGENTS.md)，尤其是其中的发布与版本号升级规则。**

# Chat

一个私聊 + 群聊的聊天软件。后端 Rust/axum + PostgreSQL，前端 React 单代码库，浏览器 / Windows(Tauri) / Android(Tauri) 三端复用。

## 功能

- 注册登录（用户名 + 可改昵称），JWT access + refresh token
- 好友制：搜索用户名 → 发请求 → 对方同意后成为好友
- 私聊：加好友自动建 DM；删好友后 DM 变只读
- 群聊：只能拉自己的好友；群主可改名/踢人/转让/解散；群主退群自动转让给最早加入成员
- 消息：持久化历史、每会话递增 seq、离线重连按 seq 补发（`sync`）
- 附件：图片/文件消息（multipart 上传 → WS `attachment_id` 发送），图片气泡内联预览、文件卡片下载
- @提及：群聊 @用户名（输入自动补全），气泡高亮 + 会话列表「有人@我」标记，已读后消除
- 会话内搜索：聊天头 🔍 → `GET /conversations/{id}/messages/search?q=`，点结果跳转定位
- 后台管理：最早注册用户自动成为管理员，客户端「管理」tab 提供用户列表/禁用/启用/重置密码/任命管理员 + 全局统计；禁用即踢下线（`force_logout`）且全线拒绝，数据保留、启用恢复；管理员不可被禁用、末位管理员不可撤销
- 系统通知：窗口不在前台时弹系统通知（Web Notification / Tauri），标题栏显示未读数，桌面端任务栏闪烁；静音会话只放行 @我
- 草稿：每个会话的未发送文字自动保存，切换会话、刷新后仍在，会话列表显示「[草稿]」
- 链接与代码：消息里的链接可点击（桌面端用系统浏览器打开），支持行内代码和带复制按钮的代码块
- 暗色模式：跟随系统 / 浅色 / 深色，在「我」页切换
- 存储配额：管理员可设每人附件总量上限，「我」页显示已用空间
- 注册开关：管理员可切换开放注册 / 邀请码 / 关闭注册，并可签发、撤销邀请码
- 登录设备与改密：可查看本账号的登录设备并移出；自助改密码后其他设备立即下线
- 头像：上传 PNG / JPEG / WebP，按版本号更新
- 图片缩略图：较长边超过 480 像素的图片自动生成 JPEG 缩略图
- 语音消息：带时长的音频以语音气泡发送，占位文字为「语音」
- 文件传输助手：每人一个仅自己的会话，用于在多设备间传文件和备忘
- 转发：把消息转发到其他会话（转发到文件传输助手即收藏）；同一次操作重试不产生重复
- 全局搜索：跨会话搜索消息
- 群公告与置顶：群主可设公告；任何成员可置顶消息（每会话最多 20 条）
- 视频会议：会话内打开会议链接（默认 Jitsi，管理员可改地址或关闭）
- 手机推送：管理员配置 ntfy 后，用户在「我」页开启
- 备份与恢复：管理后台一键生成备份（数据库全部表 + 附件 / 头像文件，tar.gz）、下载到本地；上传备份文件恢复（需键入 RESTORE 确认，恢复后所有用户需重新登录）
- 桌面端：关掉窗口留在系统托盘继续收消息；开机自启在「我」页打开，默认关
- 机器人（webhook bot）：任何人可在「我」页建 bot，给它加一个或多个令牌。外部程序带令牌往入站地址 POST 就以 bot 身份发消息；填了回调地址，私聊 bot 或在群里 @ 它时服务器把消息 POST 过去，回复的文字发回原会话。agent 可以按 OpenAPI 描述（`/v1/bot/openapi.json`）用同一个令牌拉取新消息、读历史、发文件、改和撤回
- 实时：WebSocket（一次性 ticket 鉴权）、多设备同时在线、在线状态、正在输入、未读数、已读同步
- 部署：`./scripts/run.sh` 一条命令（Postgres + 服务端托管前端，主机端口 28080）

## 快速开始（服务端）

```bash
./scripts/run.sh              # 一条命令：缺 .env 时自动生成 JWT_SECRET，然后构建启动
# 浏览器打开 http://localhost:28080
```

更新代码后跑同一个脚本：检测到已有容器时会先校验当前在 `main` 分支且工作区干净，然后 `git pull --ff-only origin main` 再重建（必须带 `--build`，否则 compose 会继续用旧镜像）。

脚本等价于手动跑这几条，仍然可以这样部署/更新：

```bash
echo "JWT_SECRET=$(openssl rand -hex 32)" > .env   # 只做一次；没有它服务端拒绝启动
docker compose up -d --build                     # 首次启动
git pull --ff-only && docker compose up -d --build  # 更新
```

环境变量（compose 自动读本目录 `.env`，全部字段见 [.env.example](.env.example)）：`DATABASE_URL`、`JWT_SECRET`（必填，至少 32 个字符；不设置服务端拒绝启动。换掉后已登录的设备会自动刷新凭据，不用重新登录）、`PORT`、`BIND`、`APP_DIST`、`FILES_DIR`（附件存储目录，compose 挂 `chat-files` 卷）、`UPLOAD_MAX_MB`（单文件上限，默认 25）。挂在 nginx / Caddy 等反向代理后面时设 `TRUST_PROXY=1`，登录 / 注册限流按 `X-Forwarded-For` 认客户端 IP；直接对外时不要设，否则谁都能伪造这个头绕过限流。

## 开发

```bash
# 数据库（只监听本机 127.0.0.1:15432）
docker compose up db -d
# 后端（:28080）；开发用任意 32 位以上的字符串即可
cd server && JWT_SECRET=$(openssl rand -hex 32) cargo run
# 前端（:5173，已配置 /v1 代理到 :28080）
pnpm install && pnpm dev
```

## 测试

```bash
pnpm test    # cargo test：auth/好友流/会话权限/seq/WS/群规则 集成测试（需 db 容器）

# E2E 冒烟：直接打运行中的部署（默认 http://localhost:28080）
# 前提：库里已经有管理员。全新部署里第一个注册用户会自动成为管理员；
# 若未设置 E2E_ADMIN_USER，且脚本注册的第一个用户成了管理员，脚本提示后退出 2，
# 避免「非管理员 403」整批误报。管理员流程和注册开关需要 E2E_ADMIN_USER / E2E_ADMIN_PASS。
E2E_ADMIN_USER=<管理员用户名> E2E_ADMIN_PASS=<密码> node scripts/e2e.mjs
docker compose down && docker compose up -d --wait
node scripts/e2e.mjs phase2   # phase2：验证数据持久化（含头像、文件传输助手、公告、置顶、bot）
```

## 推送（ntfy）

管理员在后台填 ntfy 服务器地址，用户在「我」页开启，并在手机 ntfy App 里订阅页面显示的地址。

## 客户端打包

桌面安装包只能在对应系统上编：Windows 包在 Windows 上编，macOS 包在 Mac 上编，Linux 包在 Linux 上编。Android 包在任一台桌面系统上都能编。

所有平台都要先准备：

- Node.js 20 以上、pnpm、Rust stable（用 [rustup](https://rustup.rs) 安装）。
- 仓库根目录执行一次 `pnpm install`。
- 版本号改 `app/src-tauri/tauri.conf.json` 的 `version`，安装包文件名和 Android 的 versionName 都跟着它变。
- 图标已生成在 `app/src-tauri/icons/`。换图标后在 `app/` 下执行 `node gen-icon.cjs`。

下面的打包命令都在 `app/` 目录执行。装好后在客户端登录页填服务器地址（如 `https://chat.287152.xyz`）。

### Windows

准备：Visual Studio 2022 生成工具，勾选「使用 C++ 的桌面开发」；Rust 用默认的 MSVC 工具链。WebView2 在 Windows 10 / 11 上已自带。

```bash
cd app
pnpm tauri build
```

产物：`src-tauri/target/release/bundle/nsis/Chat_<版本>_x64-setup.exe`。

- 第一次打包会自动下载 NSIS，需要联网。release 编译第一次约 5–10 分钟，之后是增量编译。
- 安装包没有代码签名，从网上下载后运行时 SmartScreen 可能提示「未知发布者」，点「更多信息 → 仍要运行」。

### macOS

准备：Xcode 命令行工具（`xcode-select --install`）。

```bash
cd app
pnpm tauri build --bundles app
```

产物：`src-tauri/target/release/bundle/macos/Chat.app`，拖进「应用程序」即可。

- 打出来的包只适配编译它的那台 Mac 的芯片（Apple 芯片或 Intel）。
- 加上 `dmg`（`--bundles app,dmg`）会多打一个 DMG。这一步要用 AppleScript 控制 Finder，可能失败；失败时 `Chat.app` 已经生成，直接拖即可。
- 包没有签名。拷到别的 Mac 上，第一次打开如果提示「已损坏」或「无法验证开发者」，执行 `xattr -cr /Applications/Chat.app` 后再打开。
- 同时适配两种芯片的通用包（未实测）：先 `rustup target add aarch64-apple-darwin x86_64-apple-darwin`，再 `pnpm tauri build --bundles app --target universal-apple-darwin`，产物在 `src-tauri/target/universal-apple-darwin/release/bundle/macos/Chat.app`。

### Linux（未实测）

以 Debian / Ubuntu 为例先装系统依赖（其他发行版见 [Tauri 前置要求](https://v2.tauri.app/start/prerequisites/)）：

```bash
sudo apt update
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

```bash
cd app
pnpm tauri build --bundles deb,appimage
```

产物：

- `src-tauri/target/release/bundle/deb/Chat_<版本>_amd64.deb`，用 `sudo apt install ./Chat_<版本>_amd64.deb` 安装。
- `src-tauri/target/release/bundle/appimage/Chat_<版本>_amd64.AppImage`，`chmod +x` 后直接运行。
- 要 RPM 包就在 `--bundles` 里加上 `rpm`。

### Android

准备：

- Android Studio，并在 SDK Manager 里装上 NDK。
- 环境变量：`JAVA_HOME` 指向 Android Studio 自带的 JBR，`ANDROID_HOME` 指向 SDK 目录，`NDK_HOME` 指向 NDK 目录。
- Rust 目标：`rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android`。
- Android 工程已经提交在 `app/src-tauri/gen/android`，不用再执行 `tauri android init`。

```bash
cd app
pnpm tauri android build --apk --debug --target aarch64
```

- APK 在 `src-tauri/gen/android/app/build/outputs/apk/` 下，命令结束时会打印完整路径。用 `adb install -r <apk 路径>` 安装，或者拷到手机上点开安装。
- `--target aarch64` 只编 arm64，现在的手机基本都是这个架构，编得也快。去掉它会把四种架构都编进去。
- 用 debug 包的原因：release 包禁止明文 HTTP（`usesCleartextTraffic=false`），连不上局域网的 `http://` 服务器；而且 release 包没有签名，不能直接安装。只有服务器走 HTTPS、又配好了签名，才用 release 包（去掉 `--debug`）。

### Android 在 Windows 上的已知坑

`tauri android build` 里有一步给 `.so` 建符号链接，需要开启 Windows 开发者模式（设置 → 系统 → 开发者选项 → 开发人员模式）。若没开：

```bash
cd app/src-tauri
# 1. 交叉编译 rust lib（以 arm64 为例）
cargo build --target aarch64-linux-android --release
# 2. 手动复制到 jniLibs
mkdir -p gen/android/app/src/main/jniLibs/arm64-v8a
cp target/aarch64-linux-android/release/libchat_app_lib.so gen/android/app/src/main/jniLibs/arm64-v8a/
# 3. 跳过 rustBuild* 直接 gradle 打包
cd gen/android && ./gradlew assembleDebug \
  -x rustBuildArm64Debug -x rustBuildArmDebug -x rustBuildX86Debug -x rustBuildX86_64Debug
# 产物：app/build/outputs/apk/arm64/debug/app-arm64-debug.apk
```

需要 NDK（`sdkmanager "ndk;<ver>"`）+ `rustup target add aarch64-linux-android` 等。

## 发布新版本（在线更新）

推 `v*` 标签后，GitHub Actions 在免费托管 runner 上打 Windows 安装包、Android APK、deb 和 AppImage，先挂到草稿 Release，7 个产物齐了自动转公开。Chat 服务端读最新 Release：「我」页显示新版本，Windows / Linux 点「更新并重启」自动装，Android 下载 APK 后由系统覆盖安装。浏览器端不用管，服务端更新后刷新就是新版。

发一个版本：

```bash
# 1. 改 app/src-tauri/tauri.conf.json 的 version（如 0.2.0），提交合入 main
# 2. 打标签并推送，标签必须和 version 一致；标签说明就是更新说明
git tag -a v0.2.0 -m "更新说明"
git push origin v0.2.0
```

一次性准备（Secrets、服务端设置、排障）见 [docs/release-runners.md](docs/release-runners.md)。

第一个能在线更新的版本需要手动安装一次：之前装的版本里没有更新功能。

## 机器人（webhook bot）

在「我」页 → 机器人里新建。bot 会出现在你的联系人和会话里，可以拉进你所在的群。

入站：先给 bot 加令牌（可以加多个，分给不同程序，单独删除）。外部程序带令牌以 bot 身份发消息，默认发到和你的私聊，带 `conversation_id` 发到 bot 所在的其他会话。令牌不对返回 401。要安全重试（比如 CI 超时重发）就带上 `client_msg_id`（1–80 字符）：同一会话里重发同一个值，只会有一条消息，返回的也是第一次那条。

```bash
curl -X POST http://<服务器>/v1/hooks -H 'Authorization: Bearer <令牌>' -H 'Content-Type: application/json' -d '{"content":"部署完成","client_msg_id":"deploy-1234"}'
```

出站：填了回调地址时，bot 在私聊里收到消息、或在群里被 @ 时，服务器 POST：

```json
{"bot_id": "…", "conversation_id": "…", "conversation_type": "dm", "message": { "id": "…", "sender_id": "…", "sender_name": "…", "content": "…", "mentions": [] }}
```

请求头 `X-Chat-Signature: sha256=<hex>`，值是用签名密钥（「我」页可复制、可更换）对请求体做的 HMAC-SHA256。10 秒内回 `{"content": "..."}` 就作为 bot 的回复发回原会话，回别的内容不回复。bot 自己发的消息不会触发任何回调。

已知限制：令牌明文存在服务器上，管理员和备份都能看到；服务器会向用户填的任意地址发请求，包括局域网地址。

### Bot API（给 agent 用）

同一个令牌还能读会话和历史、拉取新消息、发图片和文件、改和撤回自己的消息。接口描述是 OpenAPI，地址 `http://<服务器>/v1/bot/openapi.json`（不要令牌）：能导入 OpenAPI 的 agent 平台直接填这个地址，鉴权选 Bearer、填令牌。

| 接口 | 做什么 |
|---|---|
| `GET /v1/bot/updates?timeout=30` | 拉取发给 bot 的新消息（私聊、群里 @ 它的），和回调是同样的事件；取到即交付，不会重复给。没有公网地址的程序用它代替回调地址 |
| `POST /v1/hooks` | 发消息；可带 `attachment_id`（先 `POST /v1/bot/files` 上传到同一会话）和 `reply_to_id`，返回完整消息 |
| `GET /v1/bot/conversations`、`/v1/bot/conversations/{id}`、`/v1/bot/conversations/{id}/messages` | bot 所在的会话、成员、历史 |
| `PATCH /v1/bot/messages/{id}`、`POST /v1/bot/messages/{id}/recall` | 改自己 15 分钟内的文字消息、撤回自己 24 小时内的消息 |

```bash
curl 'http://<服务器>/v1/bot/updates?timeout=30' -H 'Authorization: Bearer <令牌>'
```

bot 上传的文件算进主人的存储配额（主人和他的 bot 共用一份）。主人被管理员禁用期间，bot 的令牌返回 403、也不回调，启用后恢复。

### SDK

四种语言，接口一样：用令牌 `send` 发消息，注册一个处理函数，`run` 起服务收回调（自动验签，返回的文字就是回复）。已有 web 服务时不用 `run`，在自己的路由里调 `handle(请求体, X-Chat-Signature 头)`，拿到状态码和响应 JSON。令牌和签名密钥在「我」页 → 机器人里复制。回调要在 10 秒内返回，耗时的活在处理函数里另开线程，做完再 `send`。

| 语言 | 位置 | 依赖 | 测试 |
|---|---|---|---|
| Python ≥ 3.8 | [sdk/python](sdk/python/chatbot.py) | 只用标准库，单文件 | `cd sdk/python && python -m unittest` |
| Go ≥ 1.21 | [sdk/go](sdk/go/chatbot.go) | 只用标准库，单文件；`Bot` 本身是 `http.Handler` | `cd sdk/go && go test` |
| Java ≥ 11 | [sdk/java](sdk/java/chatbot/ChatBot.java) | 只用 JDK，单文件（自带一个小 JSON 解析器） | `cd sdk/java && javac -d out chatbot/ChatBot.java ChatBotTest.java && java -cp out ChatBotTest` |
| Rust | [sdk/rust](sdk/rust/src/lib.rs) | ureq、tiny_http、serde_json、hmac；同步，不要 async 运行时 | `cd sdk/rust && cargo test` |

Python 可以直接拷文件，或 `pip install "git+<仓库地址>#subdirectory=sdk/python"`；Go 拷文件或在 go.mod 里 `replace chatbot => <路径>`；Rust 用 `chatbot = { git = "<仓库地址>" }` 或 path 依赖。

```python
from chatbot import Bot

bot = Bot("https://chat.example.com", token="入站令牌", secret="签名密钥")
bot.send("部署完成")                     # send(文字, conversation_id=, client_msg_id=)

@bot.on_message
def reply(event):                       # 私聊它、或在群里 @ 它时调用
    return "收到：" + event.text        # 返回 None = 不回

bot.run(port=8080)                      # 回调地址填 http://<这台机器>:8080/
```

```go
bot := chatbot.New("https://chat.example.com", "入站令牌", "签名密钥")
bot.Send("部署完成")                     // SendWith(文字, chatbot.SendOptions{ConversationID, ClientMsgID})
bot.OnMessage(func(ev chatbot.Event) string { return "收到：" + ev.Message.Content }) // "" = 不回
log.Fatal(bot.Run(":8080"))
```

```java
ChatBot bot = new ChatBot("https://chat.example.com", "入站令牌", "签名密钥");
bot.send("部署完成");                    // send(文字, 会话 id, client_msg_id)，不要的传 null
bot.onMessage(ev -> "收到：" + ev.text()).run(8080);   // 返回 null = 不回
```

```rust
let bot = chatbot::Bot::new("https://chat.example.com", "入站令牌", "签名密钥");
bot.send("部署完成")?;                   // send_with(文字, SendOptions { conversation_id, client_msg_id })
bot.on_message(|ev| Some(format!("收到：{}", ev.message.content))).run("0.0.0.0:8080")?; // None = 不回
```

## 局域网访问

- **同一台机器只跑一个服务端实例**：多实例共享数据库但 WS 连接表各自独立，跨实例的消息只能落库、推不到对端（表现为"必须刷新才看到新消息"）。开发调试也统一用 docker。
- 原生跑 `cargo run`（仅临时调试用）：直接监听 `0.0.0.0:28080`，同网段设备访问 `http://<本机IP>:28080`。用时先停掉 docker server。
- WSL2 里的 `docker compose up`：发布端口只到 Windows localhost。要暴露到局域网需管理员跑一次：
  `netsh interface portproxy add v4tov4 listenport=28080 listenaddress=0.0.0.0 connectport=28080 connectaddress=127.0.0.1`
  （真实部署到 Linux 服务器无此问题，`0.0.0.0` 直接生效）

## 架构

```
app/              React + TS + Vite（Web/Tauri 共用 UI）
  src/ui/         chat/ admin/ me/ 按页面分组；Avatar、icons 等公用件在根下
  src-tauri/      Tauri v2 壳（Windows/Android）
server/           axum + sqlx + PostgreSQL
  src/api/        REST：account/users/friends/conversations/messages/avatar/files/admin/bots
  src/domain/     membership / seq / 发消息 / 消息生命周期 / 投票 / 转发
  src/ws.rs       WS：连接注册、消息广播、presence、typing、read、sync
  migrations/     SQL 迁移（sqlx::migrate! 编译期嵌入）
  tests/it/       集成测试，按领域分文件，公用 helper 在 common.rs
docs/archive/     已完成的开发计划、任务单、交付文档（只读）
scripts/devin/    并行派发 Devin / Grok 的 worktree 脚本
docker-compose.yml  db + server（托管 app/dist）
```

## 目录约定

- 领域词汇与边界规则：`CONTEXT.md`
- 架构决策记录：`docs/adr/`
