# Grandettoken 架构重构验收报告（2026-11-25）

- **验收结论**：`CONDITIONAL PASS`（阶段 A、B 全绿；阶段 C 原生回归、V1–V4、数据库迁移与数据还原均通过；未跑项仅受本机 DPI 环境、冷启动取数与托盘菜单交互限制，另 B11 为已知过时断言）
- **基线对比**：`fc1c19d` (`v0.3.76`) → 当前工作区（任务卡 `R1-1` ~ `R4-2`）
- **验收环境**：Windows 11 专业版；显示缩放 **125%**（首轮 2560×1440、工作区 2560×1380；2026-11-26 修复复验时为 1920×1080、工作区 1920×1020，窗口按新工作区夹取）；Rust 1.98.1 / cargo 1.98.1；Node v24.16.0；WebView2（`tokenscope.exe` Release）；Chrome 无头用于 `verify-refresh-stability`
- **验收方式**：独立只读核验。未修改产品源码；所有门禁串行执行；临时探针仅在 `build/qa/refactor-acceptance-2026-11-25/` 与本机 `%TEMP%` 下使用，并在同一轮删除
- **原始证据**：`build/qa/refactor-acceptance-2026-11-25/`（已被 `.gitignore`，不入库）

## 1. 阶段 A 自动化门禁结果

| 门禁项 | 命令 | 实测结果（含用例数/耗时） | 状态 |
|---|---|---|---|
| IPC 契约 | `node build/check-ipc-contract.js` | 前端 111 invoke / 111 唯一命令；Rust 111 定义 / 111 注册；未定义 0、孤儿注册 0 | PASS |
| 静默失败（含自检） | `node build/check-silent-failures.js --self-test` + 正式 | 自检 6 类坏样本全部判红；正式 30 个脚本、121 个 catch 全部已处理或已认领 | PASS |
| 文件规模（含自检） | `node build/check-file-size.js --self-test` + 正式 | 自检 5 类；正式仅剩 `app/src/index.html` 600/698 行、`app/src/app*.js` 合计 8774/8780；Rust 超标清零 | PASS |
| CSS 形态（含自检） | `node build/check-css-forms.js --self-test` + 正式 | 自检 6 类；正式 10 个样式文件、7 个整块容器、1429 条规则全部受状态门控制 | PASS |
| UTF-8 编码（含自检） | `node build/check-encoding.js --self-test` + 正式 | 自检 8 类；正式 534 个受版本控制文本文件均合法 UTF-8、无 U+FFFD | PASS |
| app 拆分完整性（含自检） | `node build/check-app-split.js --self-test` + 正式 | 自检 6 类；正式 14 个脚本标签与磁盘一致、422 个顶层声明无重名、逐文件加载期零异常 | PASS |
| Rust 锁中毒棘轮（含自检） | `node build/check-lock-unwrap.js --self-test` + 正式 | 自检 6 类；正式 150 个生产源码文件、0 处锁 `unwrap/expect` | PASS |
| 临期/套餐计算 | `node build/verify-expiry.js`、`verify-qoder-expiry.js --self-test`、`verify-quota-plan.js` | 上海/纽约两时区、账户到期、Qoder 明细、三渠道并行与短窗全部通过；坏样本均判红 | PASS |
| 前端语法 | `node --check` 覆盖 `app/src/*.js` 与 `google_cloud_capture.js` | 0 个失败；`gt-api.js`、`gt-store.js`、`gt-window-state.js` 均实际覆盖 | PASS |
| `jsdom` 冒烟 | `node build/jsdom-smoke.js` | **317/317** 通过 | PASS |
| 9 项反向哨兵 | 逐个单独运行 `jsdom-smoke.js` | 9/9 均以退出码 1 结束，命中对应功能 `FAIL` 行；日志无 `TypeError`/`ReferenceError` 遮盖 | PASS |
| 专项前端验证 | `verify-vault` / `verify-codex-reset` / `verify-google-cloud` / `verify-antigravity-capture` / `verify-gateway` / `verify-gateway-magpie` | 6/6 通过（密钥箱、重置顾问、Google Cloud、Antigravity、网关、网关接入） | PASS |
| 刷新稳定性 | `node build/verify-refresh-stability.js --assert` | 20 个场景；`#list` 推进 1 分钟 `childListCount=0`，节点身份全部保留 | PASS |
| core 单测 | `cargo test --locked -- --test-threads=1` | **700 passed / 0 failed / 10 ignored**；耗时 78.6s（串行） | PASS |
| 同步夹具 | `cargo test --locked --test sync_fixtures -- --test-threads=1` | 6 passed / 0 failed | PASS |
| core 全目标编译 | `cargo check --all-targets --locked` | exit 0，**0 warning** | PASS |
| 桌面全目标编译 | `cargo check --all-targets --locked`（`app/src-tauri`） | exit 0，**0 warning** | PASS |
| 锁定依赖 | `cargo fetch --locked --manifest-path app/src-tauri/Cargo.toml` | exit 0 | PASS |
| 凭据扫描 | `build/scan-secrets.ps1` | 扫描 628 个文件，跳过 13 个已知测试夹具，`CLEAN` | CLEAN |
| 本机路径残留 | `git diff` 受控文件 + 新增文档 | 仅产品标识（`com.wacil.tokenscope`、`C:\Program Files` 示例）与验收说明中的 `%APPDATA%`；无本机绝对路径、无明文凭据 | PASS |

