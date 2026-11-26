# Grandettoken 架构重构开发计划

> **日期**：2026-11-25
>
> **基线版本**：`v0.3.76`（commit `fc1c19d`）
>
> **适用对象**：执行本仓库重构任务的自动化 Agent / 开发者
>
> **性质**：可执行开发计划（共 4 个阶段、12 张独立任务卡）

---

## 任务卡认领与状态总表

> 执行 Agent 认领或完成某张卡后，请在此表同步状态，并在仓库根 `PROGRESS.md` 文末追加实测记录。

| 卡号 | 阶段 | 标题 | 依赖 | 优先级 | 状态 |
|---|---|---|---|---|---|
| `R1-1` | Phase 1 | 移除 `snapshots` 冗余索引 `idx_snap_fp`（数据库瘦身 ~20%） | 无 | P0 | 完成（Codex，2026-11-25） |
| `R1-2` | Phase 1 | Win32 原生进程快照替换 `tasklist.exe` 子进程风暴 | 无 | P1 | 实现完成，耗时未达预期（Codex，2026-11-25） |
| `R1-3` | Phase 1 | 本地网关热路径去同步 IO（内存缓存 + 异步台账写队列） | 无 | P0 | 完成（Codex，2026-11-25） |
| `R1-4` | Phase 1 | 收敛零散临时 `reqwest::Client` 与临时 Tokio Runtime | 无 | P2 | 完成（Codex，2026-11-25） |
| `R2-1` | Phase 2 | SQLite 读写分离与重型查询/维护下沉 `spawn_blocking` | `R1-1`, `R1-3` | P1 | 完成（Codex，2026-11-25） |
| `R2-2` | Phase 2 | 前端视图脏标记（Dirty Flag）与容器级局部扫描 | 无 | P2 | 完成（Codex，2026-11-25） |
| `R3-1` | Phase 3 | `app/core/src` 领域目录化（`store/`、`inject/`、`providers/`）与大文件清零 | Phase 1–2 | 维护 | 完成（Codex，2026-11-25） |
| `R3-2` | Phase 3 | `Fetcher` 状态收敛（14 锁 → 3 组）与渠道后置钩子统一 | Phase 1–2 | 维护 | 完成（Codex，2026-11-25） |
| `R3-3` | Phase 3 | 削薄 `src-tauri` 壳层（拆分 `model_cmds.rs` + 下沉 `sync_cmds.rs` 纯逻辑） | Phase 1–2 | 维护 | 完成（Codex，2026-11-25） |
| `R3-4` | Phase 3 | 前端渐进模块化（`gt-api.js` + `gt-store.js` + `styles.css` 拆分） | `R2-2` | 维护 | 完成（Codex，2026-11-25） |
| `R4-1` | Phase 4 | 将托盘数字图标绘制从前端 Canvas 迁至 Rust 原生位图渲染（可选） | Phase 1–3 | 可选 | 实现完成，原生 DPI 待验（Codex，2026-11-25） |
| `R4-2` | Phase 4 | 可选「省内存模式」（隐藏到托盘空闲后释放 WebView2） | `R4-1` | 可选 | 完成（Codex，2026-11-25） |

---

## 一、架构现状诊断摘要（基于 `v0.3.76` 源码核查）

### 1. 应保留的架构基座
- **双 Crate 解耦**：`tokenscope-core`（[`app/core`](../../app/core/Cargo.toml)）与 `src-tauri`（[`app/src-tauri`](../../app/src-tauri/Cargo.toml)）隔离，纯逻辑可在无 WebView2 下秒级完成 670+ 单测。
- **单 exe + 零打包前端 + 双向门禁**：静态链接 WebView2Loader、10+ 道自带 `--self-test` 的静态与冒烟门禁（IPC 契约、静默失败、文件规模棘轮、锁中毒门等）提供了可靠的防退化护栏。

### 2. 核心失配点与主要瓶颈
项目已从早期的「4 渠道余额小挂件」演进为「14+ 渠道监控 + 签到 + 密钥箱 + Gist 加密同步 + 8 家 AI Agent 配置注入 + 常驻本地多协议 LLM 网关」，但底层仍沿用早期挂件的三条隐含假设：
1. **数据面与控制面混用同一套同步慢路径**：本地模型网关每笔请求同步触发 Win32 `CredReadW`、SQLite `vault_list()`、4SAPI 整份 JSON 价目表二次反序列化与注册表读取，并同步写 SQLite 台账。
2. **全应用共享一把 `Arc<Mutex<Store>>` 单连接**：在 tokio async 线程上直接持锁执行每日 `prune`/`wal_checkpoint`、14 渠道批量历史读与网关台账写入。
3. **前端与 Core 仍存隐式耦合与目录膨胀**：前端 14 个 `app-*.js` 仍共享 436 个裸全局顶层声明；`Fetcher` 聚集 14 把独立 `Mutex` 且主轮询与探活存在重复后置逻辑；`app/core/src/` 根层平铺 45+ 个文件。

> **关于内存占用的客观边界**：历史实测 Rust 主进程工作集约 **44–60 MB**，整棵进程树约 **430 MB**，其中 **WebView2（Chromium）占 ~390 MB（~90%）**。Phase 1–3 重点解决 CPU 抖动、子进程卡顿、锁争用、磁盘增长（`idx_snap_fp` 占主库 20.86%）与代码可维护性；若需将托盘常驻空闲内存压至 **~50–60 MB**，需启用可选 **Phase 4**。

---

## 二、给执行 Agent 的全局硬约束（所有任务卡通用）

1. **严格按卡划界**：只改本卡范围，一卡一提交（或按卡单独验证后再合并）；严禁顺手改无关业务文案、UI 视觉或推翻 [`DESIGN.md`](../../DESIGN.md) §16 用户裁决。
2. **绝不触碰的兼容性红线**：
   - Windows 凭据管理器服务名 `TokenScope` 与 bundle identifier `com.wacil.tokenscope` **绝对不可更改**；
   - 严禁把任何 API Key / Cookie / Token / 访问密钥写入 SQLite、JSON 配置、日志或导出包；
   - 保持单 exe 交付与零 npm 打包构建链，不引入新的 C/汇编构建依赖；
   - 不做自动 `VACUUM`（仅允许用户主动触发入口）；不做订阅反代/中继。
3. **门禁避坑铁律**：
   - [`build/check-ipc-contract.js`](../../build/check-ipc-contract.js) 只扫描 `app/src/*.js` 与 `app/src-tauri/src/*.rs` **单层目录**，且**禁止动态 `invoke(var, ...)`**（所有 `invoke("cmd")` 必须为字符串字面量）；
   - [`build/check-app-split.js`](../../build/check-app-split.js) 校验 `app-*.js` 首尾必须为 `app-core.js` / `app-boot.js`、跨文件顶层声明不重名，且 [`build/check-file-size.js`](../../build/check-file-size.js) 对 `app*.js` 与超标文件实施行数棘轮（上限只许下调）；
   - [`build/check-lock-unwrap.js`](../../build/check-lock-unwrap.js) 禁止生产源码出现 `.lock()/.read()/.write()` 后接 `.unwrap()/.expect()`，统一走 `crate::lockx::lock(...)`（core）或 `lock_rw(...)`（src-tauri）；
   - 所有文本文件按 UTF-8 读写（PowerShell 须显式 `-Encoding utf8`），通过 [`build/check-encoding.js`](../../build/check-encoding.js) 校验。
