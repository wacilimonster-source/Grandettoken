# Grandettoken 全面代码审查与实机测试报告（2026-11-26）

- **审查对象**：`v0.3.78`（`22e2a4f`，工作区干净）
- **审查方式**：只读代码审查 + 自动化门禁 + Release 构建实机测试（未修改产品源码）
- **环境**：Windows 11 / i5-13490F / 32 GB；2560×1440 @125%（工作区 2560×1380）；Rust 1.98.1；Node 24.16.0；WebView2（`tokenscope.exe` Release，11.33 MB）
- **结论**：发现 **1 个 P0：公开仓库中的 QA 截图含真实手机号/账户信息**（详见 §3 P0-1，需立即处置）；其余整体健康，无 P0。命令面、CSP、密钥存储、窗口/托盘/休眠线程模型、主列表 keyed 渲染均优于同类项目；主要优化空间集中在**快照写入放大、休眠与网页会话的交互、网关台账/大响应边界、前端渲染节流与可访问性**。
- **原始证据**：`build/qa/code-review-2026-11-26/`（已被 `.gitignore`，不入库）

## 1. 执行摘要

| 维度 | 评价 | 关键证据 |
|---|---|---|
| 功能与门禁 | 强 | 702 个 core 单测 + 6 个同步夹具全绿；静态门/9 哨兵/317 jsdom/专项/凭据扫描全绿；双端 0 warning |
| 原生稳定性 | 强 | 原生 CDP 6/8（2 项仅 B11 已知断言）；页面切换 3–27 ms；20 轮切页 DOM/监听器回到基线；无加载期 JS 错误 |
| 性能 | 中上 | 冷启动 0.73 s DOM / 1.42 s 首屏数据；空闲 CPU 0.41% 单核；历史批量查询 6–13 ms（旧路径 16–24×）；但 WebView2 常驻约 445–548 MB |
| 内存 | 有明确抓手 | 休眠后 537 MB → 47.4 MB；但设置页全部常驻 DOM 使打开过设置后常驻 +77 MB |
| 数据层 | 有高杠杆优化 | 92 MB / 51.9 万条快照中 **99.1% 与上一条完全相同**；freelist 18 MB；台账查询无 LIMIT/GROUP BY |
| 安全 | **1 个 P0 + 2 个边界项** | **公开仓库 QA 截图含真实手机号/账户信息（P0）**；密钥进 Credential Manager；CSP 禁内联脚本；网关 401/Origin 拒绝正确；但自定义请求头明文入库、Gist 响应先下载后限长、Release 先上传后验签 |
| 可维护性 | 中 | 前端 12.3k 行、`app*.js` 预算 **8774/8780（仅 6 行余量）**；174 条 Clippy（多为风格）；10 个 ignored 测试与 B11 过时脚本 |

**最值得先做的 6 件事**：快照去重 + 心跳、休眠感知的取数/签到门、修复只读连接上的周期学习写入、台账 SQL 聚合、Gist 流式限长、自定义请求头的密钥策略。

## 2. 实机测量结果

### 2.1 启动与空闲

| 指标 | 实测 |
|---|---|
| 进程启动 → CDP 页面可用 | **725 ms** |
| 进程启动 → DOM `complete` | **734 ms** |
| 进程启动 → 首屏出现渠道数据 | **1421 ms** |
| Rust 进程初始工作集 / 线程 / 句柄 | 46.7 MB / 46 / 589 |
| 空闲 60 s CPU | **0.25 s → 0.41% 单核** |
| 空闲进程树（含 6 个 WebView2 子进程） | 537–548 MB |

### 2.2 WebView2 进程构成（启动 12 s，缩略布局）

| 类型 | 进程数 | 工作集 |
|---|---|---|
| browser/host | 2 | 188.0 MB |
| renderer | 1 | 104.6 MB |
| gpu-process | 1 | 79.9 MB |
| utility | 2 | 61.7 MB |
| crashpad-handler | 1 | 10.5 MB |
| `tokenscope.exe`（Rust） | 1 | 49.5 MB |
| **合计** | **8** | **444.6 MB** |

> 结论：内存大头是 Chromium 运行时，不是 Rust 业务层；仅靠 Rust 侧优化无法显著降低常驻内存。

### 2.3 UI、渲染与切页

- 深/浅主题、单列/双列/缩略 × 标准/大/特大字号 9 组全部无横向越界，列表可见；9 个设置子页均可切换。
- 网关页刷新后 `ch`/`disp` 正确标脏，切回后立即补绘，脏标记清零。
- 页面切换（暖缓存）：`ch` 3–8 ms、`ck` 6–24 ms、`mdl` 5–10 ms、`disp` 3–27 ms、`gateway` 3–6 ms、其余 4–13 ms。
- 20 轮“打开设置 → 网关 → 渠道 → 关闭”：DOM 节点最终回到 **10,008**、监听器 **174**，无单调增长；GC 后 JS 堆约 3.0 MB（基线 1.9 MB，差异来自设置面板常驻）。
- **设置面板首开后不释放**：进程树 445.3 MB → 514.4 MB，30 s 后 522.2 MB；这是“一次性常驻”而非持续泄漏，但占用了可观的渲染内存。

### 2.4 托盘

- 125% 下 `94`（绿）/`17`（红）清晰、居中、无裁边；轮播 `OpenCode Go 94.0% → Codex 17.0%` 与 tooltip 同步。
- 关闭“显示余额数字”立即还原品牌图标，重新打开恢复数字。
- 启动、轮播、开关切换前后 GDI 对象 **21 → 21**、User 对象 47–51，无 GDI 泄漏。
- 16 px 下四字符（如 `12.5`）偏紧，20 px 以上清晰（P3，已在前一版记录）。

### 2.5 省内存模式

- 隐藏 4 个网页会话渠道并开启休眠：**537.1 MB → 47.4 MB，0 个 WebView2 进程**；回环网关 200、快照时间戳继续推进、托盘 tooltip 继续轮播；热键唤回 **741 ms**，几何与 11 行缓存首屏恢复。
- 但 `needs_webview()` 对 `workbuddy`/`doubao` 的 `None` 状态按“保守保留”处理（`hibernate.rs`），因此**配置了这些渠道且会话状态未知/在线时，主 WebView 不会被释放**；只有显式隐藏这些渠道或确认无会话时才进入上面的 47 MB 路径。见 P1-2。

