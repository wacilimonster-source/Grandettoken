# Grandettoken 本地模型网关对标 Magpie 升级详细产品与技术设计

> 版本：v1.0 · 日期：2026-11-23  
> 关联代码：`app/core/src/gateway/` · `app/core/src/gateway_proto/` · `app/core/src/store_gateway.rs` · `app/core/src/inject_cli.rs` · `app/core/src/inject_gateway.rs` · `app/src-tauri/src/gateway_ext.rs` · `app/src/gateway-advanced.js`  
> 上游裁决约束：严格遵守 `DESIGN.md` §16（保留局域网网关；`OpenAiWire::Responses` 只作上游选项）及 `AGENTS.md` §2/§4/§7/§8.6。

---

## 1. 背景、定位与差异化取舍

对标开源本地网关 **`yetone/magpie`**，Grandettoken 的本地模型网关原本已完成 OpenAI Chat Completions 入口、Responses 上游协议转换、三类接入源（`openai_compat` / `agent_platform` / `gemini_aistudio`）、`<think>` / `google.thought` 思维链归一及 400 自动修复。  
但在日常多模型编码 Agent 与长对话场景下，仍有五大明显短板：

1. **无虚拟路由与自动容灾**：仅支持 1 对 1 的 `alias/model`，上游 429 限流、余额归零或 5xx 故障时下游客户端直接报错中断；且未能利用 Grandettoken 已有的**真实余额、7 天临期积分、DeepSeek 峰谷时段与窗口剩余比**。
2. **无会话粘性与缓存统计**：多候选切换若不粘连同一上游会导致 Prompt Cache 失效；诊断页未采集 `cached_tokens` / `cache_read_input_tokens` / `reasoning_tokens`。
3. **诊断仅驻留内存 50 条**：重启即丢，未借助已有 SQLite `Store` 与价目表沉淀请求成功率、TTFT 分布、缓存命中率与估算费用。
4. **缺 Anthropic 原生入口与可选隐私脱敏**：Claude Code 等工具依赖 `/v1/messages`（含 `/v1/messages/count_tokens`）；部分用户希望出站前对密钥/邮箱/私钥等敏感文本做本地占位符脱敏。
5. **客户端一键配置直接写入真实上游密钥**：`inject.rs` 目前把真实上游 API Key 以明文落盘至外部 IDE 配置，既增加敏感凭证暴露面，也无法在不重配客户端的情况下在网关侧切换模型或启用容灾。

### 明确保留与明确不做

| 决策 | 结论 | 依据 |
|---|---|---|
| 局域网网关（`lan: true` + 密钥鉴权） | **保留** | `DESIGN.md` §16 裁决：同局域网移动设备依赖该能力，不改只绑 `127.0.0.1` |
| `OpenAiWire::Responses` | **仅作上游选项** | `DESIGN.md` §16 裁决：OpenAI 客户端入口维持 `/v1/chat/completions` |
| OAuth / 网页订阅逆向转推理网关上游 | **明确不做** | 高封号风险且违反平台风控边界；订阅端维持只读配额监控 + 重置顾问 |
| 盲目扩展 TUI / Docker / 80+ CLI | **明确不做** | 聚焦 Windows 轻桌面挂件 + 现有 5 个 IDE + Claude Code / Codex / OpenCode 三大主流编码 CLI |

---

## 2. 功能设计（F1 ~ F5）

### 2.1 (F1) 虚拟路由组 + 配额/临期/峰谷感知路由 + 首字节前自动容灾

#### 数据模型（挂于 `gateway::Settings`）
```rust
pub enum RoutingStrategy {
    Order,    // 按配置顺序依次回退
    Rotate,   // 按请求轮转负载均衡
    Expiring, // 7天内有临期积分者优先 > DeepSeek 谷时优先 > 原顺序
    Pace,     // 按渠道剩余额度/配额比例从高到低分配（避免单一渠道瞬间打穿）
}

pub struct RouteTarget {
    pub channel_id: String,
    pub model: String, // 上游模型 ID（非 alias/model）
}

pub struct RouteGroup {
    pub id: String,              // 小写 [a-z0-9_-]{1,32}，对外暴露模型名为 `group/<id>`
    pub name: String,            // 展示名称
    pub strategy: RoutingStrategy,
    pub sticky: bool,            // 默认 true
    pub sticky_ttl_secs: u64,    // 默认 1800 (30分钟)
    pub cooldown_secs: u64,      // 默认 60
    pub targets: Vec<RouteTarget>,
}
```
- **暴露路径**：`/v1/models` 与 `/v1/models/{*id}` 自动列出所有至少包含 1 个有效（已上架且所属渠道密钥存在且该模型在有效模型内）目标的 `group/<id>`（`owned_by = "routing_group"`）。若与普通 `alias` 重名，规范化校验禁止把 `group` 作为普通渠道别名或拦截非法路由组 ID。