4. **验证纪律（见 [`AGENTS.md`](../../AGENTS.md) §2.1 & §4.1）**：
   - 所有验证**串行执行**，Rust 测试需设 `NO_PROXY=127.0.0.1,localhost,::1`，`src-tauri` 跑 `cargo check --all-targets --locked`（本机不跑 `src-tauri` 的 `cargo test`）；
   - 每张新增或修改行为的卡片必须有**双向验证**（正常全绿 + 坏样本/哨兵必红），并在 [`PROGRESS.md`](../../PROGRESS.md) 文末追加实测记录。

---

## Phase 1：高确定性性能与资源减负（推荐优先执行，零 UI 行为变化）

> **依赖关系**：`R1-1`、`R1-2`、`R1-3`、`R1-4` 彼此低耦合，可按序执行或分分支并行。

### 任务卡 `R1-1`：移除 `snapshots` 冗余索引 `idx_snap_fp`（数据库瘦身 ~20%）

- **目标**：消除 `snapshots` 表上严格被覆盖索引 `idx_snap_fp_cover` 包含的最左前缀索引 `idx_snap_fp`，削减约 20% 稳态磁盘体积与 ~33% 快照索引写放大（按 [`docs/report/db-growth-measurement-2026-11-22.md`](../report/db-growth-measurement-2026-11-22.md) 实测，该冗余索引占主库 20.86%）。
- **涉及文件**：
  - [`app/core/src/store.rs`](../../app/core/src/store.rs)（`migrate`、`ensure_covering_index`）
  - [`app/core/src/store_tests.rs`](../../app/core/src/store_tests.rs)
- **具体需求**：
  1. 将 `Store::migrate()` 中 `WANT` 从 `8` 升级到 `9`：
     - 先同步建立 `idx_snap_fp_cover ON snapshots (provider_id, key_fp, ts, remaining)`（升级为迁移必备索引，不再仅靠 `ensure_covering_index` 尽力而为）；
     - 确认覆盖索引存在后，执行 `DROP INDEX IF EXISTS idx_snap_fp;`；
     - 保留 `idx_snap_lookup ON snapshots (provider_id, ts DESC)`（仍服务不带 `key_fp` 的查询如 `latest` / `sql_any` / `prev_remaining`）。
  2. 不再在迁移中新建 `idx_snap_fp`（历史老库升级上来也直接建 `idx_snap_fp_cover` 并删 `idx_snap_fp`）。
  3. **禁止事项**：不得删除 `idx_snap_lookup`；不得引入自动 `VACUUM`。
- **验收标准 & 双向验证**：
  - 单测构造 v8 老库（含 `idx_snap_fp` 与真实快照数据），升级打开后验证：`sqlite_master` 中无 `idx_snap_fp`、存在 `idx_snap_fp_cover`、`PRAGMA user_version == 9`、`PRAGMA integrity_check == "ok"`；
  - 用 `EXPLAIN QUERY PLAN` 断言核心读路径（`history_between`、`adopt_orphan_snapshots`、`fingerprint_count`、`consumption_since` 带 `key_fp`）全部命中 `idx_snap_fp_cover` 且无 `SCAN snapshots`；
  - **反向验证**：临时去掉 `idx_snap_fp_cover` 后，`EXPLAIN QUERY PLAN` 断言必须失败（证红）。

**Codex 完成记录（2026-11-25）**

- `store.rs` 将覆盖索引建立、冗余索引删除和版本号写入放在同一个 SQLite 事务。指纹消耗查询单列 SQL，使查询计划能按提供方、指纹和时间定位。保留 `idx_snap_lookup`，释放的页由 SQLite 复用。
- `store_tests.rs` 用磁盘 v8 夹具验证原数据、索引清单、v9 版本和 `integrity_check`；四条生产 SQL 的查询计划均命中覆盖索引，删索引后四条均失去覆盖。旧库补指纹列的夹具也已复测。
- 本卡标准串行门禁全绿，含前端专项、core 单测与同步夹具、core 和 Tauri 全目标编译及凭据扫描。实际库文件占用仍取决于空闲页复用。

---

### 任务卡 `R1-2`：Win32 原生进程快照替换 `tasklist.exe` 子进程风暴

- **目标**：彻底消除 `inject` 检测 8 家客户端进程状态时串行拉起 8–12 个 `tasklist.exe` 子进程造成的 0.5–1.5s 卡顿与管道超时风险，将进程检测降至 `< 1 ms`。
- **涉及文件**：
  - [`app/core/Cargo.toml`](../../app/core/Cargo.toml)
  - [`app/core/src/inject.rs`](../../app/core/src/inject.rs)（`process_running`、`sites`、`blocked_by_running`、`state`）
  - [`app/core/src/inject_drift.rs`](../../app/core/src/inject_drift.rs)
  - [`app/core/src/inject_tests.rs`](../../app/core/src/inject_tests.rs)、[`app/core/src/inject_tests_trae_dsh.rs`](../../app/core/src/inject_tests_trae_dsh.rs)
- **具体需求**：
  1. 在 [`app/core/Cargo.toml`](../../app/core/Cargo.toml) 的 `[target.'cfg(windows)'.dependencies] windows-sys` features 中新增 `"Win32_System_Diagnostics_ToolHelp"`（检查 `Cargo.lock` 并保证离线 `--locked` 通过）。
  2. 在 `inject` 模块封装 `pub struct ProcessSnapshot(HashSet<String>)`（进程名统一转 ASCII 小写存储）：
     - Windows 实现：调用 `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)` + `Process32FirstW` / `Process32NextW`，通过 RAII guard 确保 `CloseHandle` 必然释放；解码 UTF-16 `szExeFile` 直到首个 `\0`，转小写插入集合；
     - 非 Windows 实现返回空集合或保留回退；
     - 提供 `ProcessSnapshot::capture() -> Self`、`ProcessSnapshot::from_names(iter)`（测试注入）和 `has(&self, process: &str) -> bool`（忽略大小写）。
  3. 改造所有调用点：
     - `sites()`、`inject_drift`、`state()` 入口**单次调用** `ProcessSnapshot::capture()` 并透传引用，不再对每个客户端/每个进程名分别探测；
     - `process_running(name)` 内部委托 `ProcessSnapshot::capture().has(name)` 保持兼容签名；
     - `blocked_by_running(site)` 与 `Client::processes()` 全名任一命中语义保持不变（`opencode-cli.exe` / `opencode-pty.exe` / Trae 多变体须 100% 等价）。
  4. 删除已无调用方的 `tasklist` 子进程代码与 `tasklist_has`（同步更新相关旧单测）。
