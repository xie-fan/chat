use crate::{
    auth::require_auth,
    error::{AppError, AppResult},
    model::MessageView,
    state::SharedState,
};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::Mutex;
use uuid::Uuid;

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const FCM_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";

#[derive(Deserialize)]
struct ServiceAccount {
    project_id: String,
    client_email: String,
    private_key: String,
}

struct Fcm {
    project: String,
    client_email: String,
    key: EncodingKey,
    client: reqwest::Client,
    oauth: Mutex<Option<(String, Instant)>>,
}

#[derive(Serialize)]
struct GoogleClaims<'a> {
    iss: &'a str,
    scope: &'static str,
    aud: &'static str,
    iat: i64,
    exp: i64,
}

#[derive(Deserialize)]
struct GoogleToken {
    access_token: String,
    expires_in: u64,
}

pub(crate) struct FcmFile {
    path: PathBuf,
    cached: Mutex<Option<CachedFcm>>,
}

struct CachedFcm {
    modified: SystemTime,
    len: u64,
    client: Option<Arc<Fcm>>,
}

impl FcmFile {
    pub(crate) fn from_env() -> Option<Self> {
        let path = std::env::var_os("CHAT_FCM_SERVICE_ACCOUNT_FILE")?;
        Some(Self {
            path: path.into(),
            cached: Mutex::new(None),
        })
    }

    async fn client(&self) -> Option<Arc<Fcm>> {
        let mut cached = self.cached.lock().await;
        let Ok(meta) = tokio::fs::metadata(&self.path).await else {
            *cached = None;
            return None;
        };
        if !meta.is_file() {
            *cached = None;
            return None;
        }
        let modified = meta.modified().ok()?;
        let len = meta.len();
        if let Some(entry) = cached
            .as_ref()
            .filter(|e| e.modified == modified && e.len == len)
        {
            return entry.client.clone();
        }
        let client = tokio::fs::read(&self.path)
            .await
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .and_then(Fcm::new)
            .map(Arc::new);
        if client.is_none() {
            tracing::warn!("invalid CHAT_FCM_SERVICE_ACCOUNT_FILE; Android push disabled");
        }
        *cached = Some(CachedFcm {
            modified,
            len,
            client: client.clone(),
        });
        client
    }
}

impl Fcm {
    fn new(account: ServiceAccount) -> Option<Self> {
        if account.project_id.is_empty()
            || !account
                .project_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || account.client_email.is_empty()
        {
            return None;
        }
        Some(Self {
            project: account.project_id,
            client_email: account.client_email,
            key: EncodingKey::from_rsa_pem(account.private_key.as_bytes()).ok()?,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(8))
                .build()
                .ok()?,
            oauth: Mutex::new(None),
        })
    }

    async fn access_token(&self) -> Result<String, &'static str> {
        let mut cache = self.oauth.lock().await;
        if let Some((token, expires)) = &*cache {
            if *expires > Instant::now() {
                return Ok(token.clone());
            }
        }
        let now = chrono::Utc::now().timestamp();
        let assertion = jsonwebtoken::encode(
            &Header::new(Algorithm::RS256),
            &GoogleClaims {
                iss: &self.client_email,
                scope: FCM_SCOPE,
                aud: TOKEN_URL,
                iat: now,
                exp: now + 3600,
            },
            &self.key,
        )
        .map_err(|_| "service-account JWT signing failed")?;
        let response = self
            .client
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", &assertion),
            ])
            .send()
            .await
            .map_err(|_| "Google OAuth request failed")?
            .error_for_status()
            .map_err(|_| "Google OAuth rejected the service account")?
            .json::<GoogleToken>()
            .await
            .map_err(|_| "invalid Google OAuth response")?;
        let ttl = response.expires_in.saturating_sub(60).min(3600);
        let token = response.access_token;
        *cache = Some((token.clone(), Instant::now() + Duration::from_secs(ttl)));
        Ok(token)
    }

    async fn send(&self, payload: Value) -> Result<bool, &'static str> {
        let access = self.access_token().await?;
        let response = self
            .client
            .post(format!(
                "https://fcm.googleapis.com/v1/projects/{}/messages:send",
                self.project
            ))
            .bearer_auth(access)
            .json(&payload)
            .send()
            .await
            .map_err(|_| "FCM send request failed")?;
        let status = response.status();
        if status.is_success() {
            return Ok(true);
        }
        if status == StatusCode::UNAUTHORIZED {
            *self.oauth.lock().await = None;
        }
        if status == StatusCode::NOT_FOUND
            && response.json::<Value>().await.is_ok_and(|body| {
                body["error"]["details"]
                    .as_array()
                    .is_some_and(|details| details.iter().any(|d| d["errorCode"] == "UNREGISTERED"))
            })
        {
            return Ok(false);
        }
        Err("FCM rejected the push request")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Registration {
    token: String,
    binding: Uuid,
    project: String,
    rotation_key: String,
    generation: i64,
}

