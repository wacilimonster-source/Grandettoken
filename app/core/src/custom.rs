//! 自定义渠道(裁决 6A)。
//!
//! 一期"OpenAI 兼容中转站"与二期"全自定义"**共用同一套字段** —— 所谓"模板"只是
//! 预填默认值,不另立一套机制(照小鲸鱼的做法:模板只提供默认值,留空沿用,用户可自救)。
//!
//! 三条纪律:
//!   ① 这是**只读账务**:只发取数/探活请求,永远不向平台写东西;
//!   ② 密钥仍只进 Windows 凭据管理器(渠道 id 就是凭据名),配置文件里只留字段路径;
//!   ③ 请求只发往**用户自己填的地址** —— 填错了是他自己的选择,界面在测试通过前
//!      不落库(测试按钮就是那道护城河)。
//!
//! 解释器:`path` 支持 `a.b` 与 `data[0].quota` 两种写法;取到的数除以 `scale`
//! 得到显示值(OneAPI 的 500000 积分 = $1,就是 scale=500000)。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 单个自定义渠道的定义。全部 `serde(default)`,老配置缺字段不吃亏。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct CustomChannel {
    /// 渠道 id(生成:custom-1、custom-2…),同时是凭据管理器里的账户名
    pub id: String,
    pub name: String,
    /// 简称(胶囊/紧凑条上用,1~2 个字符)
    pub short: String,
    /// #rrggbb
    pub color: String,
    /// 取数地址
    pub url: String,
    /// "GET" | "POST"(空 = GET)
    pub method: String,
    /// POST 的请求体(JSON 文本);里面的 `{key}` 会替换成密钥
    pub body: Option<String>,
    /// 余额字段路径;空 = 整份响应就是一个数字
    pub path: String,
    /// 换算倍数:显示值 = 原始值 / scale(<=0 当作 1)
    pub scale: f64,
    /// 显示单位:CNY | USD | credits | %(空 = CNY)
    pub unit: String,
    /// 探活模式:只验证"这份密钥这个地址通不通",不解析余额
    /// (余额显示「—」,不写快照、不参与消耗合计)
    pub probe_only: bool,
    /// 探活地址;空 = 用 url
    pub probe_url: Option<String>,
}

impl CustomChannel {
    pub fn normalized_scale(&self) -> f64 {
        if self.scale > 0.0 {
            self.scale
        } else {
            1.0
        }
    }
    pub fn normalized_unit(&self) -> String {
        let u = self.unit.trim();
        if u.is_empty() {
            "CNY".into()
        } else {
            u.to_uppercase()
        }
    }
    pub fn normalized_method(&self) -> String {
        let m = self.method.trim().to_uppercase();
        if m == "POST" {
            "POST".into()
        } else {
            "GET".into()
        }
    }
    /// 校验(保存前跑;测试也用它,保证"能存进去的都能测")。
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("渠道名不能为空".into());
        }
        if !(self.url.starts_with("http://") || self.url.starts_with("https://")) {
            return Err("取数地址要以 http:// 或 https:// 开头".into());
        }
        if self.probe_only && !self.path.trim().is_empty() {
            return Err("探活模式下不解析余额,字段路径请留空".into());
        }
        if !self.probe_only && self.path.trim().is_empty() {
            return Err("请填写余额字段路径(不知道就先用探活模式)".into());
        }
        Ok(())
    }
}

// ───────────────────────── 字段路径解释器 ─────────────────────────

/// 按 `a.b[0].c` 走一层层取值。路径语法错了就当"取不到"(返回 None),不 panic。
fn walk<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for seg in path.split('.') {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        let (name, mut rest) = match seg.find('[') {
            Some(i) => (&seg[..i], &seg[i..]),
            None => (seg, ""),
        };
        if !name.is_empty() {
            cur = cur.get(name)?;
        }
        while let Some(open) = rest.find('[') {
            let close = rest[open..].find(']')? + open;
            let idx: usize = rest[open + 1..close].trim().parse().ok()?;
            cur = cur.get(idx)?;
            rest = &rest[close + 1..];
        }
    }
    Some(cur)
}

/// 取值并转数字。数字字符串也认(有些站点把金额写成字符串)。
pub fn pick(v: &Value, path: &str) -> Option<f64> {
    let node = walk(v, path)?;
    match node {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().trim_end_matches('%').trim().parse::<f64>().ok(),
        _ => None,
    }
}

// ───────────────────────── 取数 ─────────────────────────

/// 单次请求预算:与内置渠道同一档(8 秒)。
const TIMEOUT_SEC: u64 = 8;
/// 响应体上限(与内置渠道同一分块累加器)。
const MAX_BODY: usize = 2_000_000;

#[derive(Debug, Clone)]
pub struct CustomResult {
    pub valid: bool,
    /// 已按 scale 折算的显示值;探活模式或没解析到 = None
    pub remaining: Option<f64>,
    pub error: Option<String>,
    /// 实际打通的地址(报告与界面都要能说清"用的哪个域名")
    pub url_used: Option<String>,
    pub probe_only: bool,
}