#### 渠道实时情报信号（`ChannelSignal`）
每次后端完成一轮余额/配额抓取并在 `emit_channels` 推送视图时，顺便把各渠道（按平台 ID / 渠道 ID 归一）的只读信号同步到 `Gateway`：
- `remaining_ratio: Option<f64>`：
  - 配额型（OpenCode Go / Codex / Antigravity）：取主窗口 `remain_percent / 100.0`；
  - 金额型 / 积分型：若 `total > 0` 且 `remaining >= 0`，取 `(remaining / total).clamp(0.0, 1.0)`；若充值型无上限（如 DeepSeek），保留 `None`。
- `has_expiring_7d: bool` & `soonest_expiry_ts: Option<i64>`：7 个本地自然日内到期的积分是否存在及最早到期时间戳。
- `is_deepseek: bool`：用于结合 `crate::peak::info_at(now_ts)` 实时判定当前是否处于北京时间谷价时段（工作日 09:00–12:00、14:00–18:00 之外及周末/法定节假日）。
- 信号与网关已上架 `Published` 渠道的关联优先级：渠道 `template`（如 `deepseek`/`4sapi`/`hapi`/`opencode-go`）→ 密钥箱 `platform` 归一 slug → 域名匹配。

#### 候选排序与调度算法（`gateway/routing.rs`）
1. 过滤掉不存在、已被下架、或失效的候选。
2. 拆分为「健康（未在冷却期内）」与「冷却中（按最早解冻时间升序）」两组；健康候选永远排在冷却候选之前；全员冷却时不直接死锁，允许按最早解冻顺序兜底试探一次。
3. 健康组内部按 `strategy` 排序：
   - `Order`：保持用户配置顺序；
   - `Rotate`：按 `rr_cursors[group.id]` 循环偏移后递增游标；
   - `Expiring`：四档优先级：
     `(has_expiring_7d == true, soonest_expiry_ts ASC)` > `(is_deepseek && !peak_now)` > 其它；同档维持原序；
   - `Pace`：`remaining_ratio` 降序（已知比例优先于未知，未知维持原序）。
4. **Sticky 粘性覆盖**：若 `group.sticky == true` 且计算出有效 `session_hash`，且 `sticky[group.id, session_hash]` 未过期、所指目标仍存在且处于健康状态，则将该目标提升为第 1 候选。

#### 熔断冷却与首字节前容灾边界
- **可容灾切换条件（仅在向下游写出任何字节之前）**：
  - 上游 HTTP 状态码：`429`、`408`、`500..=599`、`401`、`403`、`404`；
  - 内部准备/传输失败：`missing_upstream_key`、`proxy_init_failed`、`upstream_unreachable`、`upstream_timeout`、`gateway_protocol`（非流式返回非法 JSON 或内含 `error` 字段）。
- **不可盲切条件**：
  - `400` 客户端入参错误（已经过单次 `apply_400_repairs` 仍报 400）或 `responses_conversion` 失败——直接将错误返给下游。
  - **一旦进入 SSE 流式响应构建阶段（已确定 HTTP 200 且准备下发流）**：不再切备，避免两段不同模型的输出拼接到同一条流里。
- **冷却时长**：若上游响应含 `Retry-After`（秒数或 HTTP 日期解析），clamp 至 `[5, 600]` 秒；否则取 `group.cooldown_secs`（`401/403` 至少冷却 `max(cooldown_secs, 120)` 秒）。
- 成功后在 HTTP 响应头附加 `x-gateway-route-group` 与 `x-gateway-upstream-channel`（仅辅助标识，不含密钥），并将该通道刷新写入 Sticky 映射。