pub(crate) async fn register(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<Registration>,
) -> AppResult<StatusCode> {
    let auth = require_auth(&st, &headers).await?;
    let sid = auth
        .sid
        .ok_or_else(|| AppError::unauthorized("please login again"))?;
    validate_registration(&st, &req).await?;
    save_registration(&st.pool, sid, &req, false).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn rotate(
    State(st): State<SharedState>,
    Json(req): Json<Registration>,
) -> AppResult<StatusCode> {
    validate_registration(&st, &req).await?;
    let sid = sqlx::query_scalar::<_, Uuid>(
        "SELECT session_id FROM android_fcm_devices WHERE binding = $1 AND key_hash = $2",
    )
    .bind(req.binding)
    .bind(crate::auth::refresh_hash(&req.rotation_key))
    .fetch_optional(&st.pool)
    .await?
    .ok_or_else(|| AppError::unauthorized("invalid push binding"))?;
    save_registration(&st.pool, sid, &req, true).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn validate_registration(st: &SharedState, req: &Registration) -> AppResult<()> {
    if android_push_client(st)
        .await
        .is_none_or(|fcm| fcm.project != req.project)
    {
        return Err(AppError::not_found("Android push unavailable"));
    }
    if req.token.is_empty()
        || req.token.len() > 1024
        || !req.token.bytes().all(|b| b.is_ascii_graphic())
        || req.binding.is_nil()
        || req.rotation_key.len() != 64
        || !req.rotation_key.bytes().all(|b| b.is_ascii_hexdigit())
        || req.generation <= 0
    {
        return Err(AppError::bad_request("invalid Android push registration"));
    }
    Ok(())
}

async fn save_registration(
    pool: &sqlx::PgPool,
    sid: Uuid,
    req: &Registration,
    rotation: bool,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let active = sqlx::query_scalar::<_, Uuid>(
        "SELECT s.id FROM sessions s JOIN users u ON u.id = s.user_id
          WHERE s.id = $1 AND s.revoked_at IS NULL AND u.disabled_at IS NULL
            AND EXISTS (SELECT 1 FROM refresh_tokens r WHERE r.session_id = s.id
                          AND r.revoked_at IS NULL AND r.expires_at > now())
          FOR UPDATE OF s",
    )
    .bind(sid)
    .fetch_optional(&mut *tx)
    .await?;
    if active.is_none() {
        return Err(AppError::unauthorized("session revoked"));
    }
    let hash = crate::auth::refresh_hash(&req.rotation_key);
    let device = sqlx::query_as::<_, (Uuid, String, i64)>(
        "SELECT binding, key_hash, generation FROM android_fcm_devices WHERE session_id = $1",
    )
    .bind(sid)
    .fetch_optional(&mut *tx)
    .await?;
    match device {
        Some((binding, key, generation)) if binding == req.binding && key == hash => {
            if generation >= req.generation {
                return Ok(());
            }
        }
        _ if rotation => return Err(AppError::unauthorized("invalid push binding")),
        _ => {}
    }
    if !rotation {
        sqlx::query("DELETE FROM android_fcm_devices WHERE session_id <> $1 AND token = $2")
            .bind(sid)
            .bind(&req.token)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO android_fcm_devices (token, session_id, binding, key_hash, generation)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (session_id) DO UPDATE SET token = EXCLUDED.token, binding = EXCLUDED.binding,
                                              key_hash = EXCLUDED.key_hash, generation = EXCLUDED.generation",
    )
    .bind(&req.token)
    .bind(sid)
    .bind(req.binding)
    .bind(hash)
    .bind(req.generation)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) fn schedule(st: &SharedState, msg: &MessageView) {
    if st.fcm.is_none() {
        return;
    }
    let (st, msg) = (st.clone(), msg.clone());
    tokio::spawn(async move {
        if dispatch(&st, &msg).await.is_err() {
            tracing::warn!("could not query Android push recipients");
        }
    });
}

async fn dispatch(st: &SharedState, msg: &MessageView) -> AppResult<()> {
    let Some(fcm) = android_push_client(st).await else {
        return Ok(());
    };
    let devices = recipients(&st.pool, msg.conversation_id, msg.sender_id, msg.seq).await?;
    for (token, sid, binding) in devices {
        match fcm
            .send(payload(&token, binding, msg.conversation_id, msg.id))
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                sqlx::query(
                    "UPDATE android_fcm_devices SET token = NULL WHERE token = $1 AND session_id = $2 AND binding = $3",
                )
                .bind(&token)
                .bind(sid)
                .bind(binding)
                .execute(&st.pool)
                .await?;
            }
            Err(error) => tracing::warn!(error, "Android push failed"),
        }
    }
    Ok(())
}