- **验收标准 & 双向验证**：
  - 单测验证当前测试进程自身 exe 名（`std::env::current_exe()` 文件名）能被 `ProcessSnapshot::capture().has(...)` 命中（含大小写不敏感），且不存在的随机名返回 `false`；
  - 单测验证 `blocked_by_running` 在 `opencode-cli.exe` 命中时返回实际命中进程名（遵守 `AGENTS.md §8.6`）；
  - 实测 `ProcessSnapshot::capture()` 耗时（预期 `< 5 ms`）；
  - **反向验证**：向 `from_names` 注入不含目标进程的集合，断言 `blocked_by_running` 返回 `None`；注入含目标进程名的集合，断言返回 `Some(name)`。

**Codex 实现记录（2026-11-25）**

- `inject/process.rs` 封装 ToolHelp 快照和 RAII 句柄。配置发现、状态、漂移检查和卸载清理各取一次快照。Trae 与 OpenCode 状态显示实际匹配的进程名。
- 本卡标准串行门禁与 `--locked --offline` 真机进程名测试通过。合成快照同时验证精确文件名、大小写、无命中和 OpenCode 的两个桌面进程变体。
- 当前 Windows 环境独立复跑五次，单次快照耗时 13.9–15.5 ms，最终原生用例复跑为 14.4 ms。独立 Win32 探针的中位数为 14.2 ms，其中创建进程快照本身约 10.9 ms。本机未达到计划预期的 5 ms。开发路径已移除 `tasklist.exe` 调用。

---

### 任务卡 `R1-3`：本地网关热路径去同步 IO（内存缓存 + 异步台账写队列）

- **目标**：清除 Axum 网关热路径上的四处同步阻塞（每请求 `CredReadW`、每请求 1–2 次 SQLite `vault_list()`、每笔 4SAPI 请求锁库反序列化 160 模型价目表、每笔请求读 Windows 注册表），并将台账写盘异步化。
- **涉及文件**：
  - [`app/core/src/gateway/mod.rs`](../../app/core/src/gateway/mod.rs)
  - [`app/core/src/gateway/http.rs`](../../app/core/src/gateway/http.rs)、[`dispatch.rs`](../../app/core/src/gateway/http/dispatch.rs)、[`routing.rs`](../../app/core/src/gateway/routing.rs)、[`lifecycle.rs`](../../app/core/src/gateway/lifecycle.rs)、[`refresh.rs`](../../app/core/src/gateway/refresh.rs)
  - [`app/core/src/store_gateway.rs`](../../app/core/src/store_gateway.rs)、[`app/core/src/models.rs`](../../app/core/src/models.rs)、[`app/core/src/proxy.rs`](../../app/core/src/proxy.rs)
  - [`app/src-tauri/src/gateway.rs`](../../app/src-tauri/src/gateway.rs)、[`vault_cmds.rs`](../../app/src-tauri/src/vault_cmds.rs)、[`vault_sync.rs`](../../app/src-tauri/src/vault_sync.rs)、[`model_cmds.rs`](../../app/src-tauri/src/model_cmds.rs)
- **具体需求**：
  1. **访问令牌摘要缓存**（`Gateway::Inner`）：
     - 新增 `access_token_sha256: Option<[u8; 32]>`（仅存 SHA-256 摘要，不常驻明文）；
     - 首次鉴权或 `gateway_start` / `gateway_secret` 刷新密钥时从 `access.get(SECRET_KEY)` 读一次并缓存摘要；鉴权时对入站 token 算 SHA-256 后做常量时间异或比对；
     - 在写入/轮换 `SECRET_KEY` 的全部路径（`gateway::lifecycle::start`、`src-tauri/src/gateway.rs` 的 `secret()` / `gateway_secret`）调用 `g.invalidate_access_token()`；
     - 保留无缓存纯函数 `authorized_with` 供直接传入独立 mock `CredentialStore` 的单元测试调用，网关热路径走 `Gateway::is_authorized(&self, headers, settings)`。
  2. **Vault 元数据内存投影**（`Gateway::Inner`）：
     - 新增 `vault_index: HashMap<String, VaultMeta>`（含 `id`、`platform`、`updated_at`、`label`，**严禁存密钥**）；
     - `Store` 层暴露单调 `vault_revision: AtomicU64`（每次 `vault_upsert` / `vault_upsert_default` / `vault_delete` / `vault_metadata_and_setting` 推进修订号），`Gateway` 比对修订号变化时才重读一次 SQLite（从根源避免遗漏失效点）；
     - `g.live_channels()` 与 `snapshot_vault_platforms(g)` 改为读内存 `vault_index`，同一次请求不再重复锁 `store` 扫表。
  3. **4SAPI 价目表 `Arc<Catalog>` 内存缓存**：
     - 在 `models.rs` 或 `Gateway::Inner` 按 `ModelCache` 修订号/时间戳缓存 `Arc<PricingLookup>`（内部为 `HashMap<String, ModelPricing>`，O(1) 查找）；
     - `ModelCache::save` 推进修订号；`estimate_channel_cost_cny` 优先走内存 `PricingLookup`，消除每笔请求 2 次 `serde_json::from_str`。
  4. **`ProxyClients` 代理快照节流**（[`app/core/src/proxy.rs`](../../app/core/src/proxy.rs)）：
     - 在 `ProxyClientCache` 记录 `checked_at: Instant`，距离上次检查不足 **5 秒**直接复用当前 `client.clone()`，跳过 Windows 注册表读取与 9 次 `env::var_os`；
     - 保留 `ProxyClients::invalidate()`（供单测或设置显式刷新，确保既有 `proxy_changes_refresh_the_client_and_direct_groups_bypass_it` 单元测试 100% 通过）。
  5. **网关台账异步落盘队列**：
     - `Gateway` 启动轻量后台写协程，通过有界 `tokio::sync::mpsc::channel::<GatewayUsageRow>(256)` 接收台账记录并在 `spawn_blocking` 中写入 SQLite（含每 100 笔 prune）；
     - `record_diag` 改为 `try_send`（若队列满则记节流 `diaglog` 告警而不阻塞请求；在无 tokio runtime 的同步单元测试环境下直接回退同步写以兼容全部既有单测）；
     - 优雅停服/退出前 `flush` 队列，确保不丢最后几笔记录。
- **验收标准 & 双向验证**：
  - 新增计数单测（参考 `lifecycle.rs` 已有的 `reads: AtomicUsize` 模式）：连续发送 10 笔网关请求，断言：
    - `access.get(SECRET_KEY)` 调用次数 `== 1`（刷新密钥后重新读 1 次）；
    - `ModelCache::load` / JSON 解析次数 `<= 1`；
    - `vault_list` 在无变更时调用次数 `<= 1`，`vault_delete` 后立即失效并同步反映到 `live_channels()`；
  - 跑通 `magpie_tests`、`http_bridge`、`gateway::tests` 全套测试；
  - **反向验证**：单测里故意跳过失效钩子（vault 删除或密钥轮换不刷新缓存），对应测试必须变红。

