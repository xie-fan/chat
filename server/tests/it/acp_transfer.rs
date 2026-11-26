use super::*;
use reqwest::multipart::{Form, Part};
use uuid::Uuid;

#[tokio::test]
async fn snapshot_transfer_preserves_history_and_file_and_retries_atomically() {
    let (_db, base, st) = spawn_named_state(&format!("chat_t_acpx_{}", uniq())).await;
    let member = register(&base, &format!("acpx_{}", uniq())).await;
    let other = register(&base, &format!("acpx_{}", uniq())).await;
    let uid: Uuid = member.id.parse().unwrap();
    let install_id = Uuid::new_v4();
    let host_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO acp_hosts (user_id, install_id, name) VALUES ($1, $2, 'Desktop')
         RETURNING id",
    )
    .bind(uid)
    .bind(install_id)
    .fetch_one(&st.pool)
    .await
    .unwrap();

    let url = format!("{base}/v1/acp/local-hosts/{install_id}/snapshot");
    let client = http();
    let bearer = authz(&member.access);
    let foreign: Value = client
        .get(&url)
        .header("authorization", authz(&other.access))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(foreign, Value::Null);

    let read = || client.get(&url).header("authorization", bearer.clone());
    let seed: Value = read().send().await.unwrap().json().await.unwrap();
    let mut snapshot = seed["snapshot"].clone();
    let agent_id = Uuid::new_v4();
    let workspace_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let attachment_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let transfer_id = Uuid::new_v4();
    let time = "2026-11-26T00:00:00Z";
    let att = json!({
        "id": attachment_id, "session_id": session_id, "file_name": "note.txt",
        "mime": "text/plain", "size": 4
    });
    snapshot["agents"] = json!([{
        "id": agent_id, "host_id": host_id, "preset": "custom", "name": "Local Agent",
        "adapter": "fixture", "cmd": "fixture", "env": { "FIXTURE": "fixture-local-value" },
        "short_tag": "FIX", "badge_bg": "blue", "default_model": "local", "model_options": ["local"],
        "permission_override": "inherit", "capabilities": [], "status": "standby",
        "created_at": time, "updated_at": time
    }]);
    snapshot["workspaces"] = json!([{
        "id": workspace_id, "host_id": host_id, "cwd": "/tmp/fixture", "name": "Fixture",
        "branch": "main", "branches": ["main"], "created_at": time, "updated_at": time
    }]);
    snapshot["sessions"] = json!([{
        "id": session_id, "workspace_id": workspace_id, "agent_id": agent_id, "title": "Conversation",
        "selected_model": "local", "thinking_level": "中", "permission_policy": "auto_read",
        "acp_session_key": "external-session", "running": false, "diffs": [], "rpc_logs": [],
        "created_at": time, "updated_at": time,
        "turns": [{
            "id": turn_id, "session_id": session_id, "seq": 1, "role": "user", "status": "done",
            "created_at": time, "updated_at": time,
            "payload": { "text": "question", "attachments": [att.clone()] }
        }]
    }]);
    snapshot["attachments"] = json!([att]);
    let upload = || {
        client
            .put(format!("{base}/v1/acp/transfer-files/{attachment_id}"))
            .header("authorization", bearer.clone())
            .multipart(
                Form::new().part(
                    "file",
                    Part::bytes(b"note".to_vec())
                        .file_name("note.txt")
                        .mime_str("text/plain")
                        .unwrap(),
                ),
            )
    };
    let import = || {
        client
            .post(&url)
            .header("authorization", bearer.clone())
            .json(&json!({ "transfer_id": transfer_id, "snapshot": snapshot }))
    };

    let failure = import().send().await.unwrap();
    assert_eq!(failure.status(), 409);
    let after_failure: Value = read().send().await.unwrap().json().await.unwrap();
    assert_eq!(after_failure, seed);

    let first_upload: Value = upload().send().await.unwrap().json().await.unwrap();
    let duplicate_upload: Value = upload().send().await.unwrap().json().await.unwrap();
    assert_eq!(first_upload["id"], attachment_id.to_string());
    assert_eq!(duplicate_upload, first_upload);
    for _ in 0..2 {
        let receipt: Value = import().send().await.unwrap().json().await.unwrap();
        assert_eq!(receipt["transfer_id"], transfer_id.to_string(), "{receipt}");
    }
    let exported: Value = read().send().await.unwrap().json().await.unwrap();
    assert_eq!(
        exported["snapshot"]["sessions"][0]["turns"][0]["id"],
        turn_id.to_string()
    );
    assert_eq!(
        exported["snapshot"]["sessions"][0]["acp_session_key"],
        "external-session"
    );
    assert_eq!(
        exported["snapshot"]["agents"][0]["env"]["FIXTURE"],
        "fixture-local-value"
    );
    assert_eq!(
        exported["snapshot"]["attachments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let download = || {
        client
            .get(format!("{base}/v1/acp/files/{attachment_id}"))
            .header("authorization", bearer.clone())
    };
    assert_eq!(
        download().send().await.unwrap().text().await.unwrap(),
        "note"
    );
    let delete = |revision: &str| {
        client
            .delete(&url)
            .header("authorization", bearer.clone())
            .query(&[("revision", revision)])
    };
    assert_eq!(
        delete(seed["revision"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(download().send().await.unwrap().status(), 200);
    let exported_revision = exported["revision"].as_str().unwrap();
    assert_eq!(
        delete(exported_revision).send().await.unwrap().status(),
        204
    );
    assert_eq!(download().send().await.unwrap().status(), 404);
    assert_eq!(
        delete(exported_revision).send().await.unwrap().status(),
        204
    );
}
