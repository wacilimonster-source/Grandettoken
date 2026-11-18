use serde_json::{json, Value};

pub fn from_chat_request(chat: &Value) -> Result<(Value, Vec<String>), String> {
    if chat
        .get("n")
        .is_some_and(|n| !n.is_null() && n.as_u64() != Some(1))
    {
        return Err("Responses 渠道只支持 n=1".into());
    }
    if present(&chat["functions"]) || present(&chat["function_call"]) {
        return Err("Responses 渠道请使用 tools、tool_choice 和 role=tool".into());
    }
    if present(&chat["audio"])
        || chat["modalities"]
            .as_array()
            .is_some_and(|m| m.iter().any(|v| v != "text"))
        || chat["logprobs"] == true
        || chat["top_logprobs"].as_u64().is_some_and(|n| n > 0)
    {
        return Err("Responses 渠道暂不支持 Chat Completions 音频输出或 logprobs".into());
    }

    let messages = chat["messages"].as_array().ok_or("需要 messages 数组")?;
    let mut input = Vec::new();
    for message in messages {
        append_message(message, &mut input)?;
    }
    let mut body = json!({"model": chat["model"], "input": input, "store": false});
    for key in [
        "stream",
        "store",
        "temperature",
        "top_p",
        "parallel_tool_calls",
        "metadata",
        "service_tier",
        "prompt_cache_key",
        "prompt_cache_retention",
        "safety_identifier",
    ] {
        if present(&chat[key]) {
            body[key] = chat[key].clone();
        }
    }
    if let Some(limit) = ["max_completion_tokens", "max_tokens"]
        .iter()
        .map(|&key| &chat[key])
        .find(|value| present(value))
    {
        body["max_output_tokens"] = limit.clone();
    }
    if let Some(effort) = chat["reasoning_effort"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
    {
        body["reasoning"]["effort"] = json!(effort);
    }
    if chat["include_reasoning"] == true && chat["reasoning_effort"] != "none" {
        body["reasoning"]["summary"] = json!("auto");
    }
    if let Some(tools) = chat["tools"].as_array().filter(|tools| !tools.is_empty()) {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(function_definition)
                .collect::<Result<_, _>>()?,
        );
    } else if present(&chat["tools"]) && !chat["tools"].is_array() {
        return Err("tools 必须是数组".into());
    }
    if present(&chat["tool_choice"]) {
        body["tool_choice"] = tool_choice(&chat["tool_choice"])?;
    }
    if present(&chat["response_format"]) {
        body["text"]["format"] = text_format(&chat["response_format"])?;
    }
    let mut transforms = vec!["protocol:chat_completions->responses".into()];
    for key in [
        "stop",
        "frequency_penalty",
        "presence_penalty",
        "seed",
        "logit_bias",
        "top_k",
    ] {
        if present(&chat[key]) {
            transforms.push(format!("drop:{key}(responses)"));
        }
    }
    Ok((body, transforms))
}

fn present(value: &Value) -> bool {
    !value.is_null()
}

fn required_str<'a>(value: &'a Value, field: &'static str) -> Result<&'a str, String> {
    value
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("缺少或无效的 {field}"))
}

fn append_message(message: &Value, input: &mut Vec<Value>) -> Result<(), String> {
    let role = required_str(&message["role"], "messages[].role")?;
    let content = &message["content"];
    match role {
        "tool" => {
            let call_id = required_str(&message["tool_call_id"], "tool_call_id")?;
            input.push(json!({
                "type":"function_call_output", "call_id":call_id, "output":input_content(content)?
            }));
        }
        "system" | "developer" | "user" | "assistant" => {
            if present(content) {
                input.push(
                    json!({"type":"message", "role":role, "content":input_content(content)?}),
                );
            } else if role != "assistant" {
                return Err("消息缺少 content".into());
            } else if let Some(refusal) = message["refusal"].as_str().filter(|s| !s.is_empty()) {
                input.push(json!({"type":"message", "role":"assistant", "content":refusal}));
            }
            if role == "assistant" {
                if present(&message["function_call"]) {
                    return Err("Responses 渠道的助理工具调用请使用 tool_calls".into());
                }
                for call in message["tool_calls"].as_array().into_iter().flatten() {
                    if call["type"] != "function" {
                        return Err("Responses 渠道暂只支持 function 工具".into());
                    }
                    let id = required_str(&call["id"], "tool_calls[].id")?;
                    let name =
                        required_str(&call["function"]["name"], "tool_calls[].function.name")?;
                    let args = call["function"]["arguments"]
                        .as_str()
                        .ok_or("tool_calls[].function.arguments 必须是字符串")?;
                    input.push(json!({"type":"function_call", "call_id":id, "name":name, "arguments":args}));
                }
            }
        }
        _ => return Err("Responses 渠道不支持该消息角色".into()),
    }
    Ok(())
}