pub(crate) async fn android_push_available(st: &SharedState) -> bool {
    android_push_client(st).await.is_some()
}

async fn android_push_client(st: &SharedState) -> Option<Arc<Fcm>> {
    st.fcm.as_ref()?.client().await
}

async fn recipients(
    pool: &sqlx::PgPool,
    conv: Uuid,
    sender: Uuid,
    seq: i64,
) -> sqlx::Result<Vec<(String, Uuid, Uuid)>> {
    sqlx::query_as(
        "SELECT d.token, d.session_id, d.binding
           FROM android_fcm_devices d
           JOIN sessions s ON s.id = d.session_id AND s.revoked_at IS NULL
           JOIN users u ON u.id = s.user_id AND u.disabled_at IS NULL
           JOIN memberships m ON m.user_id = s.user_id AND m.conversation_id = $1
           JOIN conversations c ON c.id = m.conversation_id AND c.type = 'dm'
           LEFT JOIN conversation_preferences p
             ON p.conversation_id = m.conversation_id AND p.user_id = m.user_id
          WHERE d.token IS NOT NULL AND m.user_id <> $2 AND m.last_read_seq < $3
            AND NOT COALESCE(p.mute_forever, false)
            AND (p.muted_until IS NULL OR p.muted_until <= now())
            AND EXISTS (
                 SELECT 1 FROM refresh_tokens r WHERE r.session_id = s.id
                   AND r.revoked_at IS NULL AND r.expires_at > now())",
    )
    .bind(conv)
    .bind(sender)
    .bind(seq)
    .fetch_all(pool)
    .await
}