**A1 额外核对**

- IPC 命令集相对基线 `fc1c19d`：基线注册 113 个，当前 111 个；语义删除恰好为 R4-1 移除的 3 个托盘绘图命令（`set_tray_icon` / `set_tray_tooltip` / `tray_icon_size`），新增 1 个 `get_cached_channels`；其余 22 项仅随模块搬迁改名，无遗漏或孤儿注册。
- `BUDGETS` 已无 Rust 条目；`GROUP_BUDGETS` 仅 `app/src/app*.js <= 8780`；本轮新增/拆分文件最大为 `diaglog/mod.rs` 1352 行，均 `<= 1500`。

## 2. 阶段 B 逐卡核验结论（R1-1 ~ R4-2）

| 卡号 | 源码核验要点 | 反向测试结果 | 结论 | 备注 |
|---|---|---|---|---|
| R1-1 | `WANT=9`；同事务建 `idx_snap_fp_cover`、删 `idx_snap_fp`、保留 `idx_snap_lookup`；无自动 `VACUUM` | `store::tests` 41 项全通过；`EXPLAIN QUERY PLAN` 命中覆盖索引且无 `SCAN snapshots`；去掉覆盖索引后 4 条断言判红；真实库副本迁移行数 100% 一致 | PASS | 迁移后空间进入 freelist，文件不自动收缩（符合“不引入 VACUUM”） |
| R1-2 | 启用 `Win32_System_Diagnostics_ToolHelp`；`CreateToolhelp32Snapshot` + RAII `CloseHandle`；`sites()/state()/drift` 单次抓拍共享；无 `tasklist` 生产调用；保留 `opencode-cli.exe` / `opencode-pty.exe` / Trae 全名 | `native_snapshot_finds_running_test_process`、`snapshot_matches_full_names_without_ascii_case` 通过；本机实测 10 次 P50 **14.9 ms**、P95/Max **15.5 ms**；旧 `tasklist` 8 次串行 8.20–8.40 s | PASS（性能未达预期） | 计划 `<5 ms` 未达到，但单次仍比旧 `tasklist` 单次约 1.03 s 快约 **69×**；开发期 13.9–15.5 ms 可复现 |
| R1-3 | SHA-256 摘要鉴权缓存且轮换失效；Vault 投影随 `vault_revision`；`PricingLookup` 随 `model_revision`；`ProxySnapshot` 5s TTL；有界 `mpsc(256)` 台账队列 + `flush/clear` + 无 runtime 同步回退 | `ten_requests_reuse_access_digest_and_rotation_rejects_the_previous_token`、`repeated_projection_reads_load_once_until_the_committed_revision_changes`、`proxy_changes_refresh_the_client_and_direct_groups_bypass_it`、`group_pre_first_byte_failover_sets_cooldown_headers_and_ledger`、`streamed_request_records_inbound_user_agent_in_ledger` 均通过 | PASS | 队列满时丢弃并写诊断；停服/退出走 `flush` |
| R1-4 | 共享 `default` / `direct` HTTP 池；MiniMax/WorkBuddy 复用常驻单线程 runtime；`GistClient` 复用池并按请求传 `Authorization` | `shared_pool_keeps_authorization_scoped_to_each_request` 通过；全仓 `reqwest::Client::builder` 复核：生产侧仅剩池自身、代理客户端与 `Fetcher::new` 构造器，无业务函数内逐次新建 | PASS | `Fetcher` 保留独立 20s 超时/UA 客户端，属主轮询专用 |
| R2-1 | `Store::open_readonly` 使用 `SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only=ON`；`read_store` 贯穿历史、视图、网关账本聚合、渠道变更、签到历史；`prune`/`wal_checkpoint` 下沉 `spawn_blocking` | `wal_reader_sees_committed_history_and_ledger_during_an_uncommitted_write` 通过：只读连接 `record_fp` 必返回 `Err`，写事务未提交时读到旧值、提交后读到新值；`cargo check` 0 warning | PASS | 读写锁未嵌套；先写后读顺序由该夹具覆盖 |
| R2-2 | 非 `ch`/`disp` 子页只标脏 `GT_STORE.dirtyTabs`；切回立即补绘；`fitCompact` 非紧凑 O(1) 早退；`tickPeakCountdowns` 只扫 `#list` 与打开的 `#detailBody` | `TAB_DIRTY_SENTINEL` 判红且命中对应断言；`verify-refresh-stability --assert` 全绿；原生 V1 实测刷新后切页补绘正常 | PASS | 原生设置为深/浅、3 布局 × 3 字号 |
| R3-1 | 目录化 `store/ inject/ providers/ checkin/ diaglog/ workbuddy/`；`inject/adapter.rs` 统一发现/预览/在跑提示/提交回滚；旧散落文件清零；测试夹具改新路径 | 旧 `inject_*.rs`、`store_connection.rs`、`store_vault.rs` 均不存在；`thumbnail-regression.js` 已读 `providers/catalog.rs`；基线 704 → 当前 717 项，唯一消失项为 `tasklist_matching_is_locale_independent`（被 2 项 Win32 快照用例替代），无意外丢测 | PASS | 770 个用例名归一化比对完成 |
| R3-2 | `Fetcher` 锁由 15 把降为 8 个内聚状态（`ChannelState` 合并 6 项、`QoderWebState` 合并 2 项）；`fetch_enrich.rs` 由轮询与探活共用 | `poisoned_recovery_progress_and_cache_locks_recover_and_log_throttled_diag`、`polling_and_probe_return_the_same_codex_and_minimax_enrichment`、`optional_enrichment_preserves_balance_when_details_are_unavailable` 通过 | PASS | 两个入口共用同一回环上游的联合夹具已具备 |
| R3-3 | `src-tauri/src` 单层平铺；壳层文件均 `<= 700` 行；Device Flow 状态机与恢复筛选在 `core/src/sync/` 有单测；IPC 语义无丢失 | 壳层最大 `weblogin_cmds.rs` 632 行；`session_and_poll_results_preserve_protocol_bounds`、`candidates_exclude_the_live_group_and_report_unverified_items` 通过；IPC 111/111 | PASS | 命令注册集仅按 R4-1 增减 |
| R3-4 | `gt-api.js` 全部 `invoke` 为静态字符串；样式按 `styles-tokens → styles-panel → styles-manage → 功能 CSS` 加载；`check-css-forms` 覆盖 3 个拆分 CSS；`build/lib/app-sources.js` 完整加载新脚本 | 111 个 `invoke` 全为字面量；`index.html` 10 个样式链接顺序正确；`probe-frontend.js` exit 0；`check-app-split` 卡住加载顺序 | PASS | 实际为 14 个 `app-*.js` + 3 个引导脚本 + 13 个功能脚本，全部纳入门禁 |
| R4-1 | `tray_icon.rs` 零外部图像依赖输出 RGBA；前端不再 Canvas→Base64→IPC；`tray.rs` 轮播、tooltip、`tray_show_number=false` 还原品牌图标 | `badge_sizes_colors_corners_and_glyphs` 覆盖 16/20/24/32/48/64 px 与 0/15/65/u32::MAX 拦截；`rotation_falls_back_from_hidden_and_badge_preserves_a_stale_value` 通过；真实 125% 下数字清晰、轮播与品牌图标切换实测通过 | PASS | 100%/150% DPI 未测，见 §5 |
| R4-2 | `hibernate_webview_on_hide` 默认 `false`；隐藏满 300s 才释放主 WebView；存在网页登录会话时保留；唤回等旧标签释放后重建并恢复几何/置顶/缓存 | 源码核验通过；真实生命周期见 §4 V4 | PASS | 真机 300s 对照组与开启组均实测 |

