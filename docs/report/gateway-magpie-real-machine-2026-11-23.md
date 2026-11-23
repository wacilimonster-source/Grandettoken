# 对标 Magpie 网关升级实机验收记录（2026-11-23）

> 对象：`docs/design/design-gateway-magpie-upgrade-2026-11-23.md` F1–F5，随 v0.3.73 发布。
> 方法：`build/build.ps1 build` 出 release exe → `build/launch-debug.ps1 -Port 9241 -Exe <新 exe>` →
> CDP（`Runtime.evaluate` / `Page.captureScreenshot`）驱动**真实应用进程**；上游为回环本地假上游
> `127.0.0.1:18991`（按模型名返回 429+`Retry-After: 7` / 503 / 400 / 正常 JSON / SSE / `reasoning_content`），
> 不消耗真实额度、不读取真实上游密钥。用户数据先 SHA-256 备份、测完还原并逐字节校验。
> 一次性脚本与假上游放 `build/tmp/`（gitignore），本轮结束已删；本文件留结论与证据。

## 1. 断言矩阵（134 项全过）

| 组 | 项数 | 覆盖 |
|---|---|---|
| 网关功能 | 68 | 免密上架、`group/<id>` 暴露与 `owned_by=routing_group`、别名 `group` 保留字拦截、空成员组拒绝、429 首字节前切备（上游先 429 后正常）、响应头 `x-gateway-route-group=group/<id>` 与 `x-gateway-upstream-channel=<别名>`、冷却快照剩余秒数=Retry-After、Sticky 第二轮单上游、400 不盲切、Anthropic 非流式（content 块/`stop_reason`/usage 折算/`system`→`role:system`）、SSE 状态闭环与开闭配对、thinking 预算 4096→medium / 1024→low、`include_reasoning` 出站前被消费、thinking→text 块序列、`count_tokens` 零上游、无/错令牌 401、脱敏默认关/开启/分类开关/关回、跨分片还原、台账 11 笔与零正文零密钥 |
| UI 走查 | 39 | 6 页签、三新面板真实渲染、四策略下拉、账本 1/7/30 天与清空、8 客户端清单与运行中徽章、预览只含回环地址、运行中拒绝写入、走查期间零 JS 异常零 console error |
| UI 深查 | 17 | 冷却徽章（刷新数据后显示「剩余 Ns · HTTP 429」）、预览框标题写明客户端与模型、目标文件列表、被阻断时「确认写入」禁用、未阻断时可点、收起预览后按钮消失 |
| 策略与生命周期 | 10 | rotate 四请求相邻命中不同且覆盖两候选、pace 可保存可推理、删组后 `/v1/models` 不再暴露 |

台账口径实测：`cacheHitRate=0.6`、`ttftP50Ms=1 / ttftP95Ms=2`、`failoverRequests>=1`、
`pricedRequests=0` 且 `estimatedCostCny=null`（假上游无目录单价，**不算 0 元**）。
`gateway_usage_log` 实表 21 列全为元数据（时间/渠道/模型/协议/状态/耗时/token/容灾/脱敏/费用/错误码），
无 prompt/response/密钥列；汇总 JSON 不含测试密钥与 prompt 原文。

## 2. 真实数据库迁移

用户库 `%APPDATA%\com.wacil.tokenscope\tokenscope.db`（85,868,544 B）：
测前 `user_version=6`、`snapshots=481,689`、8 表、`integrity_check=ok`；
新二进制启动后 `user_version=7`、9 表（新增 `gateway_usage_log`）、`snapshots=481,902`（只增不减）、`integrity_check=ok`。
测后还原：`user_version=6`、`snapshots=481,689`、8 表、`integrity_check=ok`；
`local_gateway_v1` 回到 `port=18771, lan=true, auth=true`、原 3 渠道（ss/agent/as）、0 路由组。

## 3. 实机抓出并修复的 3 个缺陷（均双向证红）