### 2.6 本地网关

- 监听 `0.0.0.0:18771`（DESIGN §16 明确保留局域网访问，不改为仅 127.0.0.1）。
- `/healthz`：本机 200，局域网 IP 200；带 `Origin` 头返回 401。
- `/v1/models` 无 key / 错误 key：**401 + JSON** `{"error":{"message":"网关密钥无效","type":"gateway_error"}}`。
- `/healthz` 单次 curl 23–25 ms（含进程启动开销；路由本身为常量级）。
- 无前端 `fetch`/WebSocket/EventSource；所有出网在 Rust 侧。

### 2.7 数据库

| 指标 | 实测 |
|---|---|
| `user_version` | 9 |
| 文件大小 | 92.0 MB |
| `snapshots` 行数 | 518,852 |
| 时间范围 | 约 71 天（<90 天） |
| 与上一条完全相同的行 | **514,276（99.1%）** |
| 120 s 内重复的行 | 458,523（88.4%） |
| 10 min 内完全重复 | 513,731（99.0%） |
| >30 天 / >60 天 / >90 天 | 169,005 / 36,287 / 0 |
| freelist | 4,654 页（约 18.2 MB） |
| 历史批量查询（真实库副本） | **6.3–13.0 ms**；旧路径 149.8–206.4 ms（**16–24×**） |

> 快照表几乎每 60 s 写一行，即使余额完全没变；这是当前最大的存储/写入放大源。

### 2.8 自动化门禁

- core：702 passed / 0 failed / 10 ignored；`sync_fixtures` 6 passed；`cargo check --all-targets --locked` 双端 exit 0、**0 warning**。
- 静态门（IPC 111/111、静默失败、文件规模、CSS、编码、拆分、锁中毒、临期/套餐）全部通过；jsdom 317/317；6 项专项与 `verify-refresh-stability --assert` 通过；凭据扫描 `CLEAN`。
- 原生 CDP：6 PASS / 2 FAIL；失败仍仅命中 B11（U2 文案、U6 官方标数/紧凑条字母、compact 尺寸、`win-style.ps1` 标题），无新增失败、无加载期 JS 错误。

## 3. 代码审查发现

> 等级：P0 阻断 / P1 高优先 / P2 中优先 / P3 低优先或观察项。行号为审查时 `v0.3.78`。

### P0-1 公开仓库中的 QA 截图含真实手机号/账户信息（P0，立即处置）

- **事实**：仓库为 Public（GitHub API `private:false`），但 `build/qa/` 下有 **22 个受版本控制的文件**，其中 `build/qa/home-shot/` 的 9 张截图自 `v0.3.30`（`879e29e`）起被跟踪；`vault.png` 可见真实手机号、平台账号名、自定义端点、掩码密钥尾号；`panel.png` 可见用户余额；`manage.png` 可见渠道数量与同步开启状态。未发现完整 API Key/Cookie，但手机号与账户元数据已构成 PII 暴露（本报告不复述具体号码）。
- **影响范围**：标签 `v0.3.30`–`v0.3.78` 均包含这些文件；`v0.3.78` 的 GitHub 源码 zip 经下载核对含 **27 个 `build/qa/` 条目**，即所有该区间的 Release 源码归档都可下载到截图。仓库当前 0 fork / 0 star，降低了扩散面，但 GitHub 缓存视图、已下载的源码包与历史克隆仍可能保留。
- **根因**：`.gitignore` 后来才加入 `build/qa/`，但已跟踪文件不会自动移除；`scan-secrets.ps1` 只扫文本，图片无法命中；没有“受版本控制的 QA 产物”门禁。
- **处置建议**（需用户确认后执行，涉及强推/历史重写）：
  1. 当前树先 `git rm -r --cached build/qa` 并提交，阻止新克隆在 HEAD 看到这些文件；
  2. 用 `git filter-repo --invert-paths --path build/qa`（或 BFG）重写全部分支与标签，强推；0 fork 使重写代价可控；
  3. 联系 GitHub Support 清理 cached views 与旧源码归档；`v0.3.30`–`v0.3.78` 的 Release 源码包需要删除/重建，或明确接受历史包仍可下载；
  4. 增加 CI 门禁：`git ls-files build/qa` 必须为空，且禁止跟踪 QA 图片/二进制；
  5. 若该手机号同时是平台账号名，建议在平台侧改名；截图中的密钥仅掩码尾号，但仍建议评估是否需要轮换对应 key。
- **注意**：仅删除当前文件**不会**从历史、标签或已发布源码包中移除；必须做历史清理与缓存清理，否则只是“从 HEAD 隐藏”。

### P1-1 快照写入放大：99% 的行没有新信息（性能/存储，非缺陷）

- **证据**：`store/snapshots.rs:26-44` 的 `record_fp()` 每次都 `INSERT`；真实库 518,852 行中 514,276 行与同渠道/指纹的上一条 `(remaining, used, total, kind)` 完全相同；DB 92 MB、freelist 18 MB。
- **影响**：数据库随运行时长线性增长；每小时每渠道约 60 次写、绝大多数无信息；备份/同步/迁移体积被动变大；历史查询虽仍快（6–13 ms），但已接近 500k 行上限的红线。
- **方向**：仅在值变化时写入；未变化时每 15–30 min 写一条心跳（保留“最后观测时间”与缺口判断）。预计行数下降 **90% 左右**，DB 可回到 20–40 MB 量级，写入/ WAL 压力下降一个数量级。
- **注意**：需同时覆盖 `prev_remaining`、积分到账、消费窗口、缺口/趋势、跨月/夏令时与 `daily_summary` 归档；建议先加“写前比较 + 心跳”开关，用差分测试对照旧行为，再默认开启。可配合一次性 `VACUUM`/`incremental_vacuum` 回收现有 18 MB freelist（需用户显式操作或迁移时一次执行，不引入后台自动 VACUUM）。