## 3. 量化指标实测对比

| 指标 | 基线 / 预期 | 本次实测 | 结论 |
|---|---|---|---|
| `ProcessSnapshot::capture()` | 计划 `<5 ms`（开发测 14.4 ms） | 10 次：P50 **14.9 ms**、P95/Max **15.5 ms**、Mean 14.9 ms | 未达 5 ms；仍远优于旧路径 |
| 旧 `tasklist` 8 次串行 | — | **8.20 s / 8.40 s**（两次），单次约 1.03 s | 新路径单次约快 69× |
| 真实库副本 v8→v9 | 行数 100% 一致、索引替换、`integrity_check=ok` | 517,502 → 517,502；`idx_snap_fp` 消失、`idx_snap_fp_cover`/`idx_snap_lookup` 保留；`integrity_check=ok`；`user_version=9`；91.99 MB → 91.99 MB；`freelist_count` 0 → 4,682 页 | PASS；空间进入 freelist，不自动 VACUUM |
| Release 单 exe | 单 exe 交付、无异常膨胀 | 首次 11.87 MB、修复后最终 **11.32 MB**；上一安装版 11.13 MB（最终 +1.7%）；两次构建 exit 0 | PASS |
| 省内存模式休眠前/后进程树工作集 | `< 80 MiB` | 修复前真实配置：1233.3 MiB → **1233.3 MiB（未释放）**；修复后：466.2 MiB → **52.6 MiB（0 WebView2）** | PASS（修复后） |
| 主窗口唤回耗时 | `< 1 s`（开发参考 1.7 s） | 修复后销毁重建 **637 ms**；会话保留态 370/387/406 ms | PASS |