---

### 2.2 (F2) Sticky 会话粘性与 Prompt Cache Token 统计

- **会话指纹（`derive_session_hash`）**：
  - 优先级 1：读取 `x-session-id` / `x-conversation-id` / `anthropic-session-id` 请求头；
  - 优先级 2：对 `system` 消息与首条 `user` 消息前 512 字符做规范化拼接后计算 SHA-256，截取前 16 字节 hex。
  - **隐私红线**：内存仅保存 32 字符 hex 摘要，绝不留存任何用户文本原文。
- **Cache 与 Reasoning Token 采集**：
  - `StreamStats`、`GatewayDiagEntry`、`GatewayUsageRow` 统一增加：
    - `cached_prompt_tokens: Option<u64>`：依次识别 OpenAI `usage.prompt_tokens_details.cached_tokens`、Anthropic `usage.cache_read_input_tokens`、Gemini `usage.cached_content_token_count`；
    - `cache_creation_tokens: Option<u64>`：识别 `usage.cache_creation_input_tokens`；
    - `reasoning_tokens: Option<u64>`：识别 `usage.completion_tokens_details.reasoning_tokens` / `usage.output_tokens_details.reasoning_tokens`。
  - 缓存命中率口径：`total_prompt_tokens > 0` 时定义为 `cached_prompt_tokens / (prompt_tokens + cache_creation_tokens)`（OpenAI 口径中 `cached_tokens` 含于 `prompt_tokens`，Anthropic 口径中总输入约为 `input + cache_read + cache_creation`，适配器统一将总输入折算进 `prompt_tokens` 避免命中率 > 100%）。

---

### 2.3 (F3) SQLite 持久化网关使用台账

- **数据库迁移**：`store.rs` `WANT` 从 `6` 升至 `7`，迁移实现拆至 `app/core/src/store_gateway.rs`，满足 `store.rs <= 1952` 行棘轮。
- **表结构 `gateway_usage_log`**：仅存脱敏元数据（时间戳 `ts`、本地日期 `day`、`channel_id`、`channel_alias`、`route_group`、`model`、`protocol`、`stream`、`status`、`latency_ms`、`first_byte_ms`、`prompt_tokens`、`cached_prompt_tokens`、`cache_creation_tokens`、`completion_tokens`、`reasoning_tokens`、`failover_count`、`masked_count`、`estimated_cost_cny`、`error_code`）。**严禁出现 prompt/response 正文、密钥或敏感原文字段。**
- **费用估算口径**：
  - 仅在 `ModelCache` 中查到对应模型明确单价（且非缺失分组）时按 `prompt/cached/completion` 估算人民币费用（单位：元）；查不到单价（如本地 Ollama、OpenCode Go 订阅、自定义中转站）一律存 `NULL`，汇总时显示已覆盖估算笔数，**绝不把未知单价算作 0**。
- **生命周期管理**：单次写入失败只记 `diaglog`（不中断推理响应）；每 100 次写入自动清理 90 天前记录；界面支持 1 天 / 7 天 / 30 天汇总（请求数、成功率、缓存命中率、P50/P95 TTFT、按渠道/按模型分布）与手动清空。

---

### 2.4 (F4) Anthropic `/v1/messages` 入口适配与可选 Sticky Prompt 脱敏

#### Anthropic Messages 入口
- 新增路由：`POST /v1/messages`、`/messages` 与 `POST /v1/messages/count_tokens`、`/messages/count_tokens`。
- 鉴权复用 `peer_allowed` 与 `authorized_with`（原生支持 `x-api-key` 与 `Authorization: Bearer`）。
- **请求转换（`gateway_proto/anthropic.rs`）**：
  - `system`（字符串或 text 块数组）→ OpenAI `system` 消息；
  - `messages` 逐条转换：`text`、`image`（`base64` 转 `data:<media_type>;base64,...` / `url`）、assistant `tool_use`（转 `tool_calls`）、user `tool_result`（转一条或多条 `role: "tool"` 消息）；`thinking` 历史块安全跳过；
  - `tools` → OpenAI `function` 工具数组；`tool_choice`（`auto` / `any`→`required` / `none` / `tool`→指定函数）；
  - `thinking: { type: "enabled", budget_tokens }` → `include_reasoning: true` 并按预算映射 `reasoning_effort`（`<= 2048` → `low`，`<= 8192` → `medium`，`> 8192` → `high`）；`stop_sequences` → `stop`。