1. **OpenCode 进程守卫漏判**：本机 OpenCode 桌面版进程为
   `opencode-cli.exe`（`%APPDATA%\ai.opencode.desktop\cli\2.3.2-x64\`）与辅助 `opencode-pty.exe`，
   `tasklist /FI "IMAGENAME eq opencode.exe"` 无命中 → 原实现会边跑边写真实配置。
   修：`Client::processes()` 全名任一命中（`app/core/src/inject.rs`），`inject_cli::cli_running` 逐名探测，
   `state()` 徽章显示实际命中名。新增单测 `opencode_desktop_cli_process_also_blocks_writing`；
   临时撤掉修复后该单测 FAILED（exit 101），恢复后 4/4 过。
   实机复核：`gateway_inject_clients` 报 `opencode running=true process=opencode-cli.exe`；
   `gateway_inject_apply` 被拒；`~/.codex/config.toml` 与 `~/.config/opencode/opencode.jsonc`
   SHA-256 与测前一致（`fc6c30a8…` / `5c109148…`），`~/.claude/settings.json` 未被创建。
2. **用量账本横向越界**：`.gw-tab-pane` 为 grid，隐式 auto 轨道被账本表格 `white-space:nowrap`
   的最小内容撑宽：513px 容器内卡片 713px（单列/缩略 444px 容器内 612px）。
   修：`grid-template-columns:minmax(0,1fr)`（`app/src/gateway-view.css`），表格改在卡片内滚动。
   新增 `build/verify-gateway-layout.js`（3 布局 × 3 面板 + 超宽卡片坏样本自检），纳入
   `build/run-native-checks.ps1`（7→8 项）。未修二进制上 exit 1（4 FAIL），修后 9/9 PASS。
3. **优惠中文 slug 解析失败**：官网第 2 页第 7 条 url 为
   `/promos/0-%E5%85%83-%E5%BC%80%E9%80%9A%E8%AF%95%E7%94%A8-gemini-ultra-x20`，
   原 ASCII 正则拒收 → 多页取数任一页坏即整体抛错 → 真机「更新失败」永久化（旧快照 24 条保留）。
   修：`decodeURIComponent(link.pathname)` 后按 `[\p{L}\p{N}-]+` 校验并查解码卡片 id，
   id/链接保留编码原样；解码失败或解码后路径穿越仍拒。jsdom 309→312（新增 3 项），
   临时撤掉修复精确红 2 项（exit 1）。真机复核：刷新成功取前 3 页 72 条，
   编码条目带摘要「通过强制试用，获得 Gemini Ultra X…」，失败提示清空。
   注：JSON-LD 顶层数组由既有 `flatMap` 自然展平，**不是**缺陷（诊断初期误判，已排除）。

## 4. 原生 8 项回归与门禁

- `run-native-checks.ps1 -Port 9241 -Exe <新 exe>`：6 PASS / 2 FAIL。
  失败为 `verify-ui-0923.js`（U2 失联副标题、U6 官方标 4 vs 2、U6 紧凑条字母）与
  `verify-form.js`（compact 期望尺寸、`win-style.ps1` 按窗口标题找窗），与 B11 记录逐字相同，
  旧二进制同样失败 → 非本版回归。新增 `verify-gateway-layout.js` PASS（16.2s）。
  每轮用户数据 SHA-256 备份还原校验通过。
- 门禁串行复跑：静态门含 `--self-test` 全 PASS；jsdom 312/312；
  vault / codex-reset / google-cloud / antigravity-capture / gateway / gateway-magpie 全 PASS；
  core `cargo test --locked -- --test-threads=1` 673 passed / 0 failed / 10 ignored，`sync_fixtures` 6 passed；
  core 与 src-tauri `cargo check --all-targets --locked` 零警告（实际重编）；`scan-secrets.ps1` CLEAN。

## 5. 已知边界（不阻断，进 acceptance-backlog）

- B15：三个现代 CLI 的真实写入/取消/新建回滚未真机实测（本机 Claude Code 未安装、Codex/OpenCode 在跑被正确拒绝）；隔离目录单测已覆盖。
- B16：容灾/Anthropic/脱敏/台账只用假上游验过；真实渠道鉴权与真实返回格式未验（耗额度）。
- B17：路由组页签冷却徽章不随时间自动刷新（切页签不重取 `gateway_status`）；「预览接入」起 tasklist 约 4.6–5.1s 无加载态。路由判定本身用实时冷却，不受影响。
- 本机安装版仍为 v0.3.71（优惠旧 RSS 已 410），其「更新失败」角标属旧版 persisted 状态；v0.3.73 安装后按 B13 复核。