## 4. 阶段 C 真机验收记录与用户数据还原校验

### C1 Release 构建

- 首次 `.\build\build.ps1 build`：exit 0，耗时 **332.5 s**，产物 `target/release/tokenscope.exe`（11.87 MB）；修复 R4-2 后重建：exit 0，耗时 **267.5 s**，产物 **11.32 MB**（最终交付产物）。
- 两次构建末尾均出现 1 条链接器提示 `corrupt .drectve at end of def file`（bin `tokenscope` 生成 1 warning）；`cargo check --all-targets --locked` 为 **0 warning**，该提示不在源码检查口径内，记录为观察项而非本次回归。

### C2 原生 CDP 回归（`build/run-native-checks.ps1`）

- `-SelfTest`：PASS（不可达端口、运行实例拒绝、SHA-256 备份/还原/篡改检测、串行 PASS/FAIL 退出码）。
- 正式运行（端口 9241，Release 新 exe；修复 R4-2 后用最终构建复跑，结果相同）：

| 脚本 | 状态 | 说明 |
|---|---|---|
| `verify-settings-entry.js` | PASS | 设置入口收敛、9 个页签、三档字号零溢出 |
| `verify-checkin-center.js` | PASS | — |
| `verify-resize.js` | PASS | 单列/双列/缩略、大/特大字号、贴边与工作区 |
| `verify-qoder-expiry.js` | PASS | — |
| `verify-ui-0923.js` | FAIL | 仅命中 B11 已知过时断言：U2 失联副标题文案、U6 官方标内嵌字母（2 个渠道）、U6 紧凑条字母 |
| `verify-focus-ring.js` | PASS | — |
| `verify-form.js` | FAIL | 仅命中 B11 已知项：compact 尺寸期望 450×73（实际 572×964）、`win-style.ps1` 按标题找不到窗口 |
| `verify-gateway-layout.js` | PASS | — |