**Codex 完成记录（2026-11-25）**

- `gateway/access.rs` 集中处理启动、轮换、接入预览和鉴权。密钥写入与摘要替换串行，常驻缓存只含摘要。回环测试连续十次请求只读一次凭据，轮换后旧值立即失效。
- `gateway/projection.rs` 按 Store 修订号缓存密钥箱元数据和 4SAPI 价目表。在首次读取及写入后的首次读取重载，稳定修订内复用。代理池按 5 秒间隔复核系统设置。
- `gateway/ledger.rs` 用 256 项队列批量下沉 SQLite 写入和清理。清空、查询前刷新、监听停服与应用退出都接入队列顺序。应用退出给在途连接最多 5 秒完成，随后刷新已入队记录。
- 本卡静态、前端、core 专项与全量、桌面全目标编译和凭据扫描通过。临时移除轮换时的摘要替换和删除时的修订号推进，两个对应断言精确报红，源码已恢复。

---

### 任务卡 `R1-4`：收敛零散临时 `reqwest::Client` 与临时 Tokio Runtime

- **目标**：消除高频或后台路径中反复构建 `reqwest::Client`（重建 SChannel TLS 上下文与连接池）及临时 `tokio::runtime` 的浪费。
- **涉及文件**：
  - 新增 `app/core/src/http_pool.rs`（并在 [`app/core/src/lib.rs`](../../app/core/src/lib.rs) 注册）
  - [`app/core/src/minimax.rs`](../../app/core/src/minimax.rs)（L382 附近）
  - [`app/core/src/qoder.rs`](../../app/core/src/qoder.rs)（L718 附近）
  - [`app/core/src/workbuddy.rs`](../../app/core/src/workbuddy.rs)（L1478 附近）
  - [`app/core/src/sync/gist.rs`](../../app/core/src/sync/gist.rs)、[`app/src-tauri/src/sync_cmds.rs`](../../app/src-tauri/src/sync_cmds.rs)（6 处 `GistClient::new`）
- **具体需求**：
  1. 在 `http_pool.rs` 提供按策略共享的 `OnceLock<reqwest::Client>` 工厂：
     - `shared_default()`（走系统代理、复用连接池）；
     - `shared_direct()`（`no_proxy()`，供需要直连的签到/刷新复用）。
  2. 替换 `minimax.rs`、`qoder.rs`、`workbuddy.rs` 中的局部 `reqwest::Client::builder().build()` 为共享客户端（超时通过单次请求的 `.timeout(...)` 设定，不破坏原有超时语义）；
  3. 审查 `workbuddy.rs` 与 `minimax.rs` 的阻塞续期路径：优先使用 `tokio::runtime::Handle::try_current()`（或将续期异步化，或在 `spawn_blocking` 中复用共享的轻量 `OnceLock<tokio::runtime::Runtime>` 而非每次重建 OS 线程池）。
  4. `GistClient` 内部改为共享静态 `reqwest::Client`，Bearer token 改为按次请求设置 `Authorization` 头，不再每次 `GistClient::new(token)` 新建 HTTP 客户端。
- **验收标准 & 双向验证**：
  - grep 确认生产代码中除 `http_pool.rs`、`fetch.rs`（主池）、`proxy.rs`（动态代理池）、`checkin.rs`（直连单例）、`download.rs`、`model_cmds.rs` 外不再存在函数内每次现建的 `reqwest::Client::builder()`；
  - `minimax` / `qoder` / `workbuddy` / `sync` 全部单测全绿（含 `sync_fixtures`）。

**Codex 完成记录（2026-11-25）**

- `http_pool.rs` 提供系统代理和直连两个共享客户端，并为同步续期入口提供持续运行的单工作线程 Tokio 运行时。MiniMax、WorkBuddy、Qoder 真机测试入口与签到使用相应的共享资源。
- Gist 与 GitHub 设备授权沿用原各自的 User-Agent 和超时，改用同一默认池。Gist 按请求添加敏感鉴权头，测试证明两个身份以及匿名请求之间未串用鉴权。
- 本卡标准串行门禁通过。源码搜索确认剩余生产客户端构造位于池模块、取数池、代理池、下载和模型入口。需要真实登录态的忽略用例保持默认跳过。

---

## Phase 2：SQLite 并发隔离与前端按需渲染（消除长尾卡顿与无效 DOM 开销）

> **依赖关系**：`R2-1` 建议在 `R1-1` 与 `R1-3` 完成后执行；`R2-2` 独立，可随时与 Phase 1 并行。

### 任务卡 `R2-1`：SQLite 读写分离与重型查询/维护下沉 `spawn_blocking`

- **目标**：消除 [`poll.rs`](../../app/src-tauri/src/poll.rs) 每日 `prune` / `wal_checkpoint`、[`fetch.rs`](../../app/core/src/fetch.rs) 14 渠道并发 `build_view`、[`store_gateway.rs`](../../app/core/src/store_gateway.rs) 30 天聚合查询在 tokio async 线程上直接锁单连接导致的阻塞与锁争用。
- **涉及文件**：
  - [`app/core/src/store.rs`](../../app/core/src/store.rs)
  - [`app/src-tauri/src/lib.rs`](../../app/src-tauri/src/lib.rs)（`AppState`）
  - [`app/src-tauri/src/poll.rs`](../../app/src-tauri/src/poll.rs)（L79–88）
  - [`app/core/src/fetch.rs`](../../app/core/src/fetch.rs)（`build_view` / `Snapshot::save` 段）
  - [`app/src-tauri/src/gateway_ext.rs`](../../app/src-tauri/src/gateway_ext.rs)（`gateway_usage_summary`）
- **具体需求**：
  1. 在 `Store` 新增只读伴生连接打开方法 `Store::open_readonly(path: &Path) -> Result<Self, String>`（使用 `OpenFlags::SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX`，配 `busy_timeout(5s)` 与 `PRAGMA query_only = ON`）；内存库 `open_memory()` 保持单连接兼容测试。
  2. `AppState`（或封装 `DbPool { write: Arc<Mutex<Store>>, read: Arc<Mutex<Store>> }`）对外保持写路径全部走 `write`、重型只读查询（`history_between` / `build_view` / `gateway_usage_summary` / `channel_changes` / `checkin_history`）走 `read`。
  3. **将所有重型 SQLite 操作从 tokio async worker 迁入 `tokio::task::spawn_blocking`**：
     - `poll.rs` 每日 `prune()` + `wal_checkpoint()`；
     - `gateway_ext.rs` 的 `gateway_usage_summary`；
     - `fetch.rs` 中单渠道取数完成后的 `Snapshot::save` + `build_view` 同步段。
  4. **锁序红线**：严格维持 `AGENTS.md` 既有全局锁序（先 `store` 后 `config`，读连接永不与写连接嵌套持锁，杜绝 AB-BA 死锁）。