### P1-2 休眠与网页会话状态相互矛盾（功能/数据正确性）

- **证据**：
  - `hibernate.rs` 的 `needs_webview()` 把 `workbuddy`/`doubao` 的 `None` 视为“保留”，只要这些渠道在配置中就很难释放；一旦会话被观测为 `Some(false)`，又会允许释放。
  - `poll.rs` 与 `checkin_scheduler.rs` 不感知 `Phase::Released`，释放后仍会取数/签到；
  - `weblogin_cmds.rs:282-294` 在 `!has_live_webview` 时直接返回 `None`；取数侧把它当成“没有登录凭据”，标记 `valid=false` 并累计 recovery；签到侧写入 `no_cred`、增加 attempts 并进入 120 s→1200 s 退避。
- **影响**：开启省内存模式的用户可能遇到两种坏结果之一：①配置了 WorkBuddy/豆包时 WebView 根本不释放（省内存失效）；②会话状态为未登录时释放了，但随后取数/签到把“休眠中不可读”误判为“没登录”，写入假失败记录并推迟真实签到。
- **方向**：引入 `PollGate`/`CheckinGate` 读取 `has_live_webview`；休眠期间对网页会话渠道返回明确的 `Hibernated` 状态，跳过而不记录失败、不累加 recovery/退避；唤回后立即补一轮。对 `needs_webview` 的 `None` 策略按“用户是否配置了该渠道 + 最近是否有会话”细分，而不是一律保留。
- **验证**：这是本次审查新增的路径，现有 300 s 验收只测了内存/几何，未测休眠中的渠道与签到状态；建议补一个“释放期间不写 no_cred / 唤回后一轮恢复”的集成测试。

### P2-3 周期学习写入只读连接，静默失败

- **证据**：`fetch_db.rs:278` 在 `finish_db()` 中锁 `reader`，`:344` 调用 `attach_plan_costs(&mut out, &s, ...)`；该函数在 `fetch_view.rs:159-161` 调 `save_cycle()` → `set_setting()`，失败仅 `eprintln!`。生产 `reader` 是 `Store::open_readonly()`（`startup.rs:93`，`PRAGMA query_only=ON`），写必然失败。
- **影响**：`opencode-go` 等套餐周期的学习结果永远不落库；每轮重复计算、诊断只在 stderr（GUI 不可见），属于 R2-1 引入的回归。
- **方向**：把 `save_cycle` 移到 writer 阶段（或把 writer 传给 `attach_plan_costs`）；失败用 `diaglog` + 用户可见提示；补一个“reader 为只读的取数整轮”测试。

### P2-4 网关用量汇总无上限、无 SQL 聚合

- **证据**：`store/gateway.rs:264-420` 的 `gateway_usage_summary()` 对 `WHERE ts >= ?1` 全量 `SELECT`，在 Rust 里逐行构建 `by_channel`/`by_model`/TTFT 向量；没有 `LIMIT`/`GROUP BY`；UI 默认 7 天、可切 30 天（`gateway-advanced.js`）。
- **影响**：网关高频使用时，30 天视图可能一次拉取数百万行（每行 ~20 字段/多个 String），占数百 MB 并长时间持有 `read_store`，与仪表盘取数互斥，表现为 UI 卡死。
- **方向**：用 SQL `SUM/COUNT ... GROUP BY channel_id/model` 返回聚合；`recent` 单独 `ORDER BY ts DESC LIMIT ?`；TTFT 用有界样本/直方图；补行数上限（与 90 天时间保留并行）。同时确认 `idx_gw_usage_day_ch` 是否被使用，避免纯写放大索引。

### P2-5 Gist 响应先完整下载再检查大小

- **证据**：`sync/gist.rs:96-102` `resp.bytes().await` 之后才比较 `MAX_RESPONSE_BYTES`；`create_gist` 的 `resp.json()` 也没有流式上限。
- **影响**：恶意/异常 gist 或 TLS 中间代理可让同步路径一次性分配数百 MB 甚至 GB，导致 OOM。共享 HTTP 客户端只有 20 s 超时，不限字节。
- **方向**：复用 `fetch_once.rs` / `responses.rs` 已有的流式 `read_body_capped`，在读取过程中按块累计并提前中止；错误信息保留“超过上限”语义。

### P2-6 自定义请求头明文入库，绕过密钥箱

- **证据**：`gateway.rs:487-488` 接受 `custom_headers`，`commit_settings` 把整个 `Settings` 以 JSON 写入 `settings` 表（`gateway/mod.rs` `CONFIG_KEY = "local_gateway_v1"`）；前端 `gateway-view.js:388` 提供“自定义请求头”文本框，无敏感头校验/提示。
- **影响**：用户若填入 `Authorization: Bearer sk-...`、`Cookie`、`x-api-key`，这些值会以明文写入 `tokenscope.db`，违背“密钥只进 Windows Credential Manager”的既有约束；备份/同步/诊断的边界也随之扩大。
- **方向**：对 `authorization`/`cookie`/`proxy-authorization`/`x-api-key` 等敏感名拒绝或要求放入 vault；至少加显式警告与脱敏；在 `Published::validate`/保存入口做统一校验，而不是只依赖 UI。

### P2-7 设置页渠道卡不可键盘展开（可访问性）

- **证据**：`app-channel-cards.js:268` 的卡头是 `<div class="cchead" data-act="chfold">`，没有 `tabindex`/`role`；`app-manage.js:63-73` 的 `keyActivate` 只对当前聚焦的 `[data-act]` 生效。主列表 `app-rows.js` 已正确使用 `tabindex="0" role="button"`。
- **影响**：键盘/辅助技术用户只能 Tab 到“眼睛”按钮（隐藏渠道），无法展开卡片，因而无法访问密钥、领取、测试连接、签到开关、排序、删除等全部每渠道设置。WCAG 2.1.1 失败；鼠标驱动的 jsdom/CDP 冒烟不会发现。
- **方向**：把头改成真正的 `button`（或 `role="button" tabindex="0"` + `aria-expanded`），`keyActivate` 支持 Enter/Space；补一个 jsdom 键盘用例。

### P2-8 推送到达即整块重绘，详情/渠道列表无合并

