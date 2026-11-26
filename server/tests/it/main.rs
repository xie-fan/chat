//! 集成测试：需要本地 Postgres（docker compose up db）。
//! 测试库不再用固定 chat_test（MM5）：TEST_DATABASE_NAME 指定或每次运行生成
//! chat_it_run_<pid>_<rand>，进程退出自动删；spawn_named_*/spawn_backup_env
//! 的独立库由 TempDb 守卫在测试结束（含 panic）时删库；启动时清扫器顺带
//! 清掉崩溃残留的 chat_it_run_/chat_t_/chat_it_* 库。
//! 运行：cargo test --manifest-path server/Cargo.toml

use chat_server::build_router;
use chat_server::config::Config;
use chat_server::state::AppState;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use std::net::SocketAddr;
use std::time::Duration as Dur;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WsMsg;

mod acp;
mod acp_transfer;
mod admin;
mod announcements;
mod attachments;
mod avatar;
mod backup;
mod basics;
mod bots;
mod client_update;
mod common;
mod conv_prefs;
mod forward;
mod forward_batch;
mod harness;
mod invites;
mod messages;
mod polls;
mod push;
mod recall;
mod regressions;
mod search;
mod sessions;
mod system_messages;
mod voice;

// MM5：测试库生命周期助手（与 tests/migrations.rs 共用同一文件）
#[path = "../common/testdb.rs"]
mod testdb;

use avatar::*;
use backup::*;
use common::*;
use conv_prefs::*;
use messages::*;
use regressions::*;
