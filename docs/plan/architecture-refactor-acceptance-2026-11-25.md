# Grandettoken 架构重构全量验收执行说明

> **日期**：2026-11-25  
> **验收范围**：基线 commit `fc1c19d`（`v0.3.76`）对比当前工作区已落地的全部架构重构改动（任务卡 `R1-1` 至 `R4-2`，共 4 个阶段 12 张卡）  
> **参考依据**：[`docs/plan/architecture-refactor-plan-2026-11-25.md`](./architecture-refactor-plan-2026-11-25.md)、[`AGENTS.md`](../../AGENTS.md)、[`DESIGN.md`](../../DESIGN.md)、[`PROGRESS.md`](../../PROGRESS.md)  
> **执行角色**：独立第三方验收 Agent（只读核验、运行门禁与真机验收；**发现缺陷只记录定级与复现证据，不擅自修改产品源码**）

---

## 0. 验收原则与角色边界

1. **独立只读，不修产品代码**：本说明面向验收执行 Agent。若门禁、单测、哨兵或真机发现回归/未达标项，只写入验收报告并附完整复现命令与日志路径，不得顺手改产品代码掩盖问题。
2. **禁止拿历史记录顶替本次实测**：虽然开发阶段在 `PROGRESS.md` 记录过各卡自测与部分原生复核，但本轮验收必须在当前最终工作区状态下**完整重跑并留存本次实测数字**。
3. **只报实测事实**：任何未跑项（例如缺少真实账号或特定 DPI 硬件环境）必须明确标为 `SKIP / 未跑` 并说明原因，严禁推算或填写未观察到的数字。

---

## 一、验收前置与安全红线

1. **生产进程保护**：
   - 开始前先检查本机是否有运行中的 `Grandettoken.exe` / `tokenscope` 实例；
   - **禁止擅自强杀用户的生产实例**。若需执行原生 WebView2 / 真机测试，必须先提示用户自行关闭运行中实例，或明确征得用户同意后再启停。