- **证据**：后端 150 ms 合并进度（`core/progress.rs`），但 `emit_channels` → `app-boot.js:34-43` 的 `applyChannelsPush` 每次调用 `render({updated:true})`；`app-view.js:78` 在详情弹层打开时重建 `body.innerHTML`，`:667` 在渠道页重建 `chList.innerHTML`，`app-channel-cards.js:173` 整表替换；只有主 `#list` 是 keyed。
- **影响**：慢取数时详情/渠道表可达约 6.7 次/秒重建，文本选择被清掉、滚动/焦点抖动、GC 压力上升；这是比 B6 更靠前的用户体验问题。
- **方向**：用 `requestAnimationFrame` 合并推送；每个表面计算数据签名，未变则跳过；详情/渠道卡改为 keyed diff（可复用 `qReconcile`）；渠道卡编辑中的输入/选区要保留。

### P2-9 四个同步 Tauri 命令在 UI 线程拿 SQLite 锁

- **证据**：`codex_reset_cmd.rs:55/66/100` 的 `codex_reset_profile_get/set/advice` 不是 `async`，直接 `lock_rw(&state.store)`；`key_ref_cmds.rs:129-134` 的 `vault_ref_map` 同样。90 天 `prune`/`wal_checkpoint` 会长时间持有同一把锁（`poll.rs`）。
- **影响**：打开 Codex 详情卡或密钥箱时，窗口可能在整个写锁期间冻结；这是项目历史上已出现过的 P1-14 类问题。
- **方向**：统一 `db(move |s| ...)` 异步 helper（`spawn_blocking`），把这四个命令改成 async；只把极轻量的配置读取留在主线程。

### P2-10 历史批量读取固定 92 天，500k 行悬崖

- **证据**：`fetch_view.rs:572-576` 的 `history_from()` 取 `min(prev0−1d, now−92d, now−90d)`；`store/mod.rs` `MAX_HISTORY_ROWS = 500_000`。若用户把 `active_interval_sec` 调到 15 s，92 天约 52 万行/渠道，超过上限后整块回退到 11 条旧查询（含已可用 SQL `GROUP BY` 的 `daily_coverage`）。
- **影响**：正常 60 s 下每次构建读 5–9 万行；高频配置下会触发 500k 行分配并整块回退，性能骤降、内存尖峰。
- **方向**：金额窗口只需要 `month0 − 1d`（约 31 天）；92 天仅用于 `gap_days`，改用 `daily_coverage` 的 SQL 聚合或每日 rollup。预计单次读取行数下降约 65%，并消除悬崖。

### P2-11 网关模型解析 O(全部模型) 且频繁 clone

- **证据**：`gateway/http.rs:325-359` 每次请求 clone `Settings`/`live_channels`，再用 `effective_models()` 生成全部模型字符串并做 `allowed.contains`；未命中还会重复扫描；`/healthz` 也 clone 整个 `Settings`。
- **影响**：聚合器配置（20 渠道 × 500 模型）下每请求数万次分配；未认证的 `/healthz` 可被局域网用作 CPU/锁竞争入口。
- **方向**：维护 `alias/model → (channel_id, model)` 的 O(1) 索引，按 `config_revision` 失效；渠道返回 `Arc<Published>`；`/healthz` 只读 `lan` 标志。

### P2-12 前端 `app*.js` 预算耗尽

- **证据**：`check-file-size.js` 的 `app*.js` 分组 **8774/8780**（按换行计），仅 6 行余量；`app-settings.js` 975、`app-rows.js` 842、`app-view.js` 799、`app-checkin.js` 770。
- **影响**：任何新功能都会触发规模门；为了过门而把代码挪到未计预算的 `gt-*.js`，会让“总量护栏”名存实亡。
- **方向**：把 `app-settings`/`app-rows`/`app-view` 中的独立面板拆成功能模块（`promo-page.js` 已有先例），同时给**全部**前端 JS 设总量/单文件预算（目前单文件 1500 行是唯一上限）；CSS 用 `@layer` 收敛层叠。

### P2-13 Release 先上传后验签，且自动校验未对钉住公钥验签

- **证据**：`.github/workflows/release.yml` 在打包后先创建/上传 Release，再运行 `verify-release.js`；`verify-release.js` 只比较线上 `.sig` 与本地 `.sig` 字符串，`verify-updater.js` 同样；真正的 `build/verify-signature.js`（对 `tauri.conf.json` 的 minisign 公钥验签）只出现在手工恢复演练中。
- **影响**：若签名私钥/公钥不匹配，流水线仍会显示“验签通过”并把版本推给用户，已安装客户端会静默拒绝更新；这是“发布成功但没人能更新”的失败类。
- **方向**：打包后、上传前运行 `verify-signature.js --local <installer> --sig <sig>`；Release 先建 draft，验签通过后再转正；`verify-release.js` 增加对公钥的独立验签。

### P2-14 依赖维护自动化失效，SQLite 版本偏旧

- **证据**：`.github/dependabot.yml` 的 cargo 目录指向 `/`，仓库根没有 `Cargo.toml`（实际清单在 `app/core` 与 `app/src-tauri`），Dependabot 不会产生升级 PR；CI 无 `cargo audit`/`cargo deny`。`rusqlite 0.32.1` 对应 SQLite 3.46.0（较早），而当前 rusqlite 已到 0.43 系列；依赖树还带 `reqwest 0.12.28 + 0.13.5`（native-tls/Schannel 与 rustls 两套）和 5 个 `windows-sys` 版本。
- **影响**：安全更新与 Tauri 插件升级不会被自动发现；二进制体积与 TLS 后端重复；SQLite 旧版本是纯技术债（当前 SQL 均参数化，未发现注入点）。
- **方向**：Dependabot 改为 `/app/core` 与 `/app/src-tauri` 两条；CI 增加每周 `cargo audit`（或 `cargo deny check advisories`）；把 rusqlite/winreg/if-addrs 升级与 reqwest 统一作为独立变更评估，Tauri 升级单独协调（会牵动 webview2-com/windows 版本）。

### P2-15 README 的安全披露不完整/不准确

