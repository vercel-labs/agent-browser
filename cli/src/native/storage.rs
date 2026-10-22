use serde_json::{json, Value};

use super::cdp::client::CdpClient;

pub async fn storage_get(
    client: &CdpClient,
    session_id: &str,
    storage_type: &str,
    key: Option<&str>,
) -> Result<Value, String> {
    let st = storage_js_name(storage_type);

    if let Some(k) = key {
        let js = format!(
            "{}.getItem({})",
            st,
            serde_json::to_string(k).unwrap_or_default()
        );
        let result = eval_simple(client, session_id, &js).await?;
        Ok(json!({ "key": k, "value": result }))
    } else {
        let js = format!(
            r#"(() => {{
                const s = {};
                const data = {{}};
                for (let i = 0; i < s.length; i++) {{
                    const key = s.key(i);
                    data[key] = s.getItem(key);
                }}
                return data;
            }})()"#,
            st
        );
        let result = eval_simple(client, session_id, &js).await?;
        Ok(json!({ "data": result }))
    }
}

pub async fn storage_set(
    client: &CdpClient,
    session_id: &str,
    storage_type: &str,
    key: &str,
    value: &str,
) -> Result<(), String> {
    let st = storage_js_name(storage_type);
    let js = format!(
        "{}.setItem({}, {})",
        st,
        serde_json::to_string(key).unwrap_or_default(),
        serde_json::to_string(value).unwrap_or_default(),
    );
    eval_simple(client, session_id, &js).await?;
    Ok(())
}

pub async fn storage_clear(
    client: &CdpClient,
    session_id: &str,
    storage_type: &str,
) -> Result<(), String> {
    let st = storage_js_name(storage_type);
    let js = format!("{}.clear()", st);
    eval_simple(client, session_id, &js).await?;
    Ok(())
}

fn storage_js_name(storage_type: &str) -> &str {
    match storage_type {
        "session" => "sessionStorage",
        _ => "localStorage",
    }
}

async fn eval_simple(client: &CdpClient, session_id: &str, js: &str) -> Result<Value, String> {
    client
        .evaluate_value(js, Some(session_id))
        .await
        .map_err(|e| format!("Storage error: {}", e))
}
