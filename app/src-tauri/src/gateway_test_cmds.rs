use crate::gateway::{get_gateway, lock};
use serde::Deserialize;
use serde_json::Value;
use tokenscope_core::gateway::http::{run_test_chat, GatewayDiagEntry};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestChatInput {
    pub channel_id: String,
    pub model: String,
    pub prompt: String,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub include_reasoning: Option<bool>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

#[tauri::command]
pub async fn gateway_test_chat(
    app: tauri::AppHandle,
    input: TestChatInput,
) -> Result<Value, String> {
    let g = get_gateway(&app)?;
    let channel = lock(&g.inner)
        .settings
        .channels
        .iter()
        .find(|c| c.channel_id == input.channel_id)
        .cloned()
        .ok_or("渠道不存在")?;
    let model = input.model.trim();
    let stripped = model
        .strip_prefix(&format!("{}/", channel.alias))
        .unwrap_or(model)
        .to_string();
    if stripped.is_empty() {
        return Err("请选择测试模型".into());
    }
    let prompt = if input.prompt.trim().is_empty() {
        "请用一句中文打个招呼。".to_string()
    } else {
        input.prompt
    };
    let stream = input.stream.unwrap_or(true);
    let include_reasoning = input.include_reasoning.unwrap_or(true);
    let reasoning_effort = input.reasoning_effort;
    let gw = g.inner().clone();
    let handle = tokio::spawn(async move {
        run_test_chat(
            &gw,
            channel,
            stripped,
            prompt,
            stream,
            include_reasoning,
            reasoning_effort,
        )
        .await
    });
    let test_id = {
        let mut inner = lock(&g.inner);
        if let Some((_, prev)) = inner.test_chat_abort.take() {
            prev.abort();
        }
        let id = inner.next_test_id;
        inner.next_test_id += 1;
        inner.test_chat_abort = Some((id, handle.abort_handle()));
        id
    };
    let joined = handle.await;
    {
        let mut inner = lock(&g.inner);
        if inner.test_chat_abort.as_ref().is_some_and(|(id, _)| *id == test_id) {
            inner.test_chat_abort = None;
        }
    }
    match joined {
        Ok(res) => serde_json::to_value(res).map_err(|e| e.to_string()),
        Err(e) if e.is_cancelled() => Err("已取消本次联调测试".into()),
        Err(e) => Err(format!("联调测试任务异常结束：{e}")),
    }
}

#[tauri::command]
pub async fn gateway_cancel_test_chat(app: tauri::AppHandle) -> Result<bool, String> {
    let g = get_gateway(&app)?;
    let prev = lock(&g.inner).test_chat_abort.take();
    if let Some((_, abort)) = prev {
        abort.abort();
        return Ok(true);
    }
    Ok(false)
}

#[tauri::command]
pub async fn gateway_diagnostics(app: tauri::AppHandle) -> Result<Vec<GatewayDiagEntry>, String> {
    let g = get_gateway(&app)?;
    let inner = lock(&g.inner);
    Ok(inner.diagnostics.iter().rev().cloned().collect())
}

#[tauri::command]
pub async fn gateway_clear_diagnostics(app: tauri::AppHandle) -> Result<(), String> {
    let g = get_gateway(&app)?;
    lock(&g.inner).diagnostics.clear();
    Ok(())
}