- **证据**：README 写“出网只到渠道与 GitHub”，但实际还会访问 `open.er-api.com`/`api.frankfurter.dev`（汇率）与 `www.aipromonow.com`（优惠）；同时未披露 AGENTS §2.4 要求的高权限路径：以 `ELECTRON_RUN_AS_NODE=1` 启动 WorkBuddy 并读取内部存储、`taskkill /T /F`，以及 DPAPI 解封 Qoder Electron `safeStorage` 密钥。
- **影响**：知情同意与合规披露不完整；用户无法从 README 判断应用会启动/终止其它客户端或读取本机凭据。
- **方向**：补齐出网域名清单与高权限能力说明、默认开关/关闭方式；与 `README`、`DESIGN`、`AGENTS` §2.4 同步维护。

### P2-16 “安装更新”路径没有自动化测试

- **证据**：CI 对 `src-tauri` 只跑 `cargo check --all-targets`，Release 只跑 core 测试；`src-tauri` 的 8 个纯逻辑测试（下载 URL 白名单、vault URL、托盘 digest、LAN IP 准入）本地因 WebView2 启动失败无法执行；`app-update.js` 没有 jsdom 覆盖，`build/` 中没有 `check_update`/`install_update` 的专项脚本。
- **影响**：最危险的“下载并安装更新”路径只靠人工；一旦更新 UI/状态机回归，CI 不会红。
- **方向**：把纯逻辑测试移到 `tokenscope-core` 让 CI 跑；为 `app-update.js` 增加 jsdom 测试（无 info 不安装、忙时禁止重复、跳过版本持久化失败提示、安装失败可重试）。

### P3 级发现（择要）

| 编号 | 发现 | 证据/影响 | 建议 |
|---|---|---|---|
| T-1 | `redact()` 在每个字节位置复制后缀，O(n²) | `diaglog/redact.rs:151-190`；错误体先脱敏后截断（`responses.rs` 只保留 300–400 字），大错误页浪费 CPU/分配 | 按字节前缀比较；先截断到 ~16 KB 再脱敏 |
| T-2 | 异步路径上的阻塞 I/O | 冷 token 读 Credential Manager 持 `access_token_sha256`；`ProxyClients::client` 每 5 s 读环境/注册表并可能建 TLS 客户端；`gateway_status` 在 async 命令里 `vault_list`/`commit_settings` | 缓存/原子化；注册表与客户端重建、SQLite 提交移入 `spawn_blocking` |
| T-3 | 静默写失败 | `sync/publish.rs:222` 忽略 watermark 保存；`fetch.rs:447-449` 忽略删除旧 quota/scope 键；台账队列满仅诊断后丢弃；`fx.rs` 无负缓存/退避，断网时每轮可能多花 2×6 s | 按 AGENTS §2.1.6 改为可见失败或明确降级；补水位/删除/队列测试 |
| T-4 | `live_channels()` 在 vault 索引错误时返回空集合 | `gateway/mod.rs` 读取失败被当成“没有渠道”，外部客户端看到空模型列表 | 返回错误/保留上次快照，并写诊断 |
| T-5 | 自定义渠道 `color`/`unit` 未转义就进 `innerHTML`/`style` | `app-core.js:579-581`、`app-rows.js:86`、`app-view.js:29-30`；`custom.rs` 校验 URL/路径但不校验这两个字段 | 输出边界统一 `esc()`/CSS 值白名单校验；CSP 已阻止脚本，故为 UI 欺骗/布局破坏 |
| T-6 | `#fstat` 隐藏但仍每轮计算，jsdom 还断言其内容 | `index.html:524` 带 `hidden`；`app-view.js:559-563/624-635` 仍更新；`jsdom-smoke.js` 断言隐藏节点 | 删除死节点/死计算与对应断言，或恢复可见聚合状态 |
| T-7 | CSS 层叠债务 | `styles-tokens.css` 整组调色板被重复定义；`@keyframes rowflash` 在 panel/manage 各定义一次，后者覆盖前者；`.btn.p` 重复；16 个 `!important`、0 个 `@layer` | 用 `@layer tokens, base, panel, manage`；删除死调色板/重复规则；逐步替换 `!important` |
| T-8 | `GT_STORE` 迁移只完成一半 | `gt-store.js` 的 `subscribe/getState/setState` 没有外部调用者；`setTabDirty` 返回“是否变化”而非“之前是否脏”；`index` 脏标记永远不被消费 | 明确保留为状态容器或完成订阅/批处理；修正 dirty 语义 |
| T-9 | `final_text` 用字节偏移切最终文本 | `gateway_proto/responses/stream.rs:227-230`：最终文本变短/改写时 `get(sent..)` 返回 `None`，尾部静默丢失 | 校验前缀或按字符边界比较；补“最终文本缩短”的协议测试 |
| T-10 | 生产 `unwrap/expect` 与 `unsafe` 分布 | 约 22 unwrap + 9 expect（core 生产段，启发式排除测试文件/首个 `#[cfg(test)]` 之后）、1+3（桌面）；0 `panic!` 生产；59 处 `unsafe` 集中在 Antigravity/ConPTY、GDI 托盘、DPAPI | 把 `powershell.parent().unwrap()`、`console.as_ref().unwrap()` 等改为带上下文的 `ok_or`；`unsafe` 已有 RAII，可补安全注释/审计清单 |
| T-11 | 依赖重复 | `reqwest 0.12.28 + 0.13.5`、`windows-sys 0.48/0.52/0.59/0.60/0.61`、`base64 0.22/0.23`、`brotli 8/9` 等；Release 11.33 MB | 评估把 core 升到 reqwest 0.13 统一 TLS/HTTP 栈；其余多为 Tauri 传递依赖，升级窗口期处理 |
| T-12 | 托盘 GDI 两个 P3 观察 | `TextOutW` 失败返回全零掩码而非点阵回退；16 px 四字符偏紧 | 失败返回 `None` 落到点阵；16 px 再收紧字号搜索 |
| T-13 | 设置面板常驻 DOM | 首开设置后进程树 445→522 MB，30 s 不回落；DOM 从 3.2k 增至 10k | 关闭设置时卸载非当前面板，或对 `.panel` 用 `content-visibility:auto`/`contain: layout paint`；复测常驻内存 |
| T-14 | IPC 契约只校验命令名 | `check-ipc-contract.js` 只提取 `invoke("…")` 字面量；104 个内联 `GT_API` 调用当前与 Rust 签名全部匹配，但参数名/结构无门禁 | 从 Rust 签名生成/校验 JSON schema；jsdom mock 对未知参数抛错 |
| T-15 | CI 的 `node --check` 列表漏文件；`check-yaml.js` 从未运行 | `ci.yml` 手写列表漏 `gt-api.js`/`gt-store.js`/`gt-window-state.js`/`gateway-ui-state.js`/`promo-page.js`；`build/check-yaml.js` 无任何 workflow 引用 | 改成 `app/src/*.js` glob；把 yaml 门禁接进 CI 或删除 |
| T-16 | MSRV 元数据与依赖不符 | `app/src-tauri/Cargo.toml` 写 `rust-version = "1.77"`，但 `tauri-utils 2.10.2` 要求 1.90；本机/CI 实际用 1.98 | 改成真实 MSRV 并加 `rust-toolchain.toml` 固定；否则下游用 1.77 构建会失败 |
| T-17 | 仓库卫生与文档腐烂 | 9 个 `build/bundle-log-*.txt` 与 22 个 `build/qa/**` 仍被跟踪；`app/README.md` 仍指 `store.rs`/`inject.rs` 等旧路径、18 模块/43 命令；README 的 21/5 文档数与 8.5 MB exe 已过时；`acceptance-backlog.md` 有重复 B20；`release.yml` 写死“243 条”测试 | 清除受跟踪产物；文档改为链接/以命令输出为准；去重 B20；测试数不写死 |
| T-18 | 截断的加密备份会被当明文旧格式 | `backup_crypto.rs` 对 ≤40 字节的 `MAGIC` 前缀文件按 legacy plaintext 处理 | 以 `MAGIC` 开头但长度不足时直接报“备份损坏”，不降级 |
| T-19 | `clip`/`taskkill` 以裸名调用 | `os_cmds.rs`、`workbuddy/mod.rs` 通过 PATH 查找；本机若存在同名恶意可执行文件可被劫持 | 用 `%SystemRoot%\System32\clip.exe`/`taskkill.exe` 绝对路径，或校验路径 |

