use serde_json::{json, Value};

use super::cdp::client::CdpClient;
use super::cdp::types::EvaluateParams;

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
    let result: super::cdp::types::EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: js.to_string(),
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    if let Some(ref details) = result.exception_details {
        return Err(format!("Storage error: {}", details.text));
    }

    Ok(result.result.value.unwrap_or(Value::Null))
}