- **验收标准 & 双向验证**：
  - 新增并发单测：在线程 A 对写连接执行长事务写的同时，线程 B 通过只读连接并发执行 `history_between` / `gateway_usage_summary`，断言读连接不被写事务阻塞（WAL 并发读成立）且返回一致快照；
  - 单测验证对 `open_readonly` 连接调用任何写方法（如 `record_fp`）必然返回 `Err`（`query_only` 强制生效，反向验证）；
  - 全量 `cargo test --locked -- --test-threads=1` 与 `src-tauri` `cargo check --all-targets --locked` 零警告通过。

**Codex 实施拆分（2026-11-25）**。先接入只读连接及桌面查询，再独立拆出取数的数据库阶段，最后验证事务内写入对并发读者的可见边界。

**Codex 完成记录（2026-11-25）**

- `store_connection.rs` 打开只读伴生连接并设置 `query_only` 与 5 秒等待。桌面历史、签到历史与网关账本统计切到读连接。打开读连接失败时保留共享连接作为退路，并将原因留给启动提示。
- `fetch_db.rs` 负责取数视图和快照持久化。快照落盘、重计算及每日清理均在阻塞线程执行，读写阶段依次释放各自的连接。现有单连接取数入口继续兼容内存库。
- 磁盘 WAL 测试证实未提交修改期间，另一个线程能立即读到旧的已提交历史和账本，提交后能看到新值。只读写入失败，`query_only` 状态正确。标准串行门禁通过。

---

### 任务卡 `R2-2`：前端视图脏标记（Dirty Flag）与容器级局部扫描

- **目标**：消除面板态/非当前设置子页下的无效 DOM 重建与全文档扫描，避免在「网关 / 密钥箱 / 模型」子页停留时因后台余额轮询不断重建隐藏的 `#chList` 与 `#cfgPill`。
- **涉及文件**：
  - [`app/src/app-view.js`](../../app/src/app-view.js)（`renderInner`、`fitCompact`）
  - [`app/src/app-channel-cards.js`](../../app/src/app-channel-cards.js)（`refreshChCards`、`renderPillPick`）
  - [`app/src/app-core.js`](../../app/src/app-core.js)（`tickPeakCountdowns`）
  - [`app/src/app-manage.js`](../../app/src/app-manage.js) / [`app/src/app-settings.js`](../../app/src/app-settings.js)（`showTab`）
  - [`build/jsdom-smoke.js`](../../build/jsdom-smoke.js)
- **具体需求**：
  1. 维护当前激活设置子页状态与脏标记集合 `DIRTY_TABS = new Set()`：
     - `isTabVisible(tab)`：`manageOpen() && !document.body.classList.contains("manage-index") && currentTab === tab`；
     - `renderInner()` 中：
       - `renderPillPick()` 仅在 `isTabVisible("disp")` 时立即执行，否则 `DIRTY_TABS.add("disp")`；
       - `refreshChCards()` 仅在 `isTabVisible("ch")` 时立即执行，否则 `DIRTY_TABS.add("ch")`；
     - `showTab(tab)` 切入目标子页时，若 `DIRTY_TABS.has(tab)` 则消费该标记并立即补绘对应面板（`ch` → `refreshChCards()`，`disp` → `renderPillPick()`，`index` → `renderManageIndex()`）。
  2. 优化 `fitCompact()`：维护 `let WAS_COMPACT = false;`；非紧凑条形态且 `!WAS_COMPACT` 时 O(1) 直接返回，不再每轮遍历 `#list .row` 与 `querySelectorAll(".more,.cempty,.ckchip")`；仅在当前是紧凑条或刚离开紧凑条（`WAS_COMPACT === true`）时执行清理。
  3. 优化 `tickPeakCountdowns(root = document)`：当 `root === document` 时，只扫描 `#list` 和（若 `DETAIL_ID` 非空）`#detailBody`，不再全文档遍历隐藏的 8 个设置子页 DOM。
  4. **禁止事项**：不可门控 `renderPill` / 托盘数字轮播 / 数据接收 / 签到与同步状态更新；不得破坏 `PERFORMANCE_SENTINEL` 与现有焦点保留语义。
- **验收标准 & 双向验证**：
  - 在 [`build/jsdom-smoke.js`](../../build/jsdom-smoke.js) 新增测试：
    1. 停留在「本地模型网关」子页时推送新渠道数据，断言 `#chList` 与 `#cfgPill` 零重建；切回「渠道」/「显示」子页后立即看到最新数据；
    2. 面板态下推送数据，`fitCompact` 不触发 `.row` 遍历清理，从紧凑条切回面板时仍完整恢复 `.lv1/.lv2/display`；
  - 新增反向哨兵（如 `TAB_DIRTY_SENTINEL=1`）：关闭子页门控或切页不消费脏标记时，对应测试必须判红退出 1；
  - 跑通 `jsdom-smoke.js`、`verify-refresh-stability.js --assert`、`check-file-size.js`（并维持或下调 `app*.js` 总行数预算）。

**Codex 完成记录（2026-11-25）**

- 管理子页隐藏时积累脏标记，切进渠道或显示页时补绘。面板连续刷新跳过紧凑条清理，切回面板仍恢复降级类和隐藏行。倒计时从列表与打开的详情容器刷新。
- jsdom 验证后台推送与切页补绘、形态切换以及元数据请求复用。反向哨兵命中三个指定断言。旧的元数据测试改为等待前一个保存链及当前请求结算，避免异步夹具串扰。
- 本卡标准串行门禁通过。Edge 无头模式未返回 DOM，换用本机 Chromium 的 Chrome 接口运行 `verify-refresh-stability.js --assert`，全部断言通过。

---

## Phase 3：可维护性深度重构（领域模块化 + 消除隐式耦合）

> **依赖关系**：建议在 Phase 1 & Phase 2 稳定合入后再逐卡推进。`R3-1`、`R3-2`、`R3-3`、`R3-4` 可按顺序逐卡合入。

### 任务卡 `R3-1`：`app/core/src` 领域目录化（`store/`、`inject/`、`providers/`）与大文件清零

**Codex 实施拆分（2026-11-25）**。第一步按现有作用域搬迁源码与测试并保留公开入口。第二步复用客户端发现、进程提示和备份校验。第三步核对用例清单与读取 Rust 源码的门禁。