## 4. 优化路线图

| 优先级 | 项目 | 预期收益 | 工作量 | 风险/前置 |
|---|---|---|---|---|
| **P0（立即）** | **清除公开仓库 QA 截图 + 历史/源码归档清理 + CI 产物门禁** | 停止 PII 继续公开；新克隆不再携带 | M（含强推/GitHub Support） | 需用户明确确认；0 fork 使重写可控；旧 Release 源码包需删除/重建 |
| 立即（1–2 天） | 快照去重 + 15–30 min 心跳；一次性 compact | 行数 −90%，DB 92→20–40 MB，写入/WAL −90% | S | 需差分回归消费/到账/缺口/趋势；先做开关 |
| 立即 | 休眠感知的取数/签到门 + `None` 会话策略 | 消除假 `no_cred` 与退避；让省内存模式对常见配置真正可用 | S | 需真实 300 s 测试与唤醒补轮 |
| 立即 | 修复 `attach_plan_costs` 只读写入 | 恢复套餐周期学习；去掉每轮失败重试 | XS | 改成 writer 或移到 writer 阶段 |
| 立即 | 台账 SQL 聚合 + recent LIMIT | 30 天台账从潜在秒级/数百 MB 降到几十 ms | S–M | 需 ≥1 万行夹具与查询计划断言 |
| 立即 | Gist 流式限长 | 关闭唯一无界响应分配 | S | 复用现有 capped reader |
| 立即 | 自定义请求头密钥策略 | 关闭明文密钥入库的口子 | S | 需产品确认拒绝名单/迁移已有值 |
| 下一批（3–5 天） | 渲染调度器 + 详情/渠道卡 keyed diff | 推送重绘从 ~6.7/s 降到 ≤1 帧；保留选区 | M | 需 DOM/焦点/IME 回归 |
| 下一批 | 设置卡键盘化 + `aria-expanded` | 可访问性达标；每渠道设置可达 | S | jsdom 键盘用例 |
| 下一批 | 四个同步命令改 async/spawn_blocking | 消除最后一类 UI 线程 SQLite 冻结 | S | 复用 config_cmds 范式 |
| 下一批 | 历史读取按用途拆分 | 行数 −65%，消除 500k 悬崖 | S–M | 需 15 s 间隔夹具与缺口对照 |
| 下一批 | 模型索引 + `Arc<Published>` | 大配置请求分配下降一个量级 | M | 需缓存失效/路由组回归 |
| 下一批 | 设置面板卸载/`contain` | 首开设置后常驻 −50–80 MB | S–M | 需滚动/焦点/重开体验回归 |
| 后续（1–2 周） | 前端模块拆分 + CSS `@layer` + 全量预算 | 解除 8774/8780 预算锁死；层叠可预测 | L | 需逐页视觉/几何回归 |
| 后续 | 类型化 IPC（schema）+ 门禁 | 111 命令的参数漂移在 CI 可见 | M | 需生成器/迁移期双轨 |
| 后续 | WebView2 内存实验 | 可能再降 50–100 MB（需实测） | M | 需 A/B、稳定性/兼容性验证 |
| 后续 | 依赖统一（reqwest/windows-sys） | 二进制/攻击面下降 | M | 受 Tauri 传递依赖约束 |

**建议的验收方式**（对应 AGENTS §2.1）：每个优化都先证明“坏样本会变红”。例如快照去重需有“值不变但时间推进”“值变化但 remaining 相同”“跨午夜/夏令时”“积分到账”四类夹具；休眠门需有“释放期间不写失败、唤回补轮”的集成测试；台账聚合需有 ≥1 万行、30 天窗口的查询计划/耗时断言；Gist 需有超大响应的回环服务器。

## 5. 测试与工程改进