- **响应与 SSE 状态机（`AnthropicSseTranslator`）**：
  - 非流式：按 `thinking` → `text` → `tool_use` 顺序构造 `content` 块，并映射 `stop_reason`（`end_turn` / `max_tokens` / `tool_use`）与 Anthropic `usage`。
  - 流式：消费规范化 `ChatSse` 帧，严格维护当前打开的 `content_block` 索引与类型（同一时刻最多一个未闭合块，切换 `thinking`→`text`→`tool_use[i]` 时先发 `content_block_stop` 再开新块），确保 `message_start` → `content_block_*` → `message_delta` → `message_stop` 状态闭环；异常断流发 `event: error`。
  - `/v1/messages/count_tokens`：对 `system`、`messages`、`tools` 执行本地确定性字符权重估算（ASCII ~4 字/tok，CJK ~1.5 字/tok），不消耗上游额度。

#### 可选 Sticky Prompt 脱敏（默认关闭）
- 配置 `MaskingSettings`：`enabled: false`（默认关闭）、`sticky: true`、四类开关（API Keys `sk-`/`AIza`/`ghp_`/`AKIA` 等、邮箱地址、PEM 私钥块、常见赋值密码/Token 串 `password=...`/`token=...`）及自定义敏感词（长度 `>= 4` 才生效）。
- **Sticky 会话映射表**：纯内存挂于 `gateway::Inner`（上限 1024 会话、TTL 1 小时，**永不落盘**）。同一会话内同一原始值固定映射为同一占位符（如 `[MASKED_KEY_01]`），保证多轮对话 Prompt Cache 前缀稳定且不破坏上下文引用。
- **流式反脱敏（`StreamUnmasker`）**：逐块扫描正文与思维链增量，若字符串末尾匹配 `[MASKED_` 前缀或未闭合的 `[MASKED_...`（最长 32 字符）则暂存到下一分片再拼合还原，避免占位符跨 SSE 分片被截断而漏出。

---

### 2.5 (F5) 客户端「通过本地网关接入」模式 + 现代 CLI 写入契约

#### 安全模型优势
与原有的直接向外部客户端写入明文上游 API Key 不同，**网关接入模式**下：
- 外部客户端配置文件仅写入**本地网关地址**（如 `http://127.0.0.1:18771/v1` 或 `http://127.0.0.1:18771`）与可随时轮换的**本地网关访问令牌**（当网关禁用鉴权且仅本机访问时写入占位值 `"grandettoken-local"`）。
- **真实上游 API Key 永远留在 Windows 凭据管理器中**，不会落盘到 Trae / WorkBuddy / ZCode / DSH / Claude Code / Codex / OpenCode 的任何配置文件；同时客户端可无缝享受虚拟路由组 `group/<id>` 与自动容灾。

#### 三类现代 CLI 写入契约（`app/core/src/inject_cli.rs`）
均严格遵守现有四条安全前提：**进程未运行检查** → **Windows DPAPI 加密备份（含新建标记，使用 `backup_layered` 保障新建文件回滚可删）** → **精准局部编辑（保留原文件注释、缩进与其它配置项）** → **写后读回校验（不通过即自动回滚）**：

| 客户端 | 配置落点 | 进程检查 | 局部编辑与取消写入策略 |
|---|---|---|---|
| **Claude Code** | `~/.claude/settings.json` | `claude.exe` | 局部更新 `env.ANTHROPIC_BASE_URL`（本地网关根地址）、`env.ANTHROPIC_AUTH_TOKEN`、`env.ANTHROPIC_MODEL`；保留其余 `env` 与顶层设置；取消仅移除托管的 3 个 `env` 键 |
| **Codex** | `~/.codex/config.toml` | `codex.exe` | 行级保注释编辑：更新顶层 `model_provider = "grandettoken"`、`model = "<id>"` 并在文末或原区块以 `# grandettoken-managed-start` / `# grandettoken-managed-end` 维护 `[model_providers.grandettoken]`；保留用户全部原有注释与其它 provider；取消仅还原或摘除托管段落 |
| **OpenCode** | `~/.config/opencode/opencode.jsonc`（优先）或 `opencode.json` | `opencode.exe` | 字符串感知剥离 JSONC 注释进行校验解析，保留原文件头部注释块并局部更新 `provider.grandettoken`（`@ai-sdk/openai-compatible`）与 `model`；取消仅移除 `provider.grandettoken` 并在 `model` 指向它时重置 |

