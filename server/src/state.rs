use crate::config::Config;
use crate::error::AppResult;
use dashmap::DashMap;
use sqlx::PgPool;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use uuid::Uuid;

pub type SharedState = Arc<AppState>;

/// 连接出站事件：普通帧，或踢下线（先推 payload 帧再主动断开 socket）
pub enum ConnMsg {
    Frame(String),
    Shutdown(String),
}

/// 单连接出站队列上限。对端读不动（弱网/死连接）时队列满 → 断开，客户端重连后按 seq 补齐。
/// ponytail: 按帧数限，不按字节；大帧多的场景再改字节预算。
pub const CONN_QUEUE: usize = 1024;

/// 单条在线连接：出站事件发送端 + 所属登录会话（老连接可为 None）
pub struct Conn {
    pub tx: mpsc::Sender<ConnMsg>,
    pub sid: Option<Uuid>,
    /// 出站队列满（或已关）时置位，会话循环看到后断开
    pub stalled: Arc<AtomicBool>,
}

impl Conn {
    /// 非阻塞投递：广播方绝不等慢连接
    fn push(&self, m: ConnMsg) {
        if self.tx.try_send(m).is_err() {
            self.stalled.store(true, Ordering::Relaxed);
        }
    }
}

/// user_id -> (conn_id -> 连接)
pub type ConnMap = DashMap<Uuid, DashMap<Uuid, Conn>>;

/// 一次性 WS ticket 的内容：签发时的用户、凭据版本与登录会话
pub struct TicketGrant {
    pub uid: Uuid,
    pub session_version: i64,
    pub sid: Option<Uuid>,
    pub issued: Instant,
}

pub struct AppState {
    pub pool: PgPool,
    pub cfg: Config,
    pub conns: ConnMap,
    /// 一次性 WS ticket -> 授权信息
    pub tickets: DashMap<String, TicketGrant>,
    /// 简易限流：key -> 时间戳窗口
    pub rate: DashMap<String, Vec<Instant>>,
    /// 每次在线/离线翻转加一。广播前再核对，避免连上和断开交错时把状态写反。
    epochs: DashMap<Uuid, u64>,
    /// (user_id, conversation_id) -> 最近一次推送时刻。既是合并窗口，也标记「手机上还挂着这会话的通知」，
    /// 已读时据此去 ntfy 清通知并移除。
    /// ponytail: 单实例内存状态，重启即丢：重启前推的通知读了也不会被自动清，需在手机上手动划掉。
    pub push_last: DashMap<(Uuid, Uuid), Instant>,
    /// 推送合并窗口（毫秒），默认 30s；测试可调小
    pub push_coalesce_ms: AtomicU64,
    /// V2 P3-3：WS 入站静默超时（毫秒），默认 75s ≈ 2.5 个 ping 周期；测试可调小
    pub ws_idle_timeout_ms: AtomicU64,
    /// V2 P3-3：WS ping 周期（毫秒），默认 30s；与超时一起调小做心跳测试
    pub ws_ping_interval_ms: AtomicU64,
    /// V2 P4-1：维护模式。为 true 时除正在执行的恢复请求外所有 /v1 请求 503（含 WS 升级）。
    pub maintenance: AtomicBool,
    /// V2 P4-1：备份/恢复互斥锁（同一时间只允许一个备份生成或一次恢复进行）。
    pub backup_busy: AtomicBool,
    /// bot id → 在 GET /bot/updates 上长轮询的请求；有发给它的新消息时 notify_waiters 叫醒。
    /// 和 conns 一样是单实例内存状态。
    pub bot_wake: DashMap<Uuid, Arc<tokio::sync::Notify>>,
}

impl AppState {
    pub fn new(pool: PgPool, cfg: Config) -> SharedState {
        Arc::new(Self {
            pool,
            cfg,
            conns: DashMap::new(),
            tickets: DashMap::new(),
            rate: DashMap::new(),
            epochs: DashMap::new(),
            push_last: DashMap::new(),
            push_coalesce_ms: AtomicU64::new(30_000),
            ws_idle_timeout_ms: AtomicU64::new(75_000),
            ws_ping_interval_ms: AtomicU64::new(30_000),
            maintenance: AtomicBool::new(false),
            backup_busy: AtomicBool::new(false),
            bot_wake: DashMap::new(),
        })
    }