- **B11 债务**：`verify-ui-0923.js`/`verify-form.js` 仍命中 5 类过时断言（U2 文案、U6 官方标数/紧凑条、compact 尺寸、`win-style.ps1` 标题）。建议按现行设计重写断言并加坏样本，不再让原生回归长期“6/8”。
- **10 个 ignored 测试**：Antigravity 真实 `agy`、Qoder 登录、Windows Credential Store 交互、checkin/Trae/DSH/WorkBuddy 的手动用例、`bench_history_batch`。建议至少把可离线化的用环境变量/夹具自动化，其余在 CI 外保留明确“需真机”标签。
- **src-tauri 本地不可跑**：`cargo test` 启动即 WebView2 `STATUS_ENTRYPOINT_NOT_FOUND`（AGENTS §6.7 已知）。建议把可纯逻辑的窗口/暂停/同步筛选取到 core，或在 CI 的 Windows runner 跑 `cargo test`（不是只 `cargo check`）。
- **IPC 参数契约**：当前 111/111 名称匹配，但参数形状无门禁；建议从 Rust `#[tauri::command]` 签名生成 JSON schema，扩展 `check-ipc-contract.js`，jsdom mock 对未知参数抛错。
- **可访问性**：补键盘展开设置卡、Tab 顺序、`aria-expanded`/`aria-controls`、对比度（尤其浅色主题）与 16 px 托盘文本的实机用例。
- **休眠状态**：在 `run-native-checks.ps1` 增加“释放期间渠道/签到状态”脚本，而不只量内存和几何。
- **公开产物门禁**：增加 `git ls-files build/qa` 为空的检查，禁止跟踪 QA 截图/二进制；Release 前对安装包执行 `verify-signature.js`（钉住公钥）并先 draft 后转正。
- **更新路径**：为 `app-update.js` 增加 jsdom 测试，并把 `src-tauri` 的 8 个纯逻辑测试迁到 core 让 CI 真正执行。
- **性能预算**：把前端总行数、DOM 节点数、WebView2 常驻内存纳入门禁（例如首开设置后 ≤ +50 MB），避免“先开设置就多 77 MB”这类回归无人发现。

## 6. 结论

`v0.3.78` 的工程质量高于同类桌面挂件：命令面清晰、密钥边界明确、窗口/托盘/休眠线程模型经过实战修正、主列表与历史查询有可观的性能投入。但本次审查发现 **1 个必须先处理的 P0：公开仓库中的 QA 截图携带真实手机号/账户信息，且从 v0.3.30 起进入所有标签与 Release 源码归档**；在完成历史清理与缓存清理前，不应继续新增公开产物。此后最值得投入的是**把数据写入从“每轮一行”改成“变化 + 心跳”**、**把休眠与网页会话/签到状态解耦**、以及**修复只读连接上的周期学习写入**；这三项就能同时改善磁盘、内存、正确性与长期可维护性。前端则优先做渲染合并与设置卡键盘化，并在拆分模块的同时把预算扩展到全部前端 JS。

> 本报告为只读审查与实机测试结论；测试期间对 `%APPDATA%\com.wacil.tokenscope` 的 `*.db*`/`*.json` 已按 SHA-256 备份并还原，最终三文件均 `MATCH`，无测试进程残留。

> 审查期间工作区另有一份未提交的 `app/core/src/tray_icon.rs` 字体权重迭代（`FW_NORMAL`/`FW_SEMIBOLD`、22 px 分界、新增多位数字 anti-clumping 测试），其 `tray_icon` 测试 5/5 通过；该改动不属于 `v0.3.78`，未纳入本报告结论，建议单独复核后再发版。


## 7. 复查与本地修复（2026-11-26）

§1–6 是 v0.3.78 的审查快照。本节记录当前工作区的复查结果。改动按数据读写与会话、网关、前端渲染、发布和仓库门禁展开，沿用现有入口与夹具。按用户裁决保留每次快照观测的时间，也保留现有历史数据。清理范围为本地工作区与 Git 索引。

### 7.1 P0 与 P1

| 编号 | 核对与处理 |
| --- | --- |
| P0-1 | 从索引移除 22 个 QA 文件和 9 个打包日志，31 份工作区文件均仍存在。产物门禁接入 CI、仅文档也会运行的编码流程和发版流程。实际索引移除前返回失败，移除后通过。 |
| P1-1 | 确认写入放大。消费边界、到账判据依赖逐次观测，按用户裁决保留采样精度。此次优化历史查询和台账聚合，详见 P2-4、P2-10。 |
| P1-2 | 确认释放主 WebView 后的误判路径。签到使用 WorkBuddy 自己的会话入口，豆包的自动签到已下线。WorkBuddy 和豆包缺少可用凭据时保留上一轮视图与重试状态，WorkBuddy 签到活动和流水跳过暂不可读的网页会话。桌面凭据仍可用，已确认存在的 Cookie 会话仍保留窗口，唤回后补取数与签到判断。离线夹具覆盖释放和恢复时的重试选择。 |

### 7.2 P2