fn input_content(content: &Value) -> Result<Value, String> {
    if content.is_string() {
        return Ok(content.clone());
    }
    let parts = content
        .as_array()
        .ok_or("消息 content 必须是文本或内容数组")?;
    let mapped = parts
        .iter()
        .map(|part| match part["type"].as_str() {
            Some("text" | "refusal") => {
                let field = if part["type"] == "text" {
                    "text"
                } else {
                    "refusal"
                };
                let text = part[field].as_str().ok_or("消息文本必须是字符串")?;
                Ok(json!({"type":"input_text","text":text}))
            }
            Some("image_url") => {
                let image = &part["image_url"];
                let url = required_str(&image["url"], "image_url.url")?;
                let detail = image["detail"].as_str().unwrap_or("auto");
                Ok(json!({"type":"input_image","image_url":url,"detail":detail}))
            }
            Some("file") => {
                let file = &part["file"];
                let mut out = json!({"type":"input_file"});
                for field in ["file_id", "file_data", "filename"] {
                    if present(&file[field]) {
                        out[field] = file[field].clone();
                    }
                }
                Ok(out)
            }
            _ => Err("Responses 渠道不支持该消息内容类型".into()),
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Value::Array(mapped))
}

fn function_definition(tool: &Value) -> Result<Value, String> {
    if tool["type"] != "function" {
        return Err("Responses 渠道暂只支持 function 工具".into());
    }
    let function = &tool["function"];
    let name = required_str(&function["name"], "tools[].function.name")?;
    // Chat Completions 的函数 schema 默认非严格，Responses 需要显式保留这个默认。
    let mut out = json!({"type":"function","name":name,"strict":false});
    for field in ["description", "parameters", "strict"] {
        if present(&function[field]) {
            out[field] = function[field].clone();
        }
    }
    Ok(out)
}

fn tool_choice(choice: &Value) -> Result<Value, String> {
    match choice.as_str() {
        Some("auto" | "none" | "required") => return Ok(choice.clone()),
        Some(_) => return Err("Responses 渠道不支持该 tool_choice".into()),
        None => {}
    }
    match choice["type"].as_str() {
        Some("function") => Ok(
            json!({"type":"function","name":required_str(&choice["function"]["name"], "tool_choice.function.name")?}),
        ),
        Some("allowed_tools") => {
            let allowed = &choice["allowed_tools"];
            let tools = allowed["tools"]
                .as_array()
                .ok_or("allowed_tools.tools 必须是数组")?;
            let tools = tools
                .iter()
                .map(tool_choice)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({"type":"allowed_tools","mode":allowed["mode"],"tools":tools}))
        }
        _ => Err("Responses 渠道不支持该 tool_choice".into()),
    }
}

fn text_format(format: &Value) -> Result<Value, String> {
    match format["type"].as_str() {
        Some("text" | "json_object") => Ok(format.clone()),
        Some("json_schema") => {
            let schema = &format["json_schema"];
            let name = required_str(&schema["name"], "response_format.json_schema.name")?;
            let mut out =
                json!({"type":"json_schema","name":name,"schema":schema["schema"],"strict":false});
            for field in ["description", "strict"] {
                if present(&schema[field]) {
                    out[field] = schema[field].clone();
                }
            }
            Ok(out)
        }
        _ => Err("Responses 渠道不支持该 response_format".into()),
    }
}