2. **用户数据 SHA-256 备份与逐字节还原**：
   - 任何启动桌面程序或读取 `%APPDATA%\com.wacil.tokenscope\` 的验收前，必须先记录原有 `*.db*` 与 `*.json` 的 SHA-256 并备份到系统临时目录（可直接使用 [`build/run-native-checks.ps1`](../../build/run-native-checks.ps1) 的备份机制或隔离 `--user-data-dir`）；
   - 测试结束后清理测试产生的新增文件、按 SHA-256 逐字节还原原始用户数据并核对哈希 `MATCH`；真实数据库的 v8→v9 迁移验证必须在 `%TEMP%` 隔离副本上只读/演练，**严禁直接修改用户原库**。
3. **临时产物与日志归位**：
   - 本次验收的原始日志、截图、临时脚本均放在 `build/qa/refactor-acceptance-2026-11-25/`（已被 `.gitignore`）或系统 `%TEMP%` 下，**严禁提交或残留在受控源码目录**。
4. **环境与命令纪律**：
   - 全部门禁与测试**串行执行**（严禁并行跑 `cargo test`、`cargo check` 与 `jsdom`，避免资源竞争假红）；
   - 运行 Rust 测试与前端本地回环验证时，验证进程显式设置 `NO_PROXY=127.0.0.1,localhost,::1`；
   - PowerShell 5.1 读写文件一律显式带 `-Encoding utf8`。

---

## 二、阶段 A：全量自动化门禁与编译基线复核（一票否决）

> 本阶段任一失败（不含已知且有证据对照的 B11 过时原生脚本项），整体验收直接判 `FAIL` 或阻塞放行。

在仓库根目录使用 Git Bash 串行执行以下命令并逐项记录原始输出与退出码：

### A1. 零依赖静态门禁（含 `--self-test` 双向证红）

```bash
node build/check-ipc-contract.js
node build/check-silent-failures.js --self-test && node build/check-silent-failures.js
node build/check-file-size.js --self-test && node build/check-file-size.js
node build/check-css-forms.js --self-test && node build/check-css-forms.js
node build/check-encoding.js --self-test && node build/check-encoding.js
node build/check-app-split.js --self-test && node build/check-app-split.js
node build/check-lock-unwrap.js --self-test && node build/check-lock-unwrap.js
node build/verify-expiry.js
node build/verify-qoder-expiry.js --self-test
node build/verify-quota-plan.js
for f in app/src/*.js app/src-tauri/src/google_cloud_capture.js; do node --check "$f" || exit 1; done
```

**额外核对项**：
- `check-ipc-contract.js` 报告的前端 `invoke` 唯一命令数与 Rust 定义/注册数必须 100% 匹配（注意 `R4-1` 移除旧托盘绘图前端推图后，对比基线 `fc1c19d` 确认命令增减完全符合预期，无遗漏或孤儿注册）。
- `check-file-size.js` 的 `BUDGETS` 表中所有 Rust 超标文件已清零（仅剩 `app/src/index.html` 与 `GROUP_BUDGETS` 下 `app/src/app*.js <= 8780`），且新增拆分文件全部 `<= 1500` 行。
- `for f in app/src/*.js` 必须实际覆盖新增的 `gt-api.js`、`gt-store.js`、`gt-window-state.js`。

### A2. 前端 `jsdom` 冒烟、专项验证与 9 项哨兵证红

```bash
NO_PROXY=127.0.0.1,localhost,::1 node build/jsdom-smoke.js
NO_PROXY=127.0.0.1,localhost,::1 node build/verify-vault.js
NO_PROXY=127.0.0.1,localhost,::1 node build/verify-codex-reset.js
NO_PROXY=127.0.0.1,localhost,::1 node build/verify-google-cloud.js
NO_PROXY=127.0.0.1,localhost,::1 node build/verify-antigravity-capture.js
NO_PROXY=127.0.0.1,localhost,::1 node build/verify-gateway.js
NO_PROXY=127.0.0.1,localhost,::1 node build/verify-gateway-magpie.js
```

同时**逐个单独运行** [`build/jsdom-smoke.js`](../../build/jsdom-smoke.js) 内置的全部反向哨兵，核对每项均以非零码退出且命中对应功能的 `FAIL` 行（而非类型报错遮盖）：
- `CHECKIN_SENTINEL=1`、`AG_LAYOUT_SENTINEL=1`、`PERFORMANCE_SENTINEL=1`、`TAB_DIRTY_SENTINEL=1`、`ANTIGRAVITY_ACCOUNT_SENTINEL=1`、`WORK_AREA_SENTINEL=1`、`EXPIRY_UI_SENTINEL=1`、`FIT_PILL_SENTINEL=1`、`GOOGLE_CLOUD_CALENDAR_SENTINEL=1`。

### A3. Rust 全量单测、集成夹具与离线编译校对

```bash
(cd app/core && NO_PROXY=127.0.0.1,localhost,::1 cargo test --locked -- --test-threads=1)
(cd app/core && NO_PROXY=127.0.0.1,localhost,::1 cargo test --locked --test sync_fixtures -- --test-threads=1)
(cd app/core && cargo check --all-targets --locked)
(cd app/src-tauri && cargo check --all-targets --locked)
cargo fetch --locked --manifest-path app/src-tauri/Cargo.toml
```

> 若无头/沙箱下 `cargo check` 报警告，必须判定为退化并记录（要求 0 warning）。

### A4. 凭据安全扫描与路径规范检查

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File build/scan-secrets.ps1
```

同时检查当前工作区新增/修改的受版本控制文件（`app/` 与 `docs/plan/`）不含本机绝对路径或明文密钥残片。

---

## 三、阶段 B：逐卡源码与专项行为核验清单（`R1-1` ~ `R4-2`）

验收 Agent 须逐卡核对实际代码实现与单元测试，确认没有「名义完成但暗留旁路或未接全」的情况。

### Phase 1：性能与资源减负

| 卡号 | 必查文件与符号 | 逐卡核验要点 | 重点风险 / 反向验证 |
|---|---|---|---|
| `R1-1` | [`app/core/src/store/schema.rs`](../../app/core/src/store/schema.rs)、[`snapshots.rs`](../../app/core/src/store/snapshots.rs)、[`tests.rs`](../../app/core/src/store/tests.rs) | 1. `WANT >= 9`；建 `idx_snap_fp_cover` 后执行 `DROP INDEX IF EXISTS idx_snap_fp`；<br>2. 保留 `idx_snap_lookup`；<br>3. 未引入自动 `VACUUM`。 | 运行 `store::tests` 中迁移与 `EXPLAIN QUERY PLAN` 用例，确认覆盖扫描命中 `idx_snap_fp_cover` 且无全表 `SCAN snapshots`。 |
| `R1-2` | [`app/core/Cargo.toml`](../../app/core/Cargo.toml)、[`app/core/src/inject/process.rs`](../../app/core/src/inject/process.rs)、[`sites.rs`](../../app/core/src/inject/sites.rs)、[`drift.rs`](../../app/core/src/inject/drift.rs) | 1. 启用 `Win32_System_Diagnostics_ToolHelp`；<br>2. `CreateToolhelp32Snapshot` 配 RAII `CloseHandle`；<br>3. `sites()`/`state()`/`drift` 单次抓拍共享快照；<br>4. 无残留 `tasklist` 生产调用；<br>5. 保留 `opencode-cli.exe` / `opencode-pty.exe` / Trae 全名命中。 | **开发遗留实测焦点**：`PROGRESS.md` 记本机 5 次快照实测 **13.9–15.5 ms**（末次 14.4 ms），未达计划 `< 5 ms` 预期。验收须在本机独立实测 **Win32 快照 10 次耗时（P50/P95/Max） vs 旧 `tasklist` 8 次串行基准**，给出量化对比与是否可接受的定性结论。 |
| `R1-3` | [`app/core/src/gateway/access.rs`](../../app/core/src/gateway/access.rs)、[`projection.rs`](../../app/core/src/gateway/projection.rs)、[`ledger.rs`](../../app/core/src/gateway/ledger.rs)、[`mod.rs`](../../app/core/src/gateway/mod.rs)、[`models.rs`](../../app/core/src/models.rs)、[`proxy.rs`](../../app/core/src/proxy.rs) | 1. 热路径鉴权缓存 SHA-256 摘要且密钥轮换必失效；<br>2. Vault 内存投影随 `vault_revision` 同步更新，不漏删改失效点；<br>3. 4SAPI 价目表 `Arc<PricingLookup>` 缓存随 `ModelCache::save` 失效；<br>4. `ProxySnapshot` 5 秒 TTL 节流；<br>5. 有界 `mpsc(256)` 异步台账队列支持 `flush`/`clear` 及无 tokio runtime 同步回退。 | 确认 [`magpie_tests.rs`](../../app/core/src/gateway/http/magpie_tests.rs) 与 [`gateway/tests.rs`](../../app/core/src/gateway/tests.rs) 中的计数断言与失效反向测试全部存在且通过；确认 `proxy_changes_refresh_the_client_and_direct_groups_bypass_it` 通过。 |
| `R1-4` | [`app/core/src/http_pool.rs`](../../app/core/src/http_pool.rs)、[`minimax.rs`](../../app/core/src/minimax.rs)、[`qoder.rs`](../../app/core/src/qoder.rs)、[`workbuddy/mod.rs`](../../app/core/src/workbuddy/mod.rs)、[`sync/gist.rs`](../../app/core/src/sync/gist.rs) | 1. 共享 `default` / `direct` HTTP 池；<br>2. MiniMax/WorkBuddy 阻塞续期复用共享运行时；<br>3. `GistClient` 复用连接池且按次传 `Authorization`。 | 全仓 grep `reqwest::Client::builder`，确认生产源码中不再有业务函数内部现建客户端的退化点。 |

### Phase 2：并发隔离与前端按需渲染

| 卡号 | 必查文件与符号 | 逐卡核验要点 | 重点风险 / 反向验证 |
|---|---|---|---|
| `R2-1` | [`app/core/src/store/mod.rs`](../../app/core/src/store/mod.rs)、[`app/core/src/fetch_db.rs`](../../app/core/src/fetch_db.rs)、[`app/src-tauri/src/poll.rs`](../../app/src-tauri/src/poll.rs)、[`gateway_ext.rs`](../../app/src-tauri/src/gateway_ext.rs)、[`lib.rs`](../../app/src-tauri/src/lib.rs) | 1. `Store::open_readonly` 使用 `SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only = ON`；<br>2. 重型读（`history_between`、`build_view`、`gateway_usage_summary`、`channel_changes`、`checkin_history`）走只读连接 + `spawn_blocking`；<br>3. 每日 `prune` + `wal_checkpoint` 下沉 `spawn_blocking`；<br>4. 严守先写锁后 config 锁序，读写锁不嵌套。 | 核验并发写同时并发读单测通过，且向只读连接执行写语句必然返回 `Err`。 |
| `R2-2` | [`app/src/app-view.js`](../../app/src/app-view.js)、[`app-channel-cards.js`](../../app/src/app-channel-cards.js)、[`app-core.js`](../../app/src/app-core.js)、[`app-manage.js`](../../app/src/app-manage.js)、[`app-settings.js`](../../app/src/app-settings.js) | 1. 停留在非 `ch`/`disp` 设置子页时，`renderInner` 不重建 `#chList` 与 `#cfgPill` 而只标脏 `DIRTY_TABS`；<br>2. 切回对应子页立即消费 `DIRTY_TABS` 补绘；<br>3. 非紧凑条态 `fitCompact` O(1) 早退；<br>4. `tickPeakCountdowns` 仅扫 `#list` 与打开时的 `#detailBody`。 | 运行 `TAB_DIRTY_SENTINEL=1 node build/jsdom-smoke.js` 必须判红；并跑 `verify-refresh-stability.js --assert` 验证 Chrome/Edge 无头下节点复用与 `childListCount === 0`。 |

### Phase 3：可维护性深度重构

| 卡号 | 必查文件与符号 | 逐卡核验要点 | 重点风险 / 反向验证 |
|---|---|---|---|
| `R3-1` | `app/core/src/{store,inject,providers,checkin,diaglog,workbuddy}/`、[`app/core/src/inject/adapter.rs`](../../app/core/src/inject/adapter.rs) | 1. 对外 `pub use` 路径保持兼容；<br>2. `inject/adapter.rs` 统一客户端接入与回滚校验骨架；<br>3. 旧散落文件（`inject_*.rs`、`store_connection.rs`、`store_vault.rs`）已清理干净；<br>4. [`build/thumbnail-regression.js`](../../build/thumbnail-regression.js) 等读取 Rust 源码的测试已切换新路径。 | 对比基线 `fc1c19d` 的 `cargo test -- --list` 归一化用例数，确认无测试用例在搬家过程中丢失。 |
| `R3-2` | [`app/core/src/fetch.rs`](../../app/core/src/fetch.rs)、[`fetch_enrich.rs`](../../app/core/src/fetch_enrich.rs)、[`qoderweb.rs`](../../app/core/src/qoderweb.rs) | 1. `Fetcher` 锁收敛为内聚状态组（不再 14 把散锁）；<br>2. `fetch_enrich.rs` 被主轮询与探活共用。 | 核验联合入口测试（同一回环上游对轮询与探活同时返回 Codex 重置卡与 MiniMax 明细，两路径结果一致），以及锁中毒测试通过。 |
| `R3-3` | `app/src-tauri/src/{model_cmds,promo_cmds,diag_log_cmds,os_cmds,inject_cmds,sync_cmds,sync_group_cmds,sync_oauth_cmds}.rs`、[`app/core/src/sync/device_flow.rs`](../../app/core/src/sync/device_flow.rs)、[`recovery.rs`](../../app/core/src/sync/recovery.rs) | 1. `src-tauri/src/` 单层平铺未破环 `check-ipc-contract.js`；<br>2. 壳层文件均 `<= 700` 行（核对实际行数）；<br>3. Device Flow 状态机与恢复筛选已在 `core/src/sync/` 有单测。 | 核验 IPC 注册命令集与业务语义无丢失。 |
| `R3-4` | [`app/src/gt-api.js`](../../app/src/gt-api.js)、[`gt-store.js`](../../app/src/gt-store.js)、[`gt-window-state.js`](../../app/src/gt-window-state.js)、[`styles-tokens.css`](../../app/src/styles-tokens.css)、[`styles-panel.css`](../../app/src/styles-panel.css)、[`styles-manage.css`](../../app/src/styles-manage.css)、[`index.html`](../../app/src/index.html) | 1. `gt-api.js` 所有 `invoke(...)` 为静态字符串字面量；<br>2. `styles-*.css` 按令牌→面板→管理面顺序加载，层叠等价；<br>3. `check-css-forms.js` 覆盖全部 3 个拆分 CSS 及功能 CSS；<br>4. `build/lib/app-sources.js` 与测试加载器完整加载新脚本。 | 核对 12 个功能 JS 与 14 个 `app-*.js` 均无加载顺序错误；运行 `node build/probe-frontend.js` 验证基础 DOM 类与状态切换。 |

### Phase 4：Rust 原生托盘绘制与省内存模式

| 卡号 | 必查文件与符号 | 逐卡核验要点 | 重点风险 / 反向验证 |
|---|---|---|---|
| `R4-1` | [`app/core/src/tray_icon.rs`](../../app/core/src/tray_icon.rs)、[`app/src-tauri/src/tray.rs`](../../app/src-tauri/src/tray.rs)、[`app/src/app-tray.js`](../../app/src/app-tray.js) | 1. `tray_icon.rs` 零外部图像依赖输出 RGBA；<br>2. 前端不再调 Canvas→Base64→IPC 刷托盘（胶囊面仍正常工作）；<br>3. `tray.rs` 负责轮播、tooltip 与 `tray_number == false` 还原品牌图标。 | 核验 `tray_icon` 单测覆盖 16/20/24/32/48px 与异常尺寸拦截；**真机多 DPI 清晰度列为阶段 C 必测**。 |
| `R4-2` | [`app/core/src/config.rs`](../../app/core/src/config.rs)、[`app/src-tauri/src/hibernate.rs`](../../app/src-tauri/src/hibernate.rs)、[`lib.rs`](../../app/src-tauri/src/lib.rs)、[`window_cmds.rs`](../../app/src-tauri/src/window_cmds.rs)、[`app/src/app-settings.js`](../../app/src/app-settings.js) | 1. `hibernate_webview_on_hide` 默认 `false`；<br>2. 仅当非胶囊/非贴边且隐藏满 300s 才销毁主 WebView；<br>3. 存在网页登录会话时按设计保留或安全处理（核实 `SESSION_IDS` 逻辑）；<br>4. 唤回等旧标签释放后重建，恢复几何/置顶/缓存数据。 | **核心高风险模块**：必须经阶段 C 真机生命周期与压测验证，确认无同名窗口重建 panic 或位置偏移。 |

---

## 四、阶段 C：Release 构建与原生 WebView2 真机验收矩阵

> 本阶段需用户允许暂停现有运行中实例（或使用隔离数据目录与独立调试端口），所有产出日志/截图放 `build/qa/refactor-acceptance-2026-11-25/`。

### C1. Release 单 exe 构建验证

```powershell
.\build\build.ps1 build
```

记录：编译退出码、耗时、生成 exe 大小（对比基线无异常膨胀、仍为单 exe 交付）。

### C2. 一键原生 CDP 回归脚本

在确认用户数据已按 SHA-256 备份后，使用新构建的 Release exe 运行：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File build/run-native-checks.ps1 -SelfTest
powershell -NoProfile -ExecutionPolicy Bypass -File build/run-native-checks.ps1 -Port 9241 -Exe <新编出的exe路径>
```

**判读纪律**：
- `verify-settings-entry.js`、`verify-checkin-center.js`、`verify-resize.js`、`verify-qoder-expiry.js`、`verify-focus-ring.js`、`verify-gateway-layout.js` 必须全部 `PASS`；
- 若 `verify-ui-0923.js` 或 `verify-form.js` 失败，必须逐条核对是否仅命中 [`docs/report/acceptance-backlog.md`](../report/acceptance-backlog.md) `B11` 记录的已知过时断言（U2 文案、U6 官方标数、U6 紧凑条字母、compact 尺寸、`win-style.ps1` 窗口标题），任何新增失败项直接判为**本次回归**；
- 核查日志确认零加载期 JS 报错、零 `ReferenceError`、零未捕获 Promise 异常。

### C3. 真机专项验收（4 项必测）

| 编号 | 验收场景 | 具体操作与断言 |
|---|---|---|
| **V1** | **UI 全矩阵与切页交互** | 覆盖单列 / 双列 / 缩略 × 深色 / 浅色 × 标准 / 大 / 特大字号；验证设置索引→渠道/显示/模型/网关/密钥箱/同步/关于全部子页切换、在「网关」页停留期间触发一次手动刷新，再切回「渠道」与「显示」确认数据即时同步无陈旧。 |
| **V2** | **模型页客户端探活与网关接入** | 打开「模型」与「网关 → 接入与安全」，观察打开速度（验证 `R1-2` 无黑框闪烁、无 1s 卡顿）；针对未运行与在跑客户端分别点预览，验证 `runningWarning` 与二次确认弹层正常，不误写任何配置文件。 |
| **V3** | **托盘原生绘制多 DPI 与轮播（`R4-1`）** | 在系统 100% / 125% / 150% 缩放下（或至少当前主机物理 DPI + 截图标尺核查）：<br>1. 验证托盘数字清晰可辨、无裁边或花屏；<br>2. 绿/黄/红/灰四种状态底色正确；<br>3. 开启「托盘数字轮播」并多选渠道，观察按设定驻留秒数轮播切换数字与 tooltip 同步更新；<br>4. 关闭「托盘图标显示余额数字」立即还原品牌图标。 |
| **V4** | **省内存模式全生命周期压测（`R4-2`）** | 1. **关闭态对照组**：开关关闭时隐藏 >300s，确认 WebView2 进程不退出；<br>2. **开启态休眠**：开启开关 → 隐藏到托盘 → 等待满 300s → 任务管理器/进程查询核验 WebView2 渲染子进程退出、进程树总工作集降至 `< 80 MiB`；<br>3. **休眠期后台存活**：在 WebView 销毁期间，向本地网关发 `/healthz` 或回环聊天请求确认 200 应答且台账记录写入；确认托盘数字继续随轮播/轮询更新；<br>4. **三种唤回路径**：分别通过**托盘左键**、**全局热键**、**托盘菜单「显示面板」**（以及二次启动单实例）唤回，记录唤回耗时，确认主窗口正确恢复形态、坐标、尺寸、置顶状态，且首屏秒出内存缓存余额；<br>5. **防抖压测**：隐藏后 10 秒内快速连续按热键切换显隐 10 次，确认无同名窗口冲突、无崩溃、无 `startup-error.log` 错误。 |

### C4. 真实数据库 `%TEMP%` 副本迁移只读演练（`R1-1`）

1. 将用户 `%APPDATA%\com.wacil.tokenscope\tokenscope.db*` 复制到 `%TEMP%\gt-v9-probe\`（**绝不碰原目录**）；
2. 用新版本 `Store::open` 打开副本完成 v8→v9 迁移；
3. 记录并对比迁移前后：`snapshots` 行数（必须 100% 一致）、`idx_snap_fp` 已不存在、`idx_snap_fp_cover` 存在、`PRAGMA integrity_check == "ok"`、`tokenscope.db` 体积与 `freelist_count`。测毕删净 `%TEMP%\gt-v9-probe\`。

---

## 五、阶段 D：放行判据与标准化验收报告模板

### 1. 放行判据

- **PASS（可进入发版/合入主干）**：阶段 A 全部门禁与 9 项哨兵全绿；阶段 B 12 张卡核验全部通过（`R1-2` 耗时实测确认明显优于旧 `tasklist` 且无功能/稳定性隐患）；阶段 C 原生回归、V1–V4（含真实多 DPI 托盘与省内存模式）及数据库迁移演练全部通过，用户数据还原 SHA-256 `MATCH`，无 P0/P1 缺陷。
- **CONDITIONAL PASS（有条件通过）**：仅当阶段 A、B 全部通过，且未跑项仅受外部条件限制（如本机无多 DPI 切换环境或无特定真实三方账号），同时已清楚列明缺失项并写入 [`docs/report/acceptance-backlog.md`](../report/acceptance-backlog.md)。
- **FAIL（打回修复）**：出现以下任一情形即判失败：任一静态门/单测/哨兵失败；`cargo check` 出现编译警告或 `--locked` 失败；发现凭据明文泄漏；数据库迁移丢行或 `integrity_check` 非 `ok`；省内存模式导致唤醒崩溃、几何丢失、本地网关停服或托盘停止更新；用户数据还原后 SHA-256 不一致。

### 2. 验收报告产出格式

验收完成后，请将报告写入 `docs/report/architecture-refactor-acceptance-report-2026-11-25.md`（按 UTF-8 保存，不含本机绝对路径），模板如下：

```markdown
# Grandettoken 架构重构验收报告（2026-11-25）

- **验收结论**：`PASS` / `CONDITIONAL PASS` / `FAIL`
- **基线对比**：`fc1c19d` (`v0.3.76`) → 当前工作区
- **验收环境**：OS / DPI / Rust 工具链 / Node 版本 / 浏览器版本

## 1. 阶段 A 自动化门禁结果
| 门禁项 | 命令 | 实测结果（含用例数/耗时） | 状态 |
|---|---|---|---|
| 静态门禁（含 `--self-test`） | ... | ... | PASS/FAIL |
| `jsdom-smoke` + 9 哨兵 | ... | ... | PASS/FAIL |
| 6 项专项前端验证 | ... | ... | PASS/FAIL |
| `app/core` 单测 + `sync_fixtures` | ... | ... passed / ... ignored | PASS/FAIL |
| `cargo check --all-targets --locked` | ... | ... warnings | PASS/FAIL |
| `scan-secrets.ps1` | ... | ... files | CLEAN/FAIL |

## 2. 阶段 B 逐卡核验结论（R1-1 ~ R4-2）
| 卡号 | 源码核验要点 | 反向测试结果 | 结论 | 备注 |
|---|---|---|---|---|
| R1-1 ~ R4-2 | ... | ... | PASS/FAIL/PARTIAL | ... |

## 3. 量化指标实测对比
| 指标 | 基线 / 预期 | 本次实测 | 结论 |
|---|---|---|---|
| `ProcessSnapshot::capture()` vs `tasklist` 8 进程 | 预期 <5ms（开发测 14.4ms） | ... ms vs ... ms | ... |
| 真实库副本 v8→v9 体积变化（`tokenscope.db`） | 删 `idx_snap_fp` 省 ~20% | ... MB → ... MB | ... |
| 省内存模式休眠前/后进程树工作集 | < 80 MiB | ... MiB → ... MiB | ... |
| Release 单 exe 体积 | — | ... MB | ... |
| 主 Window 唤回耗时 | < 1s（参考 1.7s） | ... s | ... |

## 4. 阶段 C 真机验收记录与用户数据还原校验
- `run-native-checks.ps1` 结果（逐脚本列明，区分 B11 与新回归）
- V1–V4 实测记录与截图目录（本地 QA 目录，不入库）
- 用户数据恢复前后 SHA-256 比对：`MATCH` / `MISMATCH`

## 5. 缺陷清单（如有）与未跑项
| 编号 | 级别（P0/P1/P2） | 关联任务卡 | 复现步骤与现象 | 建议动作 |
|---|---|---|---|---|
```