- **目标**：解决 `app/core/src/` 根层平铺 45+ 文件的混乱，将 [`build/check-file-size.js`](../../build/check-file-size.js) 预算表中剩余的 Rust 超标大文件（`inject.rs`、`store.rs`、`workbuddy.rs`、`diaglog.rs`、`checkin.rs`、`providers.rs`）全部降至 `<= 1,500` 行软上限以下并移出超限预算表。
- **涉及文件**：
  - `app/core/src/store.rs` → `app/core/src/store/{mod.rs, schema.rs, snapshots.rs, vault.rs, checkin.rs, gateway.rs, tests.rs}`
  - `app/core/src/inject*.rs`（13 个文件）→ `app/core/src/inject/{mod.rs, types.rs, process.rs, io.rs, stash.rs, discover.rs, drift.rs, source.rs, gateway.rs, cli.rs, codex.rs, workbuddy.rs, zcode.rs, trae.rs, dsh.rs, remove.rs, tests.rs, tests_trae_dsh.rs}`
  - `app/core/src/workbuddy.rs`、`diaglog.rs`、`checkin.rs`、`providers.rs` 按职责拆出子模块（例如 `diaglog/{mod.rs, redact.rs, ring.rs}`、`workbuddy/{mod.rs, locate.rs, crypto.rs}`）
  - [`build/check-file-size.js`](../../build/check-file-size.js)
- **具体需求**：
  1. **纯搬家优先，零行为变化**：所有对外模块路径（`tokenscope_core::store::*`、`tokenscope_core::inject::*` 等）通过 `mod.rs` 的 `pub use` 100% 保持原签名，保证 `src-tauri` 与外部测试零破坏；
  2. 在 `inject/` 归拢完成后，统一抽象 `ClientAdapter` 公共步骤（discover → process snapshot check → backup/stash → preview → apply/remove → verify），将模型直写与网关接入中重复的备份/回滚/进程守卫逻辑收敛到共用辅助层；
  3. 同步更新 [`build/check-file-size.js`](../../build/check-file-size.js)，删除已降到 `<= 1,500` 行的 Rust 预算项，棘轮只下调不上调。
  4. **注意**：`build/` 下的前端回归脚本（如 `thumbnail-regression.js`）会直接读取 `providers.rs` / `checkin.rs` 源码文本，拆分前先全仓 grep `build/` 对这些文件名的路径引用并同步更新。
- **验收标准 & 双向验证**：
  - `cargo test --locked -- --list` 归一化测试名后用例数不少于基线，全部通过；
  - `node build/check-file-size.js --self-test && node build/check-file-size.js` PASS 且 Rust 生产文件超限预算表归零；
  - 全套前端冒烟（含读取 Rust 源码的回归脚本）全绿。

**Codex 完成记录（2026-11-25）**

- 核心存储、写入客户端、渠道定义、签到、WorkBuddy 和诊断日志按职责分目录。保留原公开入口与 `store_gateway` 重导出。签到和缩略布局的源码夹具读取新的表路径。
- `inject/adapter.rs` 由模型直写与网关接入复用。它统一发现、运行警告和可读性校验，并让 Trae 的写入与取消共用备份、校验和回滚入口。
- 文件规模预算表已移除全部 Rust 超限项。标准串行门禁通过。将搬迁后的台账测试路径归一化后，拆分前后的用例清单一致。

---

### 任务卡 `R3-2`：`Fetcher` 状态收敛（14 锁 → 3 组）与渠道后置钩子统一

- **目标**：消除 [`app/core/src/fetch.rs`](../../app/core/src/fetch.rs) 中 14 把细粒度 `Mutex` 导致的认知负担，并彻底合并「主轮询」与「`fetch_channel_scoped`（测试连接）」两份重复维护的 Codex / MiniMax / Qoder 后置补齐逻辑。
- **涉及文件**：
  - [`app/core/src/fetch.rs`](../../app/core/src/fetch.rs)
  - [`app/core/src/fetch_once.rs`](../../app/core/src/fetch_once.rs)
  - [`app/core/src/fetch_creds.rs`](../../app/core/src/fetch_creds.rs)
  - [`app/core/src/qoderweb.rs`](../../app/core/src/qoderweb.rs)
- **具体需求**：
  1. **状态聚合**：
     - 将 `cache`、`scopes`、`recovery`、`url_memo`、`rejected`、`adopted`、`accounts_cache` 聚合进 `Mutex<ChannelState>`（单锁操作 `cached()` 不再连续加 4 把锁，杜绝锁序问题）；
     - 将 `qoder_packs` 与 `qoder_memos` 收拢为 `qoderweb::QoderWebState` 结构体由 `qoderweb` 模块自管理；
     - `Fetcher` 仅保留：`http`、`gate`、`state: Mutex<ChannelState>`、`qoder_web: Mutex<QoderWebState>`、`progress`、`progress_sink`、`last_run`、`cfg_cache`、`web_cred_src`。
  2. **后置补齐钩子统一**：
     - 提取统一异步函数 `enrich_fetched_result(ctx, def, cred, used_url, &mut result)`，承载 Codex `reset_credits`、MiniMax `merge_expiry`（以及按需执行的 Qoder 网页逐包）；
     - `fetch_selected`（主轮询）与 `fetch_channel_scoped`（探活/自由函数）统一调用该函数，删除重复分支。
- **验收标准 & 双向验证**：
  - 全部 `fetch::*` 单测（含锁中毒恢复测试 `poisoned_recovery_progress_and_cache_locks_recover_and_log_throttled_diag`）全绿；
  - 新增单测验证 `fetch_channel_scoped` 与主轮询对 Codex / MiniMax 产出结构一致；
  - [`build/check-lock-unwrap.js`](../../build/check-lock-unwrap.js) 0 违规。

**Codex 实现记录（2026-11-25）**

- 余额、作用域、重试、域名备忘、被拒凭据、认领和账本计数集中到 `ChannelState`。Qoder 逐包列表和端点备忘由 `qoderweb::QoderWebState` 管理。
- 主轮询的 `quota_attempt` 与探活的 `fetch_channel_scoped` 共用一次请求及 Codex、MiniMax 后置补齐。Qoder 网页补齐独立到 `fetch_enrich.rs`，继续使用仅轮询路径具备的账号、地区和网页登录状态。
- 标准串行门禁通过。专项用例覆盖聚合锁中毒恢复、Codex 明细回环和 MiniMax 无法补齐时保留有效余额。新增回环夹具对照两个入口的窗口、重置卡与逐笔积分完整结构，核对同账号的明细请求，单独绕过探活后置补齐时该用例报红。

---

### 任务卡 `R3-3`：削薄 `src-tauri` 壳层（拆分 `model_cmds.rs` + 下沉 `sync_cmds.rs` 纯逻辑）

**Codex 实施拆分（2026-11-25）**。先按职责拆分平铺的 Tauri 命令文件，再下沉同步协议与恢复筛选，最后核对命令契约及全目录文件规模。

- **目标**：让 `app/src-tauri/src/` 回归薄胶水层，将纯业务逻辑下沉到 `tokenscope-core` 获得秒级单元测试覆盖。
- **涉及文件**：
  - [`app/src-tauri/src/model_cmds.rs`](../../app/src-tauri/src/model_cmds.rs)（1,139 行）→ 平铺拆为 `model_cmds.rs`、`promo_cmds.rs`、`diag_log_cmds.rs`、`os_cmds.rs`（**必须平铺在 `app/src-tauri/src/` 一层，确保 `check-ipc-contract.js` 完整扫描**）
  - [`app/src-tauri/src/sync_cmds.rs`](../../app/src-tauri/src/sync_cmds.rs)（1,362 行）→ 纯 GitHub Device Flow 协议解析、状态机与同步预览组装下沉至 `app/core/src/sync/`
  - [`app/src-tauri/src/lib.rs`](../../app/src-tauri/src/lib.rs)（同步更新 `mod` 与 `generate_handler!` 模块前缀）