- 对现有 5 个客户端（Trae / WorkBuddy / WorkBuddy AI / ZCode / DSH）：通过 `inject_gateway.rs` 复用其经过实战验证的写入契约，使用 `source = "gateway"`（显示名前缀 `"GW-"`）接入本地网关。

---

## 3. 模块拆分与体积棘轮合规方案

为严格遵守 `build/check-file-size.js` 棘轮（不修改任何既有预算上限）：
- `app/core/src/store.rs`（上限 1952）：仅更新 `WANT = 7`、暴露 `conn()` 并调用 `migrate_gateway_ledger()`，新增台账全部放入 **`app/core/src/store_gateway.rs`**。
- `app/core/src/gateway/http.rs`（非预算文件受 1500 行软限）：路由组容灾调度拆入 **`http/dispatch.rs`**，Anthropic 入口拆入 **`http/anthropic.rs`**。
- `app/core/src/gateway/routing.rs`：纯路由决策、冷却、Sticky、信号结构与单测。
- `app/core/src/gateway_proto/anthropic.rs`、`app/core/src/gateway_proto/masking.rs`：纯协议与脱敏逻辑，零 Tauri/Axum 耦合。
- `app/core/src/inject.rs`（上限 2276）：外移 `CATALOG_DISPLAY` 等静态数据/小辅助函数到 `inject_io.rs` 以腾出余量，CLI 逻辑放入 **`inject_cli.rs`**，网关接入逻辑放入 **`inject_gateway.rs`**。
- `app/src-tauri/src/gateway_ext.rs`：新增 Tauri IPC 命令独立成模块并在 `lib.rs` 注册。
- `app/src/gateway-advanced.js` + `app/src/gateway-view.css`：前端新面板独立拆出，避免触碰 `styles.css`（上限 1737）及 `app-*.js`（组上限 9007）；`gateway-view.js` 仅做最小分栏挂载，完全保留 `build/verify-gateway.js` 全部 13 个哨兵锚点。

---

## 4. 验收与双向测试矩阵

1. **Rust 单元测试与真实 Axum 回环测试（`NO_PROXY=127.0.0.1,localhost,::1`）**：
   - 路由策略 `order` / `rotate` / `expiring`（7 天临期优先 + DeepSeek 谷时优先）/ `pace`；
   - `429 Retry-After` / `503` 首字节前自动切备成功，400 校验失败不盲切，Sticky 连续命中，容灾次数与缓存 token 写入 SQLite 台账；
   - `/v1/messages`（非流式 + SSE 流式 + thinking + tool_use + cache token）与 `/v1/messages/count_tokens`；
   - Sticky Prompt 脱敏（默认关；多轮占位符一致性；跨 SSE 分片 `[MASKED_KEY_01]` 完整还原；错误与台账零原文泄露）；
   - Claude Code JSON / Codex 保注释 TOML / OpenCode 保注释 JSONC 写入、读回校验、取消写入、新建文件回滚与网关模式零上游密钥落盘。
2. **前端双向冒烟（`build/verify-gateway.js` + 新增 `build/verify-gateway-magpie.js`）**：
   - 验证旧网关 13 个哨兵零回归；
   - 验证新哨兵坏样本（脱敏默认误开、保存失败未回滚、缺少写入确认预览、未知 IPC 命令）全部触发指定 `FAIL:`。
3. **全量仓库门禁**：按 `AGENTS.md` §4.1 串行跑通 IPC 契约、静默失败、文件规模、CSS 形态、UTF-8 编码、app 拆分、锁中毒、Cargo 测试/检查与凭据扫描。