- 判读：两项失败逐条比对 `docs/report/acceptance-backlog.md` B11（U2 文案、U6 官方标数、U6 紧凑条字母、compact 尺寸、`win-style.ps1` 标题），**无新增失败项**；全部日志无加载期 JS 报错、无 `ReferenceError`、无未捕获 Promise 异常。

### C3 真机专项

| 编号 | 结果 | 实测记录 |
|---|---|---|
| **V1** | PASS | 原生 125% 下：深/浅主题各 1 次；单列/双列/缩略 × 标准/大/特大 共 9 组，均无横向越界且列表可见；设置 9 个子页逐个切换均有内容；在网关页刷新后 `ch`/`disp` 被标脏，切回后卡片 15 个、显示选择项非空且脏标记清零；另由 `verify-resize`/`verify-settings-entry` 覆盖窗口几何与底栏 |
| **V2** | PASS（热缓存口径） | 模型页与网关“接入与安全”均正常打开；热缓存下模型页 **13 ms**、接入页 **2 ms**（首次打开也在 5 s 守护内完成），未见 1 s 卡顿或黑框；未运行客户端 `trae` 预览无 `runningWarning`，在跑客户端 `workbuddy` 预览带 `runningWarning`；预览前后 11 个客户端配置文件 SHA-256 全部一致，未误写 |
| **V3** | PASS（125% 实机）/ 部分 SKIP | 125% 下托盘“32”数字清晰无裁边；轮播实测 `OpenCode Go 95.0%` → `Codex 32.0%`，tooltip 与图标同步；托盘左键可隐藏/显示主窗口；关闭“显示余额数字”后立即还原品牌图标（截图对比）；100%/150% 未测（本机固定 125%）；黄/红/灰状态色未在真实低余额数据上强制复现（单测覆盖） |
| **V4** | PASS（修复后） | **关闭态对照**：隐藏 310 s 后 WebView2 仍 13 个进程、进程树 1308.9 MiB，未退出；**修复前真实配置**（Google Cloud 已连接）：隐藏 310 s 后仍 13 进程、1233.3 MiB（见 A-6）；**修复后真实配置**：隐藏 310 s → **0 个 WebView2 进程、进程树 52.6 MiB**，网关 `/healthz` 200、快照时间戳继续推进（1795675980 → 1795676252）、托盘 tooltip 轮播到 `Codex 26.0%`，热键唤回 **637 ms**，11 行缓存首屏恢复；**防抖**：10/10 次快速隐藏/热键唤回均可见，无崩溃、无 `startup-error.log`；Google Cloud 后台取数在修复后仍成功写入 2 条赠金、无错误 |

- **用户数据还原**：C2 首次运行前后与最终复跑后，`tokenscope.db` / `-shm` / `-wal` SHA-256 全部 `MATCH`；C3 结束及修复复验结束后，按 `%TEMP%\gt-c3-backup` 逐文件还原，最终三个文件均 `MATCH`（`.db` 与 `-wal` 逐字节一致；`.shm` 在无进程占用时恢复后同样一致）。

### C3 修复复核（2026-11-26，R4-2 / A-6）