- **具体需求**：
  1. 对外 Tauri 命令名、参数名、返回 JSON 字段 **100% 不变**；
  2. 拆分后 `src-tauri/src/*.rs` 每个文件控制在 `<= 700` 行；
  3. 下沉至 `core/src/sync/` 的 Device Flow 状态机与恢复候选筛选逻辑补齐核心单元测试。
- **验收标准 & 双向验证**：
  - `node build/check-ipc-contract.js` PASS，唯一命令数与注册数与拆分前一致；
  - `app/core` 新增单测全绿，`app/src-tauri` `cargo check --all-targets --locked` 零警告；
  - `jsdom-smoke.js`（含优惠、同步、模型、诊断日志用例）全绿。

**Codex 完成记录（2026-11-25）**

- 模型目录、客户端接入、操作系统入口、优惠和排查日志各有平铺模块。另拆出启动、签到历史、节拍和网关联调，使桌面壳层每个 Rust 文件均满足本卡上限。
- Device Flow 协议解析、轮询结果、同步预览构建及恢复候选筛选下沉到 core。壳层继续管理凭据持久化、会话取消、配置和事件。
- 串行门禁通过。IPC 唯一命令数、定义数和注册数与拆分前相同。协议用例覆盖过期、轮询减速及越权 scope 拒绝，恢复用例验证当前组筛除和失败汇总。

---

### 任务卡 `R3-4`：前端渐进模块化（`gt-api.js` + `gt-store.js` + `styles.css` 拆分）

**Codex 实现记录（2026-11-25）**。IPC 迁入按业务域命名的字面量封装。七个共享状态使用 `GT_STORE` 兼容访问器，三张 CSS 按令牌、面板、管理面装载。验证脚本从 HTML 读取样式顺序，文件规模门移除了原单体样式预算。

**验证**。标准串行静态、jsdom、Rust 和凭据门通过。Chrome 无头同机前后对照分别通过 `verify-refresh-stability.js --assert`，比较输出的测量项一致。隔离的 Windows WebView2 在实际 125% DPI 下通过 18 组主题、布局和字号的同窗样式对照，采样颜色、字号、网格和几何一致，窗口均在工作区内。`verify-gateway-layout.js --self-test` 的超宽坏样本先报红，移除样本后通过 9 个网关布局组合。

- **目标**：在不引入 npm 打包器、不破坏现有 `jsdom` / `vm.Script` 门禁体系的前提下，将前端 436 个裸全局声明与散落的 `invoke` 调用逐步收拢到显式边界，并消除 [`app/src/styles.css`](../../app/src/styles.css)（1,737 行）超限预算。
- **涉及文件**：
  - 新增 `app/src/gt-api.js`、`app/src/gt-store.js`（命名不以 `app-` 开头以保持 `app-*.js` 分组纯洁性，并在 `index.html` 中按加载序排在 `app-core.js` 之前）
  - [`app/src/index.html`](../../app/src/index.html)
  - [`app/src/styles.css`](../../app/src/styles.css) → 拆为 `styles-tokens.css`（设计令牌与深浅主题）、`styles-panel.css`（四形态与渠道列表/弹层）、`styles-manage.css`（设置与子页）
  - [`build/check-css-forms.js`](../../build/check-css-forms.js)、[`build/check-file-size.js`](../../build/check-file-size.js)、[`build/lib/app-sources.js`](../../build/lib/app-sources.js)
- **具体需求**：
  1. **`gt-api.js` 契约集中层**：
     - 将散落在 20 个脚本中的 `invoke(...)` 按域收口到 `window.GT_API = { channels: { ... }, config: { ... }, vault: { ... }, gateway: { ... }, sync: { ... }, ... }`；
     - **硬约束**：每个封装方法内部**必须写死字符串字面量**（如 `getConfig: () => invoke("get_config")`），绝不可写动态 `invoke(name, args)`，保证 `check-ipc-contract.js` 100% 静态可见；
     - 统一错误格式化辅助，不改变任何既有写操作失败回滚 + `showToast` 行为。
  2. **`gt-store.js` 状态单源层**：
     - 集中管理 `channels`、`homeView`、`config`、`windowVisible`、`activeTab`、`dirtyTabs`、`detailId`，提供 `getState()` / `setState(patch)` / `subscribe(slice, listener)`；
     - 通过兼容访问器或同步镜像逐步替代 `app-*.js` 中直接写入的裸全局，降低单次改动风险。
  3. **`styles.css` 拆分**：
     - 按「令牌 → 展示面与四形态 → 设置管理面」拆成 3 个文件并按序引入 `index.html`，确认层叠优先级（尤其是 V2 令牌段和浅色主题覆盖段）逐条等价；
     - 确认 [`build/check-css-forms.js`](../../build/check-css-forms.js) 扫描到全部新 CSS 文件并跑通 `--self-test`；
     - 从 [`build/check-file-size.js`](../../build/check-file-size.js) 的 `BUDGETS` 中移除 `app/src/styles.css` 超限预算。
- **验收标准 & 双向验证**：
  - `check-ipc-contract.js`、`check-css-forms.js --self-test`、`check-app-split.js --self-test`、`check-file-size.js --self-test`、`check-silent-failures.js --self-test` 全部 PASS；
  - 全部前端冒烟（`jsdom-smoke.js`、`verify-vault.js`、`verify-codex-reset.js`、`verify-google-cloud.js`、`verify-antigravity-capture.js`、`verify-gateway.js`、`verify-gateway-magpie.js`）全绿；
  - `build/verify-gateway-layout.js` 与原生 CDP 检查（深浅主题 × 三种布局 × 三档字号）零视觉回归。

---

## Phase 4（可选专项 · 极致降内存）：托盘省内存模式（隐藏态释放 WebView2）

> **说明**：这是唯一能把托盘常驻空闲内存从 ~430 MB 压至 ~50–60 MB 的重构。建议作为独立可选阶段，放在 Phase 1–3 之后实施。

### 任务卡 `R4-1`：将托盘数字图标绘制从前端 Canvas 迁至 Rust 原生位图渲染

- **目标**：解耦「托盘数字轮播」对 WebView2 前端 Canvas 的强依赖，为托盘隐藏态销毁 WebView2 铺路，同时彻底消除隐藏态下定时 Canvas 渲染 → Base64 编码 → IPC → 解码的开销。
- **涉及文件**：
  - 新增 `app/core/src/tray_icon.rs`（纯像素渲染，零外部图像库依赖）
  - [`app/src-tauri/src/window_cmds.rs`](../../app/src-tauri/src/window_cmds.rs)、[`app/src-tauri/src/poll.rs`](../../app/src-tauri/src/poll.rs)、[`app/src-tauri/src/lib.rs`](../../app/src-tauri/src/lib.rs)
  - [`app/src/app-tray.js`](../../app/src/app-tray.js)（保留胶囊面展示与轮播，去除或降级前端 Canvas 推托盘路径）