    fn bump_epoch(&self, uid: Uuid) -> u64 {
        let mut n = self.epochs.entry(uid).or_insert(0);
        *n += 1;
        *n
    }

    pub fn epoch_is(&self, uid: Uuid, epoch: u64) -> bool {
        self.epochs.get(&uid).is_some_and(|n| *n == epoch)
    }

    /// 登记连接。返回 Some(epoch) 表示这是该用户的第一条连接（离线 → 在线）。
    pub fn register_conn(
        &self,
        uid: Uuid,
        conn_id: Uuid,
        tx: mpsc::Sender<ConnMsg>,
        sid: Option<Uuid>,
        stalled: Arc<AtomicBool>,
    ) -> Option<u64> {
        let first = {
            let bucket = self.conns.entry(uid).or_default();
            let first = bucket.is_empty();
            bucket.insert(
                conn_id,
                Conn {
                    tx,
                    sid,
                    stalled,
                },
            );
            first
        };
        first.then(|| self.bump_epoch(uid))
    }

    /// 移除连接。返回 Some(epoch) 表示这是最后一条连接（在线 → 离线）。
    pub fn unregister_conn(&self, uid: Uuid, conn_id: Uuid) -> Option<u64> {
        let now_empty = {
            let bucket = self.conns.get_mut(&uid)?;
            if bucket.remove(&conn_id).is_none() {
                return None;
            }
            bucket.is_empty()
        };
        if !now_empty {
            return None;
        }
        let still_empty = self.conns.get(&uid).is_some_and(|bucket| bucket.is_empty());
        if !still_empty {
            return None;
        }
        let epoch = self.bump_epoch(uid);
        self.conns.remove_if(&uid, |_, conns| conns.is_empty());
        Some(epoch)
    }

    pub fn send_to_user(&self, uid: Uuid, payload: &str) {
        if let Some(m) = self.conns.get(&uid) {
            for s in m.iter() {
                s.push(ConnMsg::Frame(payload.to_string()));
            }
        }
    }

    pub fn send_to_users(&self, uids: &[Uuid], payload: &str) {
        for u in uids {
            self.send_to_user(*u, payload);
        }
    }

    pub fn is_online(&self, uid: Uuid) -> bool {
        self.conns.get(&uid).map(|m| !m.is_empty()).unwrap_or(false)
    }

    /// 踢下线：给该用户所有连接发 force_logout 并主动断开 socket，同时摘掉连接表。
    /// 返回 Some(epoch) 表示刚才还在线，调用方负责广播离线。
    /// 断线清理看到连接已不在表里，不会再广播一次。
    pub fn kick_user(&self, uid: Uuid, reason: &str) -> Option<u64> {
        let Some((_, bucket)) = self.conns.remove(&uid) else {
            return None;
        };
        let frame = serde_json::json!({
            "type": "force_logout",
            "reason": reason,
        })
        .to_string();
        let mut had = false;
        for conn in bucket.iter() {
            had = true;
            conn.push(ConnMsg::Shutdown(frame.clone()));
        }
        had.then(|| self.bump_epoch(uid))
    }

    /// 踢掉满足条件的连接（先收 force_logout 帧再断开）。
    /// 返回 Some(epoch) 仅当踢完后该用户已无连接（在线 → 离线），由调用方广播离线；
    /// 仍有其他连接在线时返回 None，绝不能广播离线。
    fn kick_matching(
        &self,
        uid: Uuid,
        reason: &str,
        keep: impl Fn(Option<Uuid>) -> bool,
    ) -> Option<u64> {
        let frame = serde_json::json!({
            "type": "force_logout",
            "reason": reason,
        })
        .to_string();
        let mut kicked = false;
        let emptied = {
            let bucket = self.conns.get_mut(&uid)?;
            let ids: Vec<Uuid> = bucket
                .iter()
                .filter(|e| !keep(e.sid))
                .map(|e| *e.key())
                .collect();
            for id in ids {
                if let Some((_, conn)) = bucket.remove(&id) {
                    kicked = true;
                    conn.push(ConnMsg::Shutdown(frame.clone()));
                }
            }
            kicked && bucket.is_empty()
        };
        if !emptied {
            return None;
        }
        // 与 unregister_conn 同样的二次确认：可能恰好有新连接注册进来
        let still_empty = self.conns.get(&uid).is_some_and(|b| b.is_empty());
        if !still_empty {
            return None;
        }
        let epoch = self.bump_epoch(uid);
        self.conns.remove_if(&uid, |_, conns| conns.is_empty());
        Some(epoch)
    }