fn payload(token: &str, binding: Uuid, conv_id: Uuid, msg_id: Uuid) -> Value {
    json!({
        "message": {
            "token": token,
            "data": {
                "binding": binding.to_string(),
                "convId": conv_id.to_string(),
                "messageId": msg_id.to_string(),
            },
            "android": { "priority": "HIGH", "ttl": "300s" },
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "./migrations")]
    async fn server_info_reports_native_fcm_independently_of_ntfy(pool: sqlx::PgPool) {
        let cfg = crate::config::Config {
            database_url: String::new(),
            jwt_secret: "x".repeat(32),
            bind: "127.0.0.1".into(),
            port: 0,
            app_dist: None,
            files_dir: String::new(),
            upload_max_bytes: 0,
            trust_proxy: false,
            recall_attachment_ttl_secs: 86400,
            recall_sweep_interval_secs: 3600,
        };
        let st = crate::state::AppState::new(pool.clone(), cfg.clone());
        let Json(info) = crate::settings::server_info(State(st)).await.unwrap();
        assert_eq!(
            serde_json::to_value(info).unwrap()["android_fcm_available"],
            false
        );

        let path = std::env::temp_dir().join(format!("chat-fcm-info-{}.json", Uuid::new_v4()));
        let st = crate::state::AppState::new_with_fcm(
            pool,
            cfg,
            Some(FcmFile {
                path: path.clone(),
                cached: Mutex::new(None),
            }),
        );
        for content in [None, Some("{}".to_string()), Some(json!({
            "project_id": "test-project", "client_email": "service@example.invalid",
            "private_key": "-----BEGIN RSA PRIVATE KEY-----\nMB0CAQACAgyhAgERAgIKwQIBPQIBNQIBNQIBMQIBJg==\n-----END RSA PRIVATE KEY-----",
        }).to_string())] {
            if let Some(content) = &content { std::fs::write(&path, content).unwrap(); }
            let Json(info) = crate::settings::server_info(State(st.clone())).await.unwrap();
            assert!(!info.push_available);
            assert_eq!(info.android_fcm_available, content.as_ref().is_some_and(|c| c != "{}"));
        }
        std::fs::remove_file(path).unwrap();
        let Json(info) = crate::settings::server_info(State(st)).await.unwrap();
        assert!(!info.android_fcm_available);
    }

    #[tokio::test]
    async fn loads_a_service_account_added_after_startup_and_disables_it_on_removal() {
        let path = std::env::temp_dir().join(format!("chat-fcm-{}.json", Uuid::new_v4()));
        let source = FcmFile {
            path: path.clone(),
            cached: Mutex::new(None),
        };
        assert!(source.client().await.is_none());

        std::fs::write(&path, "{}").unwrap();
        assert!(source.client().await.is_none());

        let account = json!({
            "project_id": "test-project",
            "client_email": "service@example.invalid",
            // Toy RSA parameters; the test only parses the key.
            "private_key": "-----BEGIN RSA PRIVATE KEY-----\nMB0CAQACAgyhAgERAgIKwQIBPQIBNQIBNQIBMQIBJg==\n-----END RSA PRIVATE KEY-----",
        });
        std::fs::write(&path, account.to_string()).unwrap();
        let client = source.client().await.unwrap();
        assert_eq!(client.project, "test-project");
        assert!(Arc::ptr_eq(&client, &source.client().await.unwrap()));

        std::fs::remove_file(&path).unwrap();
        assert!(source.client().await.is_none());
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn sends_only_to_eligible_dm_sessions_and_replaces_refreshed_tokens(pool: sqlx::PgPool) {
        let uid: Uuid = sqlx::query_scalar(
            "INSERT INTO users (username, display_name, password_hash)
             VALUES ('fcm_test', 'Test', 'unused') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let sid: Uuid =
            sqlx::query_scalar("INSERT INTO sessions (user_id) VALUES ($1) RETURNING id")
                .bind(uid)
                .fetch_one(&pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO refresh_tokens (user_id, session_id, token_hash, expires_at)
                     VALUES ($1, $2, 'unused', now() + interval '1 hour')",
        )
        .bind(uid)
        .bind(sid)
        .execute(&pool)
        .await
        .unwrap();
        let conv: Uuid =
            sqlx::query_scalar("INSERT INTO conversations (type) VALUES ('dm') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        sqlx::query("INSERT INTO memberships (conversation_id, user_id) VALUES ($1, $2)")
            .bind(conv)
            .bind(uid)
            .execute(&pool)
            .await
            .unwrap();
        let mut reg = Registration {
            token: "first".into(),
            binding: Uuid::new_v4(),
            project: "test".into(),
            rotation_key: "a".repeat(64),
            generation: 1,
        };
        assert!(save_registration(&pool, sid, &reg, true).await.is_err());
        save_registration(&pool, sid, &reg, false).await.unwrap();
        reg.generation = 2;
        reg.token = "second".into();
        reg.rotation_key = "b".repeat(64);
        assert!(save_registration(&pool, sid, &reg, true).await.is_err());
        reg.rotation_key = "a".repeat(64);
        save_registration(&pool, sid, &reg, true).await.unwrap();
        reg.generation = 1;
        reg.token = "first".into();
        save_registration(&pool, sid, &reg, false).await.unwrap();
        let sender = Uuid::new_v4();
        assert_eq!(
            recipients(&pool, conv, sender, 1).await.unwrap(),
            vec![("second".into(), sid, reg.binding)]
        );
        assert!(recipients(&pool, conv, uid, 1).await.unwrap().is_empty());
        assert!(recipients(&pool, conv, sender, 0).await.unwrap().is_empty());
        sqlx::query("UPDATE sessions SET revoked_at = now() WHERE id = $1")
            .bind(sid)
            .execute(&pool)
            .await
            .unwrap();
        assert!(recipients(&pool, conv, sender, 1).await.unwrap().is_empty());
        reg.generation = 3;
        assert!(save_registration(&pool, sid, &reg, true).await.is_err());
    }

    #[test]
    fn data_message_keeps_notification_rendering_and_content_on_device() {
        let (binding, conv, msg) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let data = payload("token", binding, conv, msg);
        let message = &data["message"];
        assert_eq!(message["android"]["priority"], "HIGH");
        assert!(message.get("notification").is_none());
        assert_eq!(
            message["data"],
            json!({"binding": binding.to_string(), "convId": conv.to_string(), "messageId": msg.to_string()})
        );
    }
}