- **具体需求**：
  1. 在 `core/src/tray_icon.rs` 实现无外部依赖的 `render_badge_rgba(size: u32, text: &str, tone: Tone) -> Vec<u8>`：
     - 支持 16–64px 物理尺寸（按任务栏 DPI 实时取得，复用 `tray_icon_size` 逻辑）；
     - 内置紧凑点阵字形（`0-9`、`.`、`k`、`M`、`%`、`-`、`?`），圆角状态色底（绿/黄/红/灰，色值与前端一致）+ 高对比度文字居中绘制；
     - 输出 `(size * size * 4)` 字节 RGBA，配合既有 `tray_icon_digest` 去重；
  2. 在 Rust 侧实现托盘轮播与 tooltip 生成（复用 `view::build_home` 与 `Config.pill_channels` 同一份排序/挑选口径），托盘左键/菜单/tooltip 与原行为完全一致；
  3. 当 `Config.tray_number == false` 时还原品牌图标。
- **验收标准 & 双向验证**：
  - `core/src/tray_icon.rs` 单测覆盖 16/20/24/32/48px 尺寸边界、非法尺寸拦截、不同数字/状态色产出不同 digest、透明圆角与非空前景像素断言；
  - 真机验证托盘在 100% / 125% / 150% DPI 下数字清晰度与 tooltip 一致性。

**Codex 实现记录（2026-11-25）**。核心渲染输出 16–64px 原生 RGBA；Tauri 更新循环复用任务栏 DPI、按像素摘要去重，从取数结果和配置选择余额及提示，关闭数字时恢复品牌图标。前端保留可见胶囊轮播，移除 Canvas 渲染和三条托盘 IPC。Rust 托盘专项、完整串行门禁和凭据扫描通过。当前设备的任务栏 DPI 为 120（125%）。三档原生托盘清晰度、提示文本及休眠期间的数字轮播仍待完整实测。

---

### 任务卡 `R4-2`：可选「省内存模式」（隐藏到托盘空闲后释放 WebView2）

- **目标**：用户开启后，隐藏到托盘超过空闲阈值（如 5 分钟）自动销毁主 WebView2 窗口释放 ~350 MB 内存；点击托盘或热键时即时重建窗口。
- **涉及文件**：
  - [`app/core/src/config.rs`](../../app/core/src/config.rs)（新增 `#[serde(default)] pub hibernate_webview_on_hide: bool`，默认 `false`）
  - [`app/src-tauri/src/lib.rs`](../../app/src-tauri/src/lib.rs)、[`app/src-tauri/src/window_cmds.rs`](../../app/src-tauri/src/window_cmds.rs)
  - [`app/src/index.html`](../../app/src/index.html) / [`app/src/app-settings.js`](../../app/src/app-settings.js)（设置 → 刷新与启动 新增开关）
- **具体需求**：
  1. 依赖 `R4-1` 完成（确保没有前端时托盘数字、轮询、签到、红线通知、本地网关全部独立运行）；
  2. 窗口隐藏后启动可取消的 300s 定时器（用户期间唤回则立即取消）；超时后仅当 `hibernate_webview_on_hide == true` 且窗口仍隐藏且非胶囊/贴边可见态时调用 `main` 窗口 `destroy()`；
  3. **遵守 [`AGENTS.md`](../../AGENTS.md) §8.4 窗口重建陷阱**：Tauri `destroy()` 异步派发，重建同名 `main` 窗口前须等待旧标签彻底释放，恢复工作区尺寸、位置、置顶状态；
  4. 单实例回调、全局热键、托盘左键、托盘菜单「显示面板」统一走同一个 `ensure_and_show_main_window(app)` 入口；
  5. 窗口不存在时 `emit_channels` 等事件安全跳过，不报错；重建首屏立刻从 `Fetcher` 内存缓存 `last_run` 秒出数据（不白等网络）。
- **验收标准 & 双向验证**：
  - 真机验证：开启开关 → 隐藏等待触发释放 → 任务管理器核验 WebView2 渲染子进程退出、进程树内存降至 `< 80 MB`、本地网关与托盘数字继续更新 → 热键/托盘点击正常重建窗口且状态/位置/置顶完整；
  - 边界验证：隐藏后 10 秒内快速反复切换显隐 10 次，不出现同名窗口冲突或崩溃；关闭开关时行为与历史版本 100% 一致。

**Codex 实现记录（2026-11-25）**。设置开关默认关闭；保存成功才使后台开关生效。隐藏满 300 秒后检查可见性、网页登录依赖和登录窗口，再释放主窗口。唤回等待旧标签退出，由后台线程重建，在当前工作区恢复尺寸、位置、形态和置顶。首屏从 `Fetcher::last_views` 读取，迟到的缓存回包服从已收到的实时推送。托盘和网关继续接收后台结果。标准串行门禁已通过，隔离的 Windows 实例在 10 秒内通过 10 轮真实全局热键显隐。开启后隐藏满 300 秒，主窗口的 CDP 目标消失，WebView2 渲染进程退出，本地网关 `/v1/models` 继续应答。实例及其独立 WebView2 profile 的工作集由约 507 MiB 降到 48 MiB。热键约 1.7 秒唤回，坐标、625×1020 物理像素、置顶、显示形态和内存渠道数据均恢复。另加 Rust 回归，强制网络刷新保留首屏快照，凭据失效会清除快照。关闭开关并再次隐藏超过 300 秒后，原主窗口页面标记仍在，确认同一 WebView2 页面持续保留。隔离实例通过菜单对应的退出命令正常结束。

---

## 附：每张任务卡完成后的标准串行门禁清单

所有执行 Agent 在提交前必须**串行跑完**以下全部检查（`NO_PROXY=127.0.0.1,localhost,::1`），缺一不可：

```bash
# 1. 零依赖静态门（含双向自检）
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

# 2. 前端 jsdom 冒烟与专项验证
node build/jsdom-smoke.js
node build/verify-vault.js
node build/verify-codex-reset.js
node build/verify-google-cloud.js
node build/verify-antigravity-capture.js
node build/verify-gateway.js
node build/verify-gateway-magpie.js

# 3. Rust 单测与编译校对（单线程复核避免资源竞争假红）
(cd app/core && NO_PROXY=127.0.0.1,localhost,::1 cargo test --locked -- --test-threads=1 && cargo check --all-targets --locked)
(cd app/src-tauri && cargo check --all-targets --locked)

# 4. 凭据安全扫描
powershell -NoProfile -ExecutionPolicy Bypass -File build/scan-secrets.ps1
```