- **缺陷**：真实配置（Google Cloud 已连接）下开启省内存模式并隐藏 310 s，主 WebView 不释放。根因是后台 Google Cloud 赠金抓取窗口在刷新完成后只 `hide()` 不 `destroy()`，而 `needs_webview()` 把任何非 `main` webview 都视为必须保留；即使临时隐藏 workbuddy/doubao/qoder/qoder-cn 仍不释放。
- **修复**：① 非交互式后台抓取完成或失败后销毁 `google-cloud-credits` 窗口（交互式登录仍 `hide()`，其持久 profile 保留会话）；② `needs_webview()` 对隐藏的 Google Cloud 后台窗口放行，可见的登录窗口仍保留；③ 释放主窗口前先销毁隐藏的 Google Cloud 后台窗口；④ 调度器按需重建，Google Cloud 取数不受影响（复验写入 2 条赠金、无错误）。
- **复验**：同一真实配置下隐藏 310 s → **0 WebView2、52.6 MiB**，网关 `/healthz` 200、快照时间戳推进、托盘轮播继续、热键唤回 637 ms、缓存首屏恢复；`cargo check` 0 warning，core 单测 700 + 夹具 6 全通过，静态门/jsdom/专项/凭据扫描复跑全绿。显示分辨率在复验时由 2560×1440 变为 1920×1080（125%），窗口按新工作区夹取为 715×1020 @1205,0；同分辨率下的几何精确还原已在修复前隔离复验（1137,0 / 715×1204）。

## 5. 缺陷清单与未跑项

| 编号 | 级别 | 关联任务卡 | 复现步骤与现象 | 建议动作 |
|---|---|---|---|---|
| A-1 | P2 | R1-2 | 本机 10 次 `ProcessSnapshot::capture()` P50 14.9 ms、P95 15.5 ms，未达到计划 `<5 ms`；功能与稳定性无异常，且比旧 `tasklist` 快约 69× | 保持现状；后续如需进一步降耗再评估 Win32 快照的分配/去重优化 |
| A-2 | 观察 | C1 | Release 链接阶段 1 条 `corrupt .drectve at end of def file` 提示；`cargo check` 与单测均 0 warning | 与 GNU 工具链/链接器有关，记录不改代码；CI 若复现再定位 |
| A-3 | 未跑（环境） | R4-1 / V3 | 本机固定 125%，未实测 100% 与 150% 下托盘数字清晰度 | 已在 `acceptance-backlog.md` 登记，换机或切换缩放后补测 |
| A-4 | 未跑（环境） | V2 | 模型/接入页耗时为热缓存（13 ms / 2 ms）；首次冷启动耗时未单独取数 | 已在 backlog 登记；下次冷启动补记 |
| A-5 | 未跑（交互） | V4 | 托盘菜单“显示面板”与全局热键/托盘左键同走 `ensure` 重建路径；本次实测左键与热键，菜单项未单独点击 | 已在 backlog 登记；不影响重建逻辑本身 |
| **A-6** | **P1（已修复并复验）** | **R4-2** | 修复前：真实配置（Google Cloud 已连接）下开启省内存模式并隐藏 310 s，主 WebView 不释放（13 进程、1233.3 MiB）；`needs_webview()` 被常驻隐藏窗口 `google-cloud-credits` 触发，该窗口刷新后只 `hide()` 不 `destroy()` | 已修复：后台抓取完成/失败后销毁该窗口，`needs_webview()` 仅对可见登录窗口保留，释放主窗口前销毁隐藏后台窗口。复验：310 s → 0 WebView2、52.6 MiB，Google Cloud 取数仍成功（见 §4 C3 修复复核） |
| B11 | 已知 | 原生脚本 | `verify-ui-0923.js` / `verify-form.js` 仍仅命中 B11 的 5 类过时断言 | 维持 B11“未修复”状态，另行重写断言 |

## 6. 结论

阶段 A 全部门禁与 9 项哨兵通过；阶段 B 12 张卡源码与反向测试全部通过；阶段 C 原生回归除 B11 已知过时断言外全绿，V1/V2/V3（125%）、V4（修复后真实配置：0 WebView2 / 52.6 MiB / 637 ms 唤回）与数据库迁移演练通过，用户数据 SHA-256 最终 `MATCH`。未跑项仅本机 DPI、冷启动取数与托盘菜单入口，已登记 `acceptance-backlog.md`，故判定 **CONDITIONAL PASS**，可进入发版流程。

> **修复记录**：本轮验收发现并修复 R4-2 的 Google Cloud 后台窗口阻塞休眠问题（A-6，P1）；修复后已重建 Release、复跑 V4 及全部受影响门禁，详见 §4「C3 修复复核」。