| 编号 | 核对与处理 |
| --- | --- |
| P2-3 | 周期记忆改由写连接保存。失败写排查日志并在渠道详情提示。回归以真实只读连接读取写入结果。 |
| P2-4 | SQLite 汇总渠道、模型和首字耗时分布。明细独立按时间和行 ID 倒序执行 LIMIT，仍覆盖整个查询窗口的汇总。迁移清理无查询使用的台账日索引，保留时间索引和明细。万行夹具核对汇总、分位数、最新渠道别名及明细查询计划。 |
| P2-5 | Gist 读取和创建共享分块限长，只在限内解析 JSON。回环夹具覆盖分块响应和普通响应。 |
| P2-6 | 网关保存边界拒收常见认证头。新版启动迁移移除旧设置中的对应明文头，并提示改用密钥箱。现有数据库的历史备份或页面残留与此迁移分别处理。 |
| P2-7 | 渠道卡增加独立原生展开按钮，关联内容区域并同步展开状态。显隐按钮独立接收焦点。 |
| P2-8 | 可见页面同帧推送合并，详情和渠道卡复用变化前的节点，保留焦点与编辑状态。隐藏页面仍更新胶囊所用数据。 |
| P2-9 | 重置顾问的读写及密钥箱引用查询由后台任务执行。读取使用只读连接，账号范围校验沿用现有签名。 |
| P2-10 | 金额明细从上月同期所需基线开始读取。逐日覆盖和申请跳升分别在 SQLite 聚合，避免为两项读取并分配 90 多天逐行记录。保留逐日和逐窗口的既有统计口径。 |
| P2-11 | 鉴权和健康检查只读所需开关。直接模型请求仅检查别名前缀匹配渠道的模型，避免克隆全部渠道或反复构造模型列表。重复模型和代理占位模型仍经过原有路由判定。 |
| P2-12 | 优惠 UI 归入已有优惠模块，同时增加覆盖全部前端 JS 的总量预算，原分组上限随拆分下调。 |
| P2-13 | 上传前使用 Tauri 配置公钥和锁定版本的验签库核验安装包。新 Release 为草稿，重新下载并逐件确认与本地已核验资产一致后转正。正样本、篡改文件和错公钥自检已执行。 |
| P2-14 | Dependabot 的 Cargo 目录改为两个实际清单目录。SQLite 版本和传递依赖重复属于依赖维护事项，本次修复升级检测入口。 |
| P2-15 | README 补充汇率、优惠出网，以及 WorkBuddy 进程、Qoder DPAPI 凭据和按渠道关闭自动访问的条件。 |
| P2-16 | jsdom 覆盖缺版本、安装防重入、跳过落库失败和重试。桌面发布链接、密钥箱、托盘指纹和 LAN 地址的纯逻辑用例迁入 core 运行。 |

### 7.3 P3

| 编号 | 核对与处理 |
| --- | --- |
| T-1 | 脱敏改为逐前缀字节比较，键名扫描限制候选长度。仍在截断前脱敏。 |
| T-2 | 冷凭据读取与代理变更检测进入后台阻塞池，缓存命中走快路径。网关状态查询与存储更新也移到后台。 |
| T-3 | 同步状态和水位同事务提交，旧账号缓存键原子清理并上报失败，队列丢行计数显示在台账，汇率失败按月限频重试。 |
| T-4 | 密钥箱索引错误走网关错误响应和诊断，调用者可区分故障与空模型列表。 |
| T-5 | 颜色写入 CSS 前限制为十六进制，货币单位前缀过滤并在模板边界转义。 |
| T-6 | 紧凑条实际消费空态文案。去掉隐藏节点及不可见的计数标记，紧凑条从当前渠道状态取空态文案，底栏保留可见状态点。 |
| T-7 | 移除被后置样式覆盖的调色板、按钮样式和同名动画。CSS 分层重排属于后续结构调整。 |
| T-8 | 校正脏页返回语义，去掉未消费的索引脏标记和未调用的订阅接口。 |
| T-9 | 流式末帧核对已发正文的 UTF-8 边界与前缀。缩短或改写会返回协议错误，回归覆盖三个分支。 |
| T-10 | 两个可确认的 Antigravity 路径断言改为可见错误或直接持有已建立的句柄。全仓 unsafe 数量是审计观察项。 |
| T-11 | 传递依赖重复属维护事项，交由实际清单目录的依赖更新流程评估。 |
| T-12 | GDI 无法生成字形时进入点阵回退。工作区已有的字体权重迭代继续保留，小尺寸视觉仍需实机评估。 |
| T-13 | 关闭设置时回收动态渠道卡 DOM，并保留输入草稿。jsdom 验证节点已释放，WebView2 进程内存待重新量测。 |
| T-14 | 冒烟桩从 Rust 命令签名校验顶层必填及未知参数，内嵌对象仍使用 Rust 类型反序列化校验。 |
| T-15 | CI 与发版采用前端脚本通配检查，移除无运行引用的旧 YAML 子集脚本。 |
| T-16 | 桌面 crate 声明的 Rust 最低版本与锁定依赖的最高最低版本要求对齐。GNU 编译以本机 stable 工具链核对。 |
| T-17 | 产物移出索引，更新模块路径与文档说明，去除重复 B20 及发版流程固定测试数。 |
| T-18 | 魔数匹配但容器过短的备份会明确报损坏。恢复与旧备份迁移均拒收该输入。 |
| T-19 | 剪贴板和 WorkBuddy 超时清理调用系统目录中的指定程序。 |

### 7.4 远端遗留

原报告记录的 v0.3.30–v0.3.78 标签和 GitHub 生成的源码 ZIP/TAR 仍属于历史暴露范围。本地抽查区间首尾标签仍包含截图。公开分支旧提交、GitHub 缓存、已下载归档及历史克隆可能继续保留副本。此次处理落在本地索引，旧标签、Release 资产和 GitHub 缓存待另行处置。

### 7.5 验证

- AGENTS §4.1 静态门与对应自检、前端冒烟和专项、core 单测及同步夹具、core 与桌面全目标检查、凭据扫描均通过。桌面检查无编译警告，实际用例数以各命令输出为准。
- 关闭设置释放渠道节点及更新交互已进入 jsdom。冒烟曾因优惠模块初始化顺序和隐藏窗口哨兵标记冲突中断，修正接线后复跑通过。
- 刷新稳定性补测中，默认 Edge 返回空 DOM，共用 profile 的 Chrome 探针挂起。改用独立临时 profile 与超时后，无头 Chrome 正样本通过，`ROW_SIG_SENTINEL` 在节点替换断言判红。
- Workflow YAML、原生脚本语法、原生运行器自检、签名自检均通过。本地现存 v0.3.37 安装包也通过配置公钥验签。草稿资产核验和 Rust 多清单配置分别按官方命令和配置项实现。 [GitHub CLI](https://cli.github.com/manual/gh_release_download) · [Dependabot 配置](https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-options-reference#directories-or-directory)

B11 脚本已改用现行失联文案、按有官方标的实际样本推导数量，并在测试期间保持紧凑条开关有效。Win32 窗口样式按调试实例进程定位。已运行的桌面实例占用真机跑测前置条件，300 秒省内存生命周期、CDP 实机结果和关闭设置后的工作集仍待确认。GitHub Actions 草稿转正链路留待下一次授权发版验证。