    /// 踢掉某个登录会话的全部连接（设备被移出）。
    pub fn kick_session(&self, uid: Uuid, sid: Uuid, reason: &str) -> Option<u64> {
        self.kick_matching(uid, reason, |s| s != Some(sid))
    }

    /// 踢掉除 keep 以外登录会话的全部连接（改密/一键退出其他设备）。
    /// keep=None（老 token 发起的请求）：只踢带 sid 的连接，无 sid 的老连接保留，
    /// 由 session_version 变化在下一帧自然断开。
    pub fn kick_other_sessions(&self, uid: Uuid, keep: Option<Uuid>, reason: &str) -> Option<u64> {
        self.kick_matching(uid, reason, |s| s == keep)
    }

    /// 恢复备份时踢掉全部用户的在线连接，但保留发起恢复的登录会话：
    /// 它的恢复请求还在等结果，被踢会先看到踢下线文案，盖住恢复结果。
    /// keep=None（sid 缺失的老 token）：没有可标识的发起者连接，全踢。
    pub fn kick_all_except_session(&self, keep: Option<Uuid>, reason: &str) {
        let uids: Vec<Uuid> = self.conns.iter().map(|e| *e.key()).collect();
        for uid in uids {
            self.kick_matching(uid, reason, |s| keep.is_some() && s == keep);
        }
    }

    /// 某个登录会话当前是否有在线连接（设备列表的 online 点）
    pub fn session_online(&self, uid: Uuid, sid: Uuid) -> bool {
        self.conns
            .get(&uid)
            .is_some_and(|m| m.iter().any(|c| c.sid == Some(sid)))
    }

    /// 连接是否仍在连接表里（被踢后旧任务不得再受理任何帧）
    pub fn conn_alive(&self, uid: Uuid, conn_id: Uuid) -> bool {
        self.conns
            .get(&uid)
            .is_some_and(|m| m.contains_key(&conn_id))
    }

    pub fn conn_count(&self, uid: Uuid) -> usize {
        self.conns.get(&uid).map(|m| m.len()).unwrap_or(0)
    }

    pub fn issue_ticket(&self, uid: Uuid, session_version: i64, sid: Option<Uuid>) -> String {
        let mut buf = [0u8; 32];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut buf);
        let ticket = hex::encode(buf);
        // 惰性清理超过 60s 未消费的 ticket
        self.tickets
            .retain(|_, t| t.issued.elapsed().as_secs() < 60);
        self.tickets.insert(
            ticket.clone(),
            TicketGrant {
                uid,
                session_version,
                sid,
                issued: Instant::now(),
            },
        );
        ticket
    }

    /// 好友 ∪ 共群成员 —— 即关心我 presence 的人，也是其 presence 对我可见的人。
    /// 注意排除 DM：删好友后保留的只读 DM 不构成 presence 可见性。
    pub async fn watchers(&self, uid: Uuid) -> AppResult<Vec<Uuid>> {
        let rows = sqlx::query_scalar::<_, Uuid>(
            "SELECT DISTINCT u FROM (
                 SELECT CASE WHEN user_a = $1 THEN user_b ELSE user_a END AS u
                   FROM friendships WHERE user_a = $1 OR user_b = $1
                 UNION
                 SELECT m2.user_id FROM memberships m1
                   JOIN memberships m2 ON m1.conversation_id = m2.conversation_id
                   JOIN conversations c ON c.id = m1.conversation_id
                  WHERE m1.user_id = $1 AND c.type = 'group'
             ) t WHERE u <> $1",
        )
        .bind(uid)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}