/// 打一次自定义渠道。**只读**:GET 或带 body 的 POST,绝不写平台数据。
pub async fn fetch_custom(
    client: &reqwest::Client,
    ch: &CustomChannel,
    key: &str,
) -> CustomResult {
    let probe_only = ch.probe_only;
    let url = if probe_only {
        ch.probe_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&ch.url)
            .to_string()
    } else {
        ch.url.trim().to_string()
    };
    let fail = |msg: String, url: &Option<String>| CustomResult {
        valid: false,
        remaining: None,
        error: Some(msg),
        url_used: url.clone(),
        probe_only,
    };
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return fail("地址要以 http:// 或 https:// 开头".into(), &None);
    }
    let req = if ch.normalized_method() == "POST" {
        let body = ch
            .body
            .as_deref()
            .unwrap_or("")
            .replace("{key}", key.trim());
        client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body)
    } else {
        client.get(&url)
    };
    let resp = req
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {}", key.trim()))
        .timeout(std::time::Duration::from_secs(TIMEOUT_SEC))
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "请求超时".to_string()
            } else if e.is_connect() {
                "无法连接(检查网络或代理)".to_string()
            } else {
                format!("请求失败: {e}")
            }
        });
    let resp = match resp {
        Ok(r) => r,
        Err(e) => return fail(e, &Some(url)),
    };
    let status = resp.status();
    if let Some(len) = resp.content_length() {
        if len > MAX_BODY as u64 {
            return fail(format!("响应过大({len} 字节),已丢弃"), &Some(url));
        }
    }
    let mut out: Vec<u8> = Vec::new();
    let mut resp = resp;
    loop {
        match resp.chunk().await {
            Ok(Some(c)) => {
                if let Err(e) = crate::fetch::push_chunk(&mut out, &c, MAX_BODY) {
                    return fail(e, &Some(url));
                }
            }
            Ok(None) => break,
            Err(e) => return fail(format!("读取响应失败: {e}"), &Some(url)),
        }
    }
    let body: Value = match serde_json::from_slice(&out) {
        Ok(v) => v,
        Err(_) => {
            // 探活模式不要求 JSON(有些站点根路径返回 HTML);解析模式则必须给 JSON
            if probe_only && status.is_success() {
                return CustomResult {
                    valid: true,
                    remaining: None,
                    error: None,
                    url_used: Some(url),
                    probe_only,
                };
            }
            return fail(format!("响应不是合法 JSON (HTTP {})", status.as_u16()), &Some(url));
        }
    };
    if !status.is_success() {
        let msg = body
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .or_else(|| body.get("message").and_then(|m| m.as_str()))
            .unwrap_or("");
        return fail(
            if msg.is_empty() {
                format!("HTTP {}", status.as_u16())
            } else {
                format!("HTTP {} · {msg}", status.as_u16())
            },
            &Some(url),
        );
    }
    if probe_only {
        return CustomResult {
            valid: true,
            remaining: None,
            error: None,
            url_used: Some(url),
            probe_only,
        };
    }
    match pick(&body, &ch.path) {
        Some(raw) => CustomResult {
            valid: true,
            remaining: Some(raw / ch.normalized_scale()),
            error: None,
            url_used: Some(url),
            probe_only,
        },
        None => fail(
            format!("响应里按「{}」找不到数字 —— 字段路径可能不对", ch.path),
            &Some(url),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn path_walker_handles_names_indexes_and_digits() {
        let v = json!({
            "data": { "quota": 250000, "balance": "12.50", "infos": [{"remain": 7}, {"remain": 9}] },
            "usage": { "limits": [ { "type": "TOKENS_LIMIT", "percent": 42.5 } ] }
        });
        assert_eq!(pick(&v, "data.quota"), Some(250000.0));
        assert_eq!(pick(&v, "data.balance"), Some(12.5), "数字字符串也认");
        assert_eq!(pick(&v, "data.infos[1].remain"), Some(9.0));
        assert_eq!(pick(&v, "usage.limits[0].percent"), Some(42.5));
        // 路径写错 = 取不到,不 panic
        assert_eq!(pick(&v, "data.nope"), None);
        assert_eq!(pick(&v, "data.infos[9].remain"), None);
        assert_eq!(pick(&v, "data.[bad]"), None);
        assert_eq!(pick(&v, ""), None, "空路径指向对象,不是数字");
    }

    #[test]
    fn scale_and_unit_are_normalized() {
        let mut ch = CustomChannel {
            url: "https://x.example/v1/usage".into(),
            name: "中转站".into(),
            path: "data.quota".into(),
            scale: 500000.0,
            ..Default::default()
        };
        assert_eq!(ch.normalized_scale(), 500000.0);
        ch.scale = 0.0;
        assert_eq!(ch.normalized_scale(), 1.0, "没填倍数 = 原样");
        assert_eq!(ch.normalized_unit(), "CNY");
        ch.unit = "usd".into();
        assert_eq!(ch.normalized_unit(), "USD");
        assert_eq!(CustomChannel::default().normalized_method(), "GET");
        ch.method = "post".into();
        assert_eq!(ch.normalized_method(), "POST");
    }

    #[test]
    fn validate_gates_the_dangerous_combinations() {
        let base = CustomChannel {
            url: "https://x.example/api".into(),
            name: "中转站".into(),
            ..Default::default()
        };
        assert!(base.validate().is_err(), "解析模式必须有字段路径");
        let mut probe = base.clone();
        probe.probe_only = true;
        assert!(probe.validate().is_ok(), "探活模式不需要路径");
        probe.path = "data.quota".into();
        assert!(probe.validate().is_err(), "探活模式填了路径反而矛盾");
        let mut ok = base.clone();
        ok.path = "data.quota".into();
        assert!(ok.validate().is_ok());
        let mut bad = ok.clone();
        bad.url = "ftp://x".into();
        assert!(bad.validate().is_err());
        bad.url = "https://x.example/api".into();
        bad.name = "  ".into();
        assert!(bad.validate().is_err());
    }
}
