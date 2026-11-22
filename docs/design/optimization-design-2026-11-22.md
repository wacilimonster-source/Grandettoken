# Grandettoken 优化设计文档(执行版)

> **日期**:2026-11-22 · **基线**:桌面 v0.3.70 / Android 0.1.14 + 当天工作区未提交改动(见 §1.3)
> **读者**:接手执行的 agent。本文给出问题、方案、技术路线、任务卡和测试路径,每张任务卡都可以单独领取。
> **与其它文档的关系**:
> - 取代当天的「优化路线图」草稿(它只在会话工作区里,不入库);
> - `app.js` 拆分的完整细节以 [`docs/research/appjs-split-plan-2026-11-22.md`](../research/appjs-split-plan-2026-11-22.md) 为准,本文只做摘要和排期;
> - 硬约束以根目录 `AGENTS.md` 为准,用户裁决以 `DESIGN.md` §16 为准。本文与它们冲突时,以它们为准。
>
> **数字口径**:行号、行数、计数都是 2026-11-22 对源码的静态扫描快照,随代码变化会失效。**动手前必须重新 grep 定位**,不要按本文的行号直接改。

---

## 目录

- §0 执行总则(每个 agent 必读)
- §1 问题总览
- §2 结构类问题与方案(A1–A6)
- §3 性能类问题与方案(B1–B6)
- §4 稳定性问题与方案(C1–C2)
- §5 验收与流程问题与方案(D1–D4)
- §6 任务卡
- §7 测试路径
- §8 排期、并行与文件独占
- §9 需要用户拍板的事项
- 附录 A 证据索引 · 附录 B 已完成 / 已排除

---

## §0 执行总则(每个 agent 必读)

1. **先读 `AGENTS.md` 全文**,尤其 §2.1(双向验证)、§2.3(改代码)、§4.1(门禁清单)、§6(验证不缜密的七种形态)、§7(棘轮)。
2. **一张任务卡一个提交**(卡里写明可以拆成多个提交的除外)。提交只包含这张卡的「独占文件」以及 `PROGRESS.md`、`docs/report/acceptance-backlog.md` 的追加内容。**不要用 `git add -A`**,工作区里可能有别的 agent 尚未提交的改动。
3. **搬家与改行为分开**:标为「纯搬家」的任务,diff 里除了移动、可见性(`pub(crate)`)和 `use` 语句,不能有任何其它变化。卡里要求的「搬家等价性校验」必须跑。
4. **门禁串行执行,结论单独复跑一次**(AGENTS.md §4.1、§6.7)。缺依赖跑不了的,如实写「未跑」。
5. **双向验证**:每个新门禁、新测试都要证明「坏样本会变红」。本文 §7.3 列出了每张卡需要新增的哨兵 / 自检。
6. **棘轮只紧不松**:`build/check-file-size.js` 的预算只许下调;拆完立即在同一提交里下调到新行数。
7. **不发版**:本文所有任务都不包含发版。是否发版、何时发版由用户决定。
8. **用户拍板项**(§9)没有得到明确答复之前,对应任务卡不得开工。
9. **涉及用户本机数据的操作**(数据库、凭据、真实账号)必须先征得用户同意,只在副本上做。
10. 完成后在 `PROGRESS.md` 文末追加一条记录(做了什么、验证了什么、什么没跑),新增的待验收项写进 `docs/report/acceptance-backlog.md`。

---

## §1 问题总览

### 1.1 问题清单

「状态」一栏:**待办** = 未开始;**方案已定** = 有详细方案待执行;**需拍板** = 等用户决定。

| ID | 问题 | 严重度 | 关键证据(快照) | 状态 |
|---|---|---|---|---|
| **A1** | Rust 大文件里测试代码占比很高,但全部计入行数预算 | **高**(最先做) | `fetch.rs` 测试 1562 行、`inject.rs` 2199 行、`store.rs` 1047 行 | 待办 |
| **A2** | `app/src/app.js` 9014 行,预算余量 0 | **高** | `check-file-size.js` 预算 9014 | 方案已定(需拍板 5 项) |
| **A3** | `app/src-tauri/src/lib.rs` 6271 行,余量 1;62 个命令集中在一个文件 | **高** | 签到段 1585 行、模型段 1544 行、`run()` 425 行 | 待办 |
| **A4** | `fetch.rs` 生产代码约 3700 行,单函数过大 | 中 | `fetch_once_inner` 259 行、`build_view` 189 行 | 待办(A1 之后) |
| **A5** | `inject.rs` 生产代码约 5050 行,按客户端天然可分 | 中 | 11 个分段横幅,DSH 段约 1300 行 | 待办(A1 之后) |
| **A6** | 预算表里的其它文件(`store`/`checkin`/`providers`/`workbuddy`)贴着上限 | 低 | 余量 4–30 行 | 随 A1 解决 |
| **B1** | Google Cloud 后台任务每 5 秒醒一次,多数是空转 | 中低 | `google_cloud_web.rs` loop 末尾 `sleep(5s)`,刷新间隔 ≥ 60s | 待办 |
| **B2** | Codex 重置顾问每分钟重算:2 次 IPC + 界面闪「加载中」 | 中低 | `codex-reset.js` `inputKey` 含 `Math.floor(nowSec()/60)`;`loadAdvice` 先把 `advice` 置空再重取 | 待办 |
| **B3** | 渠道行的 HTML 签名里带分钟级时间文字,每分钟整行重新解析 | 中低 | `app.js` `snapAge` / `peakCountdown` / `expiryRelativeSpan` 写进行 HTML,`QSIG` 用 HTML 串当签名 | 待办(A2 之后) |
| **B4** | `fitPill` 每次渲染都做离屏测量,强制布局 | 低 | `app.js` `fitPill` 由 `renderPillFace` 每轮调用 | 待办(A2 之后) |
| **B5** | 每轮取数逐条独立提交 SQLite | 低 | `store.rs` `record_fp` / `Snapshot::save` / `prune_grants` 各自 `execute` | **需拍板**(原判高风险) |
| **B6** | `tokenscope.db` 持续增长,没有空间回收 | 中 | 11-16 实测 68.5 MB,约 +2.3 MB/天;`store.rs` 无 VACUUM | **需拍板**(动用户数据) |
| **C1** | core 生产代码有 43 处 `.lock().unwrap()`,锁中毒会让常驻进程 panic | 中 | 全部在 `fetch.rs`;`src-tauri` 已统一用 `lock_rw` 毒化恢复,core 没有对应工具 | 待办(A1 之后) |
| **C2** | Antigravity `--version` 探测超时后只杀直接子进程,然后无限期 `join` 读线程 | 低(待复现) | `antigravity.rs` `run_command_capped`:`kill()` + `wait()` 后 `h.join()` | 待办(先复现) |
| **D1** | 依赖原生窗口 CDP 的检查长期没跑,多个版本累积未验收 | 中 | 待验收清单 B1–B9 | 待办 |
| **D2** | 桌面签名私钥演练缺少「本地文件验签」入口 | 中 | `verify-signature.js` 只验线上 release | 待办 |
| **D3** | 待验收清单没有纳入发版流程 | 低 | `acceptance-backlog.md` 新建,`RELEASE.md` 未引用 | 待办 |
| **D4** | Android / 桌面签名材料没有可移植备份 | **高** | 部分材料只受本机 DPAPI 保护 | 文档已就绪,**需用户本人执行** |

### 1.2 对会话开头结论的更正

会话开头那一轮排查有三处不准确,以本文为准:

1. 「推送合并」和「隐藏态跳过重绘」**已在 v0.3.59 落地**(`app/core/src/progress.rs` 首个渠道立即推送、其余 150 ms 合批;前端 `WINDOW_VISIBLE` 门控),不是待办。
2. `fetch.rs` 的 `.lock().unwrap()` 是**生产代码 43 处 + 测试 7 处**,不是笼统的 50 处。
3. 当时说「四个大文件预算用满,必须拆分」,但**没有注意到 Rust 文件里测试代码占比很高**。把测试模块挪到旁边的文件(A1)风险极低、收益最大,应当排在所有拆分之前。

### 1.3 当天已完成、尚未提交的改动(先提交)

以下改动在 2026-11-22 已写进工作区,**还没在本机跑门禁、也没提交**。第一张任务卡 T-00 负责把它们验证后提交:

- `PROGRESS.md` 46 处 GBK 往返损坏复原、旧条目归档到 `docs/report/progress-archive-2026-11-22.md`;
- `app/core/src/fetch.rs` 一条测试断言消息、`docs/product/product-logging-observability-2026-10-21.md` 一处 U+FFFD 复原;
- 新门禁 `build/check-encoding.js` + `.github/workflows/text-encoding.yml`,并接入 `release.yml`;
- `RELEASE.md`:`cargo fetch` 口径、Android 签名备份与恢复演练;
- 新增 `docs/report/acceptance-backlog.md`;
- `AGENTS.md` 精简(原文存档 `docs/report/agents-md-archive-2026-11-22.md`),用户裁决表移到 `DESIGN.md` §16;
- `docs/research/appjs-split-plan-2026-11-22.md`(拆分方案,未执行);
- 本文件。

---

## §2 结构类问题与方案

### A1 Rust 测试模块外移(最先做,收益最大)

#### 现状

| 文件 | 物理行数 | `mod tests` 起点 | 测试行数 | 生产代码 | 预算 |
|---|---|---|---|---|---|
| `app/core/src/fetch.rs` | 5264 | L3703 | ~1562 | ~3702 | 5264(余量 0) |
| `app/core/src/inject.rs` | 7248 | L5050 | ~2199 | ~5049 | 7320 |
| `app/core/src/store.rs` | 2996 | L1950 | ~1047 | ~1949 | 3000 |
| `app/core/src/checkin.rs` | 2221 | L1516 | ~705 | ~1515 | 2242 |
| `app/core/src/providers.rs` | 2095 | L1503 | ~592 | ~1502 | 2095 |
| `app/core/src/workbuddy.rs` | 2392 | L1751 | ~623 | ~1769 | 2420 |

合计约 **6700 行测试**计入了生产文件的预算。

仓库里已有先例:`app/core/src/codex_reset.rs` 用 `#[path = "codex_reset_date_tests.rs"] mod date_tests;` 把测试放到旁边的文件;`app/core/src/gateway/` 用 `mod tests;` + `tests.rs`。

#### 方案

把每个文件末尾的

```rust
#[cfg(test)]
mod tests {
    use super::*;
    ...
}
```

替换为

```rust
#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
```

并把花括号内的内容原样移到 `fetch_tests.rs`。模块路径仍是 `fetch::tests`,`use super::*` 照常生效,子模块对父模块私有项的访问权限不变。

**选择 `#[path]` 而不是改成 `fetch/mod.rs` 目录结构**:与 `codex_reset.rs` 先例一致;`app/core/src/lib.rs` 里的 `pub mod fetch;` 不用动;Git 历史里 `fetch.rs` 仍然是同一个文件。

#### 技术细节与坑

1. **新文件同样受 1500 行软上限约束**(`check-file-size.js` 会递归扫描 `app/core/src`)。`fetch` 测试约 1562 行、`inject` 测试约 2199 行,超过上限,**必须各拆成两个测试模块**:
   - 按主题切,例如 `fetch_tests.rs`(取数、恢复、凭据)+ `fetch_tests_view.rs`(视图、历史、指纹分段);`inject_tests.rs`(公共、WorkBuddy、ZCode)+ `inject_tests_trae_dsh.rs`(Trae、DSH);
   - 第二个模块声明为 `#[cfg(test)] #[path = "fetch_tests_view.rs"] mod tests_view;`;
   - 两个模块共用的测试辅助函数留在第一个模块里,标成 `pub(super)`,第二个模块用 `use super::tests::{...}` 引入。**不要复制一份**(AGENTS.md §0「同一件事写第二份」);
   - 被移到第二个模块的测试,完整路径会从 `fetch::tests::x` 变成 `fetch::tests_view::x`。已核对:`build/` 和 `.github/` 里没有按 `::tests::` 完整路径引用测试,`--ignored` 等过滤都用测试名子串,不受影响。执行时再全仓 grep 确认一遍(含 `docs/`、`RELEASE.md`)。
2. **不要动 `mod tests` 以外的 `#[cfg(test)]` 项**:`fetch.rs` 的 `fetch_custom_all`(cfg(test) 版本)、`inject.rs` 的 `dsh_route_for`、`store.rs` L68–85 的测试钩子(`thread_local!`、`reset_history_reads` 等)、`workbuddy.rs` L2374 的 `app_roots_for`。它们是给生产代码路径注入测试行为的,留在原文件。
3. **保持编码和换行风格**:`fetch.rs`、`diaglog.rs` 带 UTF-8 BOM,原文件保留 BOM,新测试文件用无 BOM 的 UTF-8。原文件如果是 CRLF,新文件也用 CRLF。用 `node build/check-encoding.js` 复核。
4. **缩进**:内容移出后去掉一层 4 空格缩进。用脚本做,不要手工。等价性校验时按「去掉一层缩进后逐行相同」比较。
5. **有脚本读这些源文件的文本**:`build/jsdom-smoke.js` 读 `checkin.rs` 里 `pub const CHECKINS:` 到 `pub fn find(` 之间的内容;`build/thumbnail-regression.js` 读 `providers.rs` 的 `pub const PROVIDERS:`。这两段都是生产代码,不会被移动,但执行后要跑这两个脚本确认。
6. **凭据扫描夹具**:`build/scan-secrets.ps1` 的 `$knownFixtures` 按**精确字符串**豁免(如 `fetch.rs` 测试里的 `SUPERSECRETVALUE…`),不按文件路径,移动后不受影响。执行后必须跑一遍扫描。
7. **预算**:同一提交里把 `BUDGETS` 下调到新行数。生产代码降到 1500 行以下的文件(如 `checkin.rs` ~1515、`providers.rs` ~1502 降不到,`store.rs` ~1949 也降不到),继续留在预算表里并下调上限;以后清理到 1500 以下时从表里移除,改由软上限约束。

#### 收益

`fetch.rs` 余量从 0 变成约 1560 行,`inject.rs` 约 2270 行,其余文件也都有了余量。之后的 C1、A4、A5 不会再被文件规模门禁拦住。

---

### A2 `app.js` 拆分

**完整方案见 `docs/research/appjs-split-plan-2026-11-22.md`**,这里只列要点和执行约束:

- **做法**:按原顺序切成 14 个普通 `<script>`(`app-core.js` … `app-boot.js`),**一个字符都不改**,拼接后与原文件逐字节相同。依据是多个普通脚本共享全局词法作用域,`panel-layout.js`、`antigravity.js` 等现有模块已经这样使用 `app.js` 的全局名字。
- **唯一的语义差异**是函数提升只在单个文件内生效。静态扫描 228 处加载期代码,只有 3 处引用了后面才声明的函数,切分点都已避开(其中 `startInstall` 要求「快捷键录制 / 检查更新 / 更新提示 / 排查日志」放在同一个文件)。
- **分两次提交**:
  - **1a**:先改 9 个直接读 `app.js` 源码文本的测试脚本,统一走新的 `build/lib/app-sources.js`(按 `index.html` 顺序读取;`replaceOnce` 未命中就抛错;逐文件 `vm.Script` 加载)。这一步 `app.js` 保持单文件,全量门禁必须照常全绿;
  - **1b**:一次性脚本按锚点文本切分,校验拼接后的 SHA-256 与原文件一致,替换 `index.html` 的脚本标签,删掉 `app.js`;
  - 预算改为「`app/src/app*.js` 合计 ≤ 9014」,只许下调;
  - 新门禁 `build/check-app-split.js`:脚本清单与磁盘一致、每段单独可编译、跨文件顶层名字唯一、逐文件加载不抛错。
- **1b 当天冻结 `app/src/app.js`**:其它 agent 不得修改。
- **第二阶段**(各自单独提交,在 B3 / B4 之后或之间都可以):`showToast` / `saveCfg` / `bindSwitch` 等公共工具挪回 `app-core.js`;拆开 `$("manage")` 上那个 569 行的点击回调;把散落的顶层事件绑定收进各文件的 `bindXxx()`,由 `app-boot.js` 统一调用。

---

### A3 `src-tauri/src/lib.rs` 拆分

#### 现状

6271 行,62 个 `#[tauri::command]`,`run()` 本身 425 行。按分段横幅统计:

| 原区段(起始行) | 行数 | 命令 | 内容 |
|---|---|---|---|
| 开头(1) | 542 | `get_channels` `home_view` `get_config` `set_config` `key_hint` `set_key` `delete_key` | `AppState`、`emit_channels`、`home_view_of`、配置与密钥命令 |
| 渠道密钥 ↔ 密钥箱(543) | 133 | `key_source` `set_key_ref` `clear_key_ref` `vault_ref_map` | 密钥引用 |
| WorkBuddy 网页登录(676) | 149 | — | `wb_finish_login` 等 |
| Qoder 网页登录(825) | 802 | `qoderweb_login` `doubao_login` `workbuddy_login` `channel_changes` `set_autostart` `tray_icon_size` `set_tray_icon` `set_tray_tooltip` `window_cmd` `set_pin` | **混杂**:网页登录与登录观察器、自启动 `mod autostart`(两个平台版本)、托盘、窗口命令、`lock_rw`、`note` |
| 跌破红线通知(1627) | 214 | — | `maybe_notify_crit` |
| 渠道体检(1841) | 66 | — | |
| 自定义渠道增删改(1907) | 588 | `custom_test` `custom_add` `custom_remove` `probe_channel` `run_diagnostic` | 另含 `poll_loop`(112 行) |
| 检查更新(2495) | 280 | `check_update` `install_update` `skip_update_version` `save_window_pos` `app_version` `take_startup_error` | |
| 本地密钥箱(2775) | 329 | `vault_list` `vault_save` `vault_delete` `vault_reveal` `vault_copy` | |
| 每日签到(3104) | 1585 | `credit_grants` `checkin_state` `checkin_now` `checkin_history` | 另含 `run_checkin`(320 行)、Qoder 活动记录、`monitor_*`、`settle_arrivals`、`checkin_tick_once` |
| 4SAPI 模型 + 一键配置(4689) | 1544 | 21 个(`get_model_catalog` `inject_*` `copy_*` `key_reveal` `open_url` …) | 末尾是 `run()`(L5808 起) |
| 测试(6233) | 40 | — | 两个小测试模块 |

已有拆出去的先例:`sync_cmds.rs`、`codex_reset_cmd.rs`、`vault_sync.rs`、`gateway.rs`、`google_cloud_web.rs`。

#### 方案

新模块**平铺在 `app/src-tauri/src/`**,不建子目录。原因:`build/check-ipc-contract.js` 只用 `readdirSync` 扫这一层的 `.rs`,放进子目录会让命令定义「消失」(门禁会报错,但会挡住提交)。

| 新文件 | 内容 | 预计行数 |
|---|---|---|
| `lib.rs`(保留) | `mod` 声明、`AppState`、`lock_rw`、`note`、`emit_channels`、`home_view_of`、`run()`、`generate_handler!` 注册表 | ~1000 |
| `config_cmds.rs` | `get_channels` `home_view` `get_config` `set_config` `key_hint` `set_key` `delete_key` | ~400 |
| `key_ref_cmds.rs` | 密钥 ↔ 密钥箱 4 个命令 | ~135 |
| `weblogin_cmds.rs` | WorkBuddy / Qoder / 豆包网页登录、`wb_finish_login`、`spawn_qoder_login_watcher` | ~700 |
| `window_cmds.rs` | `mod autostart`、托盘 3 个命令、`window_cmd`、`set_pin`、`save_window_pos` | ~400 |
| `notify.rs` | 跌破红线通知 | ~215 |
| `diag_cmds.rs` | 渠道体检、`probe_channel`、`run_diagnostic` | ~360 |
| `custom_cmds.rs` | `custom_test` `custom_add` `custom_remove` | ~150 |
| `poll.rs` | `poll_loop` | ~120 |
| `update_cmds.rs` | `check_update` `install_update` `skip_update_version` `app_version` `take_startup_error` | ~280 |
| `vault_cmds.rs` | 本地密钥箱 5 个命令 | ~330 |
| `checkin_cmds.rs` | 签到命令、`checkin_credentials`、`checkin_state_of`、`run_checkin` | ~800 |
| `checkin_activity.rs` | Qoder 活动记录、`monitor_*`、`record_observed_already`、`settle_arrivals`、`checkin_tick_once` | ~780 |
| `model_cmds.rs` | 4SAPI 模型目录、`inject_*`、`copy_*`、`key_reveal`、`open_*` | ~1120 |

**签到段 1585 行超过 1500 软上限,必须拆成两个文件。** 上表的行数是估计值;执行时先用脚本列出每个函数的起止行,按函数为单位分配,不在函数中间切。

#### 技术细节与坑

1. **`generate_handler!`** 里的条目改成 `config_cmds::get_channels` 这种带模块前缀的形式。`check-ipc-contract.js` 按 `::` 取最后一段,能识别。**命令名不变,前端不改**。
2. **可见性**:移出去的命令和被跨模块调用的辅助函数改成 `pub(crate)`;新文件开头用 `use crate::{AppState, lock_rw, ...};`。不要为了省事把所有东西都改成 `pub`。
3. **`#[tauri::command]` 宏的限制**:命令函数所在模块在注册处必须可见。已有 `sync_cmds` 先例可照抄。
4. **`src-tauri` 本机无法运行 `cargo test`**(`STATUS_ENTRYPOINT_NOT_FOUND`,AGENTS.md §6.7),CI 也只做 `cargo check`。所以这一步的验证靠:编译、IPC 门、搬家等价性脚本、调试实例冒烟(§7)。
5. **搬家等价性脚本**(一次性,放 `build/tmp/` 用完删):把原 `lib.rs` 的行多重集合,与「新 `lib.rs` + 新文件」的行多重集合做比较。允许的差异只有:`pub(crate)` 可见性、`use` 语句、`mod` 声明、`generate_handler!` 条目的模块前缀。其它任何差异都让脚本失败。
6. **可以分 3–4 个提交**:每个提交搬 3–4 个模块,每个提交都要能编译、IPC 门通过。建议顺序:先搬依赖最少的(`notify` `custom_cmds` `update_cmds` `vault_cmds`),再搬窗口与登录,最后签到和模型。
7. **与 B1 有冲突**:B1 要在 `set_config` 里加一行唤醒调用。两者不能同时进行(§8)。

---

### A4 `fetch.rs` 生产代码拆分(A1、C1 之后)

A1 之后 `fetch.rs` 约 3700 行,不再被预算卡住,但内部仍然过大。沿用 `#[path]` 子模块方式,并用 `pub use` 重新导出,保证 `crate::fetch::X`(以及 `src-tauri` 里的 `tokenscope_core::fetch::X`)路径不变:

| 新文件 | 内容(快照位置,执行时重新定位) |
|---|---|
| `fetch_once.rs` | `fetch_once_inner`(L911 起,259 行)、`kind_for_status`、单次请求与失败分类 |
| `fetch_creds.rs` | `credentials()`、`read_desktop_tokens_region`、`demote_rejected`、`rejected` 降权逻辑 |
| `fetch_view.rs` | `build_view`(L1968 起,189 行)、历史窗口切分、缺口标注、指纹分段(L4829 横幅附近的视图相关部分) |
| `fetch.rs` | `Fetcher` 结构体与调度(`fetch_shared` / `fetch_all` / `fetch_retries` / `publish`)、缓存与恢复状态 |

注意:`Fetcher` 的字段大多是私有的。拆出去的函数如果需要访问字段,作为 `impl Fetcher` 块放在子模块里(Rust 允许同一 crate 内在多个模块里写 `impl`),字段可见性改为 `pub(super)`。

---

### A5 `inject.rs` 生产代码拆分(A1 之后)

A1 之后约 5050 行。文件本身已按客户端分好段:

| 新文件 | 原区段(横幅起点) |
|---|---|
| `inject_io.rs` | 文件读写与备份(L809–1432)—— **含 DPAPI 备份快照,属 AGENTS.md §2.4 高权限行为** |
| `inject_workbuddy.rs` | WorkBuddy / WorkBuddy AI(L1433–1730) |
| `inject_zcode.rs` | ZCode(L1731–1975) |
| `inject_trae.rs` | Trae(SQLite)(L1976–2315) |
| `inject_dsh.rs` | DeepSeek Harness(L2316–3611,约 1300 行) |
| `inject.rs` | 客户端与站点、写入内容、预告与回执、取消写入、对外的预览 / 写入 / 状态接口 |

这是改写其它客户端配置的代码。**纯搬家,不改任何行为**;搬完要跑全部 `inject` 测试、确认 README 里的高权限披露没有需要更新的地方。

### A6 其余预算文件

`store.rs`、`checkin.rs`、`providers.rs`、`workbuddy.rs` 随 A1 获得余量,本轮不再拆。规则:A1 提交里同步下调它们的预算;以后某个文件降到 1500 行以下时,从预算表删除,改由软上限约束。

---

## §3 性能类问题与方案

> 基线:11-16 性能审计实测空闲 CPU 接近 0、主进程工作集约 44 MB。下面都是「放大效应」或「空转」,不是空闲占用问题。**不要重提附录 B 里已排除的方向。**

### B1 Google Cloud 后台任务精确睡眠

#### 现状

`app/src-tauri/src/google_cloud_web.rs` 的后台 loop 每 5 秒醒一次。每次读取配置里的 2 个字段、`ledger_cache.get(...)`(已有 60 s TTL 缓存)和 `SESSION` 状态,然后按顺序判断:

1. `hidden` → 销毁窗口、作废会话;
2. `pending` 且窗口已不存在 → 标记会话完成;
3. `pending` 且非交互且已超过 90 秒 → 记失败、刷新、提示用户;
4. 无 pending、已选结算账号、`now - updated.max(started) >= interval` → 打开赠金页刷新;

`interval = idle_interval_sec.max(60)`。所以没有 pending 会话时,每 12 次唤醒里大约有 11 次什么都不做。v0.3.59 已经把每次唤醒的开销降下来了,剩下的问题是唤醒频率本身。

#### 方案

1. **把决策逻辑抽成 core 里的纯函数**(`app/core/src/google_cloud.rs`),因为 `src-tauri` 跑不了单测:
   ```rust
   pub enum GcAction { DestroyHidden, MarkClosed, Timeout, Open, Idle }
   pub struct GcTick { pub action: GcAction, pub next_check_in: Duration }
   pub fn gc_tick(now: i64, hidden: bool, pending: bool, window_open: bool,
                  interactive: bool, started: i64, updated: i64,
                  interval: i64, has_account: bool) -> GcTick
   ```
   - 有 pending 会话时:`next_check_in = 5s`(窗口被用户关掉要靠轮询发现,保持现状);
   - 隐藏或没有选结算账号时:`next_check_in = CAP`;
   - 否则:`next_check_in = clamp(updated.max(started) + interval - now, 1s, CAP)`;
   - `CAP` 建议 60 秒。这是兜底:即使某处忘了唤醒,行为最多比现在慢 1 分钟。
2. **`src-tauri` 的 loop 只负责执行**:按 `gc_tick` 的结果执行动作,然后
   ```rust
   tokio::select! {
       _ = tokio::time::sleep(tick.next_check_in) => {},
       _ = GC_WAKE.notified() => {},
   }
   ```
   `GC_WAKE` 是 `static tokio::sync::Notify`。
3. **唤醒点**:`set_config`(隐藏渠道或刷新间隔变化时)、Google Cloud 的连接 / 停止连接 / 手动刷新命令、会话开始和结束、账本写入。用 `notify_one()`:没有等待者时会留一个许可,下一次 `notified()` 立刻返回,不会丢唤醒。
4. **语义不变**:「隐藏时作废迟到结果并销毁窗口」「90 秒超时兜底」「`OPEN_GATE` 互斥」保持原样。

#### 测试

- core 单测:为 `gc_tick` 写表驱动测试,覆盖上面 4 个分支 + 边界(恰好到期、`started > updated`、interval 小于 60 时被抬到 60);
- **等价性测试**:在 0–600 秒的网格上,每 5 秒模拟一次旧 loop 的判定;同时按新的 `next_check_in` 推进模拟新逻辑。断言两者触发动作的时刻相差不超过 5 秒,动作类型完全一致;
- 反向:把 pending 分支的 5 秒改成 `CAP`,等价性测试必须变红;
- 原生冒烟(待验收清单):连接赠金账号后到点会自动打开页面;隐藏渠道后不再打开;登录后 90 秒未读到表格时出现提示。

---

### B2 Codex 重置顾问每分钟重算

#### 现状

`app/src/codex-reset.js`:

- `inputKey(c)` 的第二项是 `Math.floor(nowSec() / 60)`,所以缓存键每分钟必变;
- 每 60 秒的定时器(仅在详情弹层里有顾问区域、且不在保存或编辑日期时)调用 `ensureAdvice(c)`,键变了就进 `loadAdvice`;
- `loadAdvice` 每次都:①把 `state.advice` 置为 `null`、`state.loading = true` 并立即 `paint()` —— **界面每分钟闪一次「加载中」**;②重新调用 `codex_reset_profile_get`(只有用户保存时才会变的本地设置);③调用 `codex_reset_advice`。

#### 方案(分两步,第一步无语义风险)

**第一步(必做)**:

1. **加载期间保留旧结果**:刷新时不清空 `state.advice`,只设 `state.refreshing = true`;新结果到达后再替换。首次加载(还没有任何结果时)仍然显示「加载中」;
2. **profile 只读一次**:`state.profileLoaded` 为真时跳过 `codex_reset_profile_get`。以下情况重新读取:保存或清空订阅日期之后、切换账号指纹之后、上次读取失败之后;
3. 效果:每分钟的 IPC 从 2 次降到 1 次,并且不再闪烁。

**第二步(先量再定)**:

4. 在 `codex_reset_advice` 后端加一次耗时采样(写进诊断日志,不新增界面)。在真实数据下采样 10 次以上;
5. 如果单次耗时可以忽略(比如小于 2 ms),到此为止,保留每分钟重算;
6. 如果耗时明显,把 `inputKey` 里的分钟项换成「时间边界序号」:本地自然日、各窗口的重置时刻、重置卡的到期时刻、订阅截止时刻。只有越过其中某个边界才重算。同时,顾问文案里如果有按小时显示的剩余时长,再加一个小时粒度的边界。**这一步属于 §9 拍板项**。

#### 测试

- `build/verify-codex-reset.js` 用固定时钟:
  - 推进 1 分钟、数据不变:断言 `codex_reset_profile_get` 调用次数不变,`codex_reset_advice` 最多 1 次,**期间 DOM 里不出现加载中状态**;
  - 保存订阅日期:断言重新读取了 profile;
  - 跨本地 0 点、跨重置时刻:断言重算,结论正确(AGENTS.md §8.2 的边界);
  - 正在编辑日期时:断言不重算(已有 `CODEX_RESET_DATE_SENTINEL`,继续保留);
- 反向哨兵 `CODEX_ADVICE_FLICKER_SENTINEL=1`:把「保留旧结果」恢复成置空,上面的「不出现加载中」断言必须变红。

---

### B3 渠道行签名不含墙钟文字(A2 之后)

#### 现状

`app.js` 的 `qReconcile` 用每行输出的 **HTML 字符串**作为签名(`QSIG` WeakMap):签名不变就复用节点,变了就重新解析整行再做 diff。但行 HTML 里直接写进了随时间变化的文字:

- 失败行 / 陈旧行:`snapAge(c.updatedAt)`(「12 分钟前快照」);
- Codex 陈旧行的副标题同上;
- 峰谷渠道:`peakCountdown(c.peak, Date.now())`;
- 到期相关:`expiryRelativeSpan(...)` 生成的相对时间。

这些文字的精度是分钟,所以即使数据完全没变,这些行的签名也每分钟变一次,整行被重新解析。

#### 方案

1. 时间相关的文字一律渲染成「带绝对时刻的占位节点」:
   ```html
   <span class="tdyn" data-tdyn="snap" data-at="1700000000"></span>
   ```
   HTML 里只有绝对时间戳,不含随时间变化的文字,所以签名只随数据变化;
2. 扩展现有的 `tickPeakCountdowns`(它已经用 `setTextIfChanged` 原地更新 `data-exp-at` 节点),统一负责填充所有 `.tdyn` 节点的文字。`renderInner` 末尾已经同步调用了它,所以首次渲染后文字立刻就有,不会出现空白;
3. 需要 `title` / `aria-label` 的地方,同样在 ticker 里填,不写进 HTML;
4. `expiryRelativeSpan` 已经带 `data-exp-at`,只需要确认它输出的文字也挪到 ticker 里填。

#### 测试

- 把 `build/verify-refresh-stability.js` 从「只打印」改成带判定的模式(或新增 `--assert`):固定时钟,数据不变,推进 1 分钟。用 `MutationObserver` 观察 `#list`,断言 **`childList` 类型的变更为 0**(节点没有被替换),只允许 `characterData` 变化;
- 推进 1 分钟后,断言失败行显示「13 分钟前快照」(文字确实更新了);
- 反向哨兵 `ROW_SIG_SENTINEL=1`:把 `snapAge` 重新写回 HTML,`childList` 断言必须变红;
- `jsdom-smoke.js` 现有的行文案断言全部要继续通过。

---

### B4 `fitPill` 测量缓存(A2 之后)

#### 现状

`fitPill` 每次被 `renderPillFace` 调用(两处调用点)时都会:创建离屏 `<span>`、`getComputedStyle` 拷贝 6 个字体属性、`getBoundingClientRect` 测宽、删除节点。这会强制样式计算和布局。

#### 方案

1. 缓存键:`textContent + "|" + fsU() + "|" + document.body.className`。字体属性只会因字号档位、主题或形态类名而变化,这些都包含在键里;
2. 键不变时直接用上次的 `textW`,跳过探针;
3. **保留现有的溢出兜底**:`currentForm() === "pill" && el.scrollWidth > el.clientWidth` 这段要继续执行(只在胶囊形态下才会读 `scrollWidth`,开销可以接受),它是防止字体 / DPI 估算失误的最后一道保护(代码注释里记录过「91.5% 被吃成 91…」的教训);
4. 收缩防抖(`pillW - w >= 6`)保持不变。

#### 测试

- `jsdom-smoke.js`:对 `document.createElement` 计数,同样的文本连续渲染两次,断言探针 `span` 只创建 1 次;文本变化后再创建 1 次;
- 切换字号档位后,断言重新测量;
- 反向:去掉缓存判断,计数断言必须变红;
- CDP:`verify-form.js` / `shot-panel.js` 检查胶囊宽度不裁字(待验收清单)。

---

### B5 每轮 SQLite 写入合并为单事务(需拍板)

#### 现状

一轮取数中,每个渠道独立执行 `Snapshot::save`、`record_fp`(`store.rs` L400 起,单条 `conn.execute`)、积分渠道的 `insert_grant` / `prune_grants`(L728)等,每条都是一次独立提交。WAL + `synchronous=NORMAL` 已经开启。`store.rs` 里已有 `unchecked_transaction()` 的用法(L1192、L1613、L1624、L1687)。11-10 的方案把它列为 W2-3,当时因为「可能影响消耗推算」判为高风险,**用户决定跳过**。

#### 方案(只有在用户重新同意后才做)

1. **先量**:加一个 test-only 计数器,统计一轮的提交次数;在调试构建里记录一轮写入的总耗时。如果总耗时可以忽略,**结论写进 PROGRESS.md 后放弃此项**;
2. 如果确有收益:新增 `Store::batch(|tx| { ... })`,在一个事务里执行一组写入。`fetch` 在一轮中先收集写操作(`Vec<WriteOp>`),本轮结束时一次性提交;
3. **红线**:
   - 不能顺手做「快照值没变就不写」——今日 / 本周 / 本月消耗推算依赖完整的快照序列;
   - 事务失败时整轮写入都会丢失(以前是部分成功)。失败必须写诊断日志,下一轮照常写;不允许静默;
   - 写入顺序、时间戳、`key_fp` 必须与旧路径完全一致。

#### 测试

- **差分测试**:同一组合成的多轮取数结果,分别走旧路径和新路径写入两个内存数据库,断言所有表逐行相等,消耗推算函数的结果逐值相等;
- 计数器断言:一轮的提交次数 ≤ 2;
- 反向:在新路径里故意丢掉一条写入,差分测试必须变红;
- 事务失败注入:断言有诊断日志,下一轮恢复正常写入。

---

### B6 数据库增长与空间回收(需拍板,动用户数据)

#### 现状

`%APPDATA%\com.wacil.tokenscope\tokenscope.db`:10-27 为 23.4 MB,11-10 为 54.1 MB,11-16 为 68.5 MB(WAL 约 4 MB),约每天 +2.3 MB。保留策略是 90 天明细 + 365 天归档(`Store::prune`,L1161),`DELETE` 不会缩小文件。11-10 曾裁定「不做 VACUUM」。

#### 方案(每一步都要先征得用户同意)

1. **只读测量(在副本上)**:关闭应用,把 `tokenscope.db` 连同 `-wal`、`-shm` 复制到临时目录,对副本执行:
   - `PRAGMA page_size; PRAGMA page_count; PRAGMA freelist_count;`(算碎片率);
   - 各表行数;如果 SQLite 编译带 `dbstat`,再统计各表实际占用;
   - 按日期统计 `snapshots` 每天新增多少行,和渠道数 × 轮询频率对照;
2. **按测量结果决定**:
   - 碎片率低于 10%:增长来自真实数据,回收空间没有意义。转而检查是哪张表在增长,以及 90 天窗口内的数据量是否符合预期(例如每个渠道每分钟一行快照)。如果明显偏多,另开任务排查是不是有重复写入;
   - 碎片率高:考虑在设置页加一个**用户手动触发**的「压缩数据库」操作:先备份 → `VACUUM INTO` 临时文件 → 校验 → 原子替换。**绝不在启动时自动执行**;
3. **低风险项(可以单独先做)**:空闲时或退出时执行 `PRAGMA wal_checkpoint(TRUNCATE)`,把 WAL 文件控制在较小规模。它不改变任何数据。

#### 测试

- 压缩操作:在副本上执行前后,对所有表逐行计算哈希,必须完全一致;消耗推算、签到流水、密钥箱索引的查询结果逐值一致;
- 失败注入:磁盘空间不足、替换失败时,原文件保持不动,并提示用户;
- WAL checkpoint:执行后数据完整性检查(`PRAGMA integrity_check`)为 `ok`。

---

## §4 稳定性问题与方案

### C1 core 的锁中毒恢复(A1 之后)

#### 现状

- `app/core/src` 的生产代码里共有 **43 处** `.lock().unwrap()`,**全部在 `fetch.rs`**。按锁统计:`self.recovery` 14 处、`store` 7 处、`self.progress` 6 处、`self.cache` 5 处、`self.scopes` 4 处、`self.progress_sink` 2 处、`self.qoder_packs` 2 处、`self.last_run` 1 处、`self.qoder_memos` 1 处,另外 `failed(self.recovery…)` 1 处;
- `fetch.rs` 生产代码里还有 4 处其它 `unwrap()` / `expect()`;
- `src-tauri` 已经统一用 `lock_rw`(`lib.rs`:`m.lock().unwrap_or_else(|p| p.into_inner())`,注释写明「一次 panic 不该把后续所有命令永久卡死」),`src-tauri` 生产代码里 `.lock().unwrap()` 为 0。core 没有对应的工具函数。

#### 方案

1. 在 core 新增 `app/core/src/lockx.rs`(**不要叫 `sync`**,`sync` 模块已经被「同步到手机」占用):
   ```rust
   pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
       m.lock().unwrap_or_else(|p| {
           crate::diaglog::write_keyed("lock-poisoned", "...");  // 节流写一次
           p.into_inner()
       })
   }
   ```
   诊断日志用 `write_keyed` 节流,避免同一把锁反复写日志;
2. **逐处替换,不要批量替换**(AGENTS.md §2.3):先用脚本列出 43 处的行号和上下文,逐处确认后再替换,替换清单写进提交说明;
3. 对 `store`(`Arc<Mutex<Store>>`)的恢复要特别说明:`rusqlite` 的 `Transaction` 在 drop 时会回滚,所以持锁线程 panic 后,恢复出来的 `Store` 不会停在半个事务里。在提交说明里写明这一点;
4. 剩下 4 处 `unwrap` / `expect` 逐一判断:能证明不可能失败的(比如对常量正则编译),加注释说明原因;否则改成返回错误;
5. **新增棘轮门禁** `build/check-lock-unwrap.js`:统计 `app/core/src` 与 `app/src-tauri/src` 生产代码(排除 `#[cfg(test)]` 模块和 `*_tests.rs`)里的 `.lock().unwrap()`、`.read().unwrap()`、`.write().unwrap()`,必须为 0。带 `--self-test`(构造一个含违规写法的临时文件,必须判红)。接入 `ci.yml`、`release.yml` 和 `AGENTS.md` §4.1。

#### 测试

- `fetch_tests.rs` 新增:在一个线程里持有 `recovery` 锁时 panic,让锁中毒;随后调用 `failed(...)` / `fetch_retries(...)` / `publish(...)`,断言不 panic、行为正常,并写了一条诊断日志;对 `progress`、`cache` 各做一例;
- `--test-threads=1` 下复核(全局诊断日志有共享状态);
- 门禁自检:违规样本判红,当前代码判绿。

---

### C2 Antigravity `--version` 探测的超时处理(先复现)

#### 现状

`app/core/src/antigravity.rs` 的 `run_command_capped`:超时后执行 `child.kill()` + `child.wait()`,然后 `stdout_h.join()` / `stderr_h.join()`,等两个读线程读到 EOF。如果 `agy` 派生了子进程并且子进程继承了管道写端,杀掉直接子进程后管道不会关闭,读线程会一直阻塞,`join()` **没有超时**,调用它的线程会被一直占住。

已核对:`run_command_capped` 目前只用于 `--version` 探测(`VERSION_TIMEOUT` 5 秒)。真正的额度采集在 `antigravity_console.rs`,已经使用 kill-on-close Job Object 和 35 秒截止时间,不受影响。所以这是一个低概率、影响有限的问题。

#### 方案

1. **先复现**:写一个 core 测试夹具,用一个会派生「持有标准输出的孙进程」的假程序代替 `agy`(Windows 上可以用 PowerShell 启动一个长时间 sleep 的子进程,并让它继承标准输出)。断言 `run_command_capped` 在超时 + 1 秒内返回。测试标记 `#[cfg(windows)]`;
2. **测试红了才修**:
   - 复用 `antigravity_console.rs` 已有的 Job Object 封装,让 `run_command_capped` 启动的进程也进入 kill-on-close 作业,超时时整棵进程树一起结束;
   - 读线程改成通过 channel 回传结果,主线程用 `recv_timeout(剩余预算)` 等待;超时就放弃这个线程(detach),返回已经读到的部分并标记截断;
3. **测试是绿的**(复现不出来):保留这个测试作为回归保护,在 PROGRESS.md 里记录「未复现,不修改」。**未复现前不改实现**。

---

## §5 验收与流程问题与方案

### D1 原生窗口 CDP 检查一键化

#### 现状

以下脚本都需要连接正在运行的应用的 WebView2 远程调试端口,多个版本都标着「本轮未运行」:`verify-settings-entry.js`、`verify-checkin-center.js`、`verify-resize.js`、`verify-qoder-expiry.js`、`verify-ui-0923.js`、`verify-focus-ring.js`、`verify-form.js`,以及截图类的 `shot-panel.js`、`visual-forms.js`(默认端口 9333,其余默认 9222)。`build/launch-debug.ps1` 已经能用 `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` 打开调试端口启动应用。

#### 方案

新建 `build/run-native-checks.ps1`(**只用 ASCII**,PowerShell 5.1 会按 GBK 读 `.ps1`):

1. 检查是否已有应用实例在运行。有就退出并提示(避免连到用户正在用的那个实例);
2. **隔离用户数据**:这些脚本会改配置(`shot-panel.js` 会写回配置)。执行前先确认应用是否支持用环境变量指定数据目录:
   - 支持:指向临时目录;
   - 不支持:**不要自行给产品加这个能力**(它属于新增能力,见 §9),退而求其次——运行前备份 `%APPDATA%\com.wacil.tokenscope` 下的配置文件,运行后恢复并逐字节核对;
3. 调用 `launch-debug.ps1` 启动调试实例,等待端口可用;
4. 依次运行上面的检查脚本,每个脚本单独记录退出码和输出的最后若干行;
5. 输出汇总表(PASS / FAIL / SKIP),只要有 FAIL 就以非零退出码结束;结果写到 `build/qa/native-<日期>/`(确认该目录已被 `.gitignore` 覆盖,不要把报告提交进库);
6. 结束时关闭调试实例,并确认配置已恢复。

完成后:`RELEASE.md` 写明「改动了用户可见交互的版本,发版前必须运行 `run-native-checks.ps1`」;结果更新到待验收清单 B1 / B9。

#### 测试

- 自检:在没有调试实例时运行,必须以非零退出码失败,并给出明确提示,不能静默通过;
- 故意让其中一个检查脚本失败(例如传一个不存在的端口),汇总必须显示 FAIL,退出码非零;
- 运行前后用户配置文件逐字节一致。

---

### D2 本地文件验签入口

#### 现状

`build/verify-signature.js` 会编译一个使用 `minisign` crate 的小程序,用 `tauri.conf.json` 里的公钥验证 **GitHub 上已发布**的安装包。`RELEASE.md`「Android 发版 → 6.」的桌面私钥演练第 3 步需要验证一个**本地**测试文件的签名,目前没有入口(待验收清单 A2)。

#### 方案

给 `verify-signature.js` 加参数 `--local <文件> --sig <签名文件>`:跳过下载,直接验证本地文件;复用现有的 minisign 小程序和公钥解析代码。`RELEASE.md` 第 3 步改成调用这个参数。

#### 测试

- 用 `cargo tauri signer generate` 生成一把**临时测试密钥**,签一个临时文件。用临时公钥验证必须通过;
- 改动文件一个字节后再验,必须失败;
- 用正式公钥验证临时签名,必须失败;
- 测试结束删除临时密钥和文件。**绝不使用正式私钥做自动化测试**。

---

### D3 待验收清单纳入发版流程

在 `RELEASE.md`「每次发版」里加一步:发版提交前,把本版新增的待验收项写进 `docs/report/acceptance-backlog.md`;发版后完成验收的项目,更新状态和日期。`verify-release.js` **不做**这项检查(它检查的是线上产物)。

### D4 签名材料备份与恢复演练(用户本人执行)

步骤已写进 `RELEASE.md`「Android 发版 → 6.」。agent **不得**代为接触私钥、口令。agent 能做的只有:D2 完成后通知用户第 3 步已有工具;演练完成后由用户告知结果,agent 更新待验收清单 A1(只记日期、通过与否、指纹前 8 位)。

---

## §6 任务卡

格式说明:
- **依赖**:必须先完成的卡;
- **独占文件**:执行期间其它 agent 不得修改;
- **完成标准**:全部满足才能提交;
- 所有卡都默认包含「§4.1 门禁串行全绿 + PROGRESS.md 追加记录」,下面不再重复。

### T-00 验证并提交 2026-11-22 的既有改动

- **依赖**:无
- **独占文件**:§1.3 列出的全部文件
- **步骤**:
  1. `git status` / `git diff --stat`,确认工作区只有 §1.3 列出的改动;如有其它改动,停下来问用户;
  2. `node build/check-encoding.js --self-test && node build/check-encoding.js`;
  3. 跑完 AGENTS.md §4.1 全部门禁;
  4. 分 3 个提交:①编码修复 + 编码门禁 + workflow;②RELEASE.md + 待验收清单 + PROGRESS 归档;③AGENTS.md 精简 + DESIGN.md §16 + 两份方案文档。
- **完成标准**:门禁全绿;推送后 `text-encoding.yml` 在 GitHub 上通过。

### T-A1 Rust 测试模块外移(可拆成 2 个提交)

- **依赖**:T-00
- **独占文件**:`app/core/src/{fetch,inject,store,checkin,providers,workbuddy}.rs` 及新的 `*_tests*.rs`、`build/check-file-size.js`
- **步骤**:
  1. 记录基线:`cd app/core && cargo test --locked -- --list > before.txt`,以及 `-- --ignored --list > before-ignored.txt`(放 `build/tmp/`);
  2. 写一次性脚本:定位 `#[cfg(test)]\nmod tests {` 到对应的闭合花括号(用括号计数,并跳过字符串和注释),把内容去掉一层缩进后写入新文件,原处替换为 `#[path]` 声明;
  3. `fetch` 和 `inject` 的测试按主题分成两个文件,公共辅助函数标 `pub(super)` 并在第二个模块里 `use`;
  4. 保持 BOM 和换行风格;
  5. 下调 `check-file-size.js` 的预算;
  6. 删除一次性脚本。
- **完成标准**:
  - `cargo test --locked -- --test-threads=1` 通过数、忽略数与基线一致;
  - 测试名集合与基线一致(只允许 `tests::` → `tests_xxx::` 的模块前缀变化);
  - 等价性:被移出的内容去掉一层缩进后与新文件逐行相同(除新增的 `use super::tests::...`);
  - `jsdom-smoke.js`、`thumbnail-regression.js`、`scan-secrets.ps1`、`check-encoding.js` 通过;
  - 所有新文件 ≤ 1500 行。
- **禁止**:修改任何测试内容、改测试名、改生产代码。

### T-A2a `app.js` 拆分 1a:测试读法改造

- **依赖**:T-00;§9 的 A2 拍板
- **独占文件**:`build/lib/app-sources.js`、`build/lib/app_sources.py`(新建)、方案 §1.3 列出的 9 个脚本、`.github/workflows/ci.yml`、`release.yml`、`build/check-file-size.js`
- **步骤**:见拆分方案 §5「阶段 1a」
- **完成标准**:`app.js` 仍为单文件时全量门禁全绿;每个哨兵环境变量单独运行仍能变红;jsdom 用例数不少于改造前。

### T-A2b `app.js` 拆分 1b:搬家

- **依赖**:T-A2a;**冻结 `app/src/app.js`**
- **独占文件**:`app/src/app*.js`、`app/src/index.html`、`build/check-file-size.js`、`build/check-app-split.js`(新建)、`app/README.md`、`AGENTS.md`(§2.3 / §7.2 措辞)
- **步骤**:见拆分方案 §5「阶段 1b」与 §6
- **完成标准**:拼接后 SHA-256 与原文件一致;`check-app-split.js --self-test` 全部坏样本判红;调试实例冒烟通过(§7.1 L4)。

### T-A3 `lib.rs` 拆分(可拆成 3–4 个提交)

- **依赖**:T-00;§9 的 A3 拍板;**不能与 T-B1 同时进行**
- **独占文件**:`app/src-tauri/src/lib.rs` 及 §2 A3 表中的新文件、`build/check-file-size.js`
- **步骤**:
  1. 用脚本列出每个函数 / 结构体 / `mod` 的起止行和它引用的其它顶层名字,按 A3 的表分配到新文件,输出分配表供核对;
  2. 每个提交搬 3–4 个模块:移动代码 → 调整可见性和 `use` → 更新 `generate_handler!` 的模块前缀;
  3. 每个提交都跑搬家等价性脚本、`cargo check --all-targets --locked`、`check-ipc-contract.js`;
  4. 全部完成后下调预算。
- **完成标准**:`lib.rs` ≤ 约 1100 行;所有新文件 ≤ 1500 行;IPC 门:定义数、注册数与拆分前一致;调试实例冒烟:每个设置页签、签到中心、模型页、密钥箱、网关页都打开一次,控制台无错误。
- **禁止**:改命令名、改函数体、合并或删除函数。

### T-A4 `fetch.rs` 生产代码拆分

- **依赖**:T-A1、T-C1
- **独占文件**:`app/core/src/fetch*.rs`
- **完成标准**:`pub use` 重新导出后,`src-tauri` 不需要任何改动就能编译;core 测试全绿;纯搬家等价性校验通过。

### T-A5 `inject.rs` 生产代码拆分

- **依赖**:T-A1
- **独占文件**:`app/core/src/inject*.rs`
- **完成标准**:同 T-A4;另外跑 `jsdom-smoke.js` 的模型页用例;确认 README 中关于改写其它客户端配置的披露无需更新。

### T-B1 Google Cloud 精确睡眠

- **依赖**:T-00;**不能与 T-A3 同时进行**
- **独占文件**:`app/core/src/google_cloud.rs`、`app/src-tauri/src/google_cloud_web.rs`、`lib.rs` 中 `set_config` 的一行唤醒调用
- **完成标准**:`gc_tick` 表驱动测试和等价性测试通过;反向样本变红;待验收清单新增「赠金页到点自动刷新 / 隐藏后不刷新 / 90 秒提示」三项原生验收。

### T-B2 Codex 顾问:保留旧结果、profile 只读一次

- **依赖**:T-00(只改 `codex-reset.js`,不依赖 A2)
- **独占文件**:`app/src/codex-reset.js`、`build/verify-codex-reset.js`
- **完成标准**:B2 第一步的测试与反向哨兵通过;第二步只做耗时采样并把数据写进 PROGRESS.md,是否继续由用户决定。

### T-B3 渠道行签名去掉墙钟文字

- **依赖**:T-A2b
- **独占文件**:渲染行的 `app-rows.js` 与 ticker 所在文件、`build/verify-refresh-stability.js`、`build/jsdom-smoke.js`
- **完成标准**:B3 的 `MutationObserver` 断言与 `ROW_SIG_SENTINEL` 通过。

### T-B4 `fitPill` 测量缓存

- **依赖**:T-A2b;与 T-B3 改的是不同文件,可以并行,但要先确认它们不改同一个 `app-*.js`
- **独占文件**:胶囊渲染所在的 `app-*.js`、`build/jsdom-smoke.js`(与 T-B3 共用,需协调先后)
- **完成标准**:B4 的计数断言与反向样本通过。

### T-B5 SQLite 单事务

- **依赖**:T-A1、T-A4;§9 的 B5 拍板
- **完成标准**:见 B5。第一步测量结果显示收益可以忽略时,**本卡以「放弃」结束**,同样算完成。

### T-B6a WAL checkpoint

- **依赖**:T-00;§9 的 B6 拍板(这一项可单独同意)
- **完成标准**:checkpoint 后 `integrity_check` 为 `ok`;WAL 文件大小在空闲后回落。

### T-B6b 数据库测量与压缩

- **依赖**:用户同意在副本上测量
- **完成标准**:测量报告写进 `docs/report/`;是否做「压缩数据库」功能由用户根据报告决定。

### T-C1 core 锁中毒恢复 + 棘轮门禁

- **依赖**:T-A1
- **独占文件**:`app/core/src/lockx.rs`(新建)、`app/core/src/lib.rs`(加 `mod lockx;`)、`app/core/src/fetch.rs`、`fetch_tests*.rs`、`build/check-lock-unwrap.js`(新建)、`ci.yml`、`release.yml`、`AGENTS.md` §4.1
- **完成标准**:43 处全部替换,清单附在提交说明里;中毒测试通过;新门禁 `--self-test` 判红、正式检查为 0。

### T-C2 Antigravity 超时复现

- **依赖**:T-00
- **独占文件**:`app/core/src/antigravity.rs`
- **完成标准**:复现测试已加入;复现成功则修复并让测试变绿;复现失败则保留测试、记录结论。

### T-D1 原生检查一键脚本

- **依赖**:T-00;§9 的 D1 拍板(数据隔离方式)
- **独占文件**:`build/run-native-checks.ps1`(新建)、`RELEASE.md`、`.gitignore`(如需)
- **完成标准**:见 D1 的三项测试;实际运行一次,把结果写进待验收清单。

### T-D2 本地文件验签

- **依赖**:T-00
- **独占文件**:`build/verify-signature.js`、`RELEASE.md`
- **完成标准**:见 D2 的四项测试。

### T-D3 发版流程加待验收步骤

- **依赖**:T-00
- **独占文件**:`RELEASE.md`
- **完成标准**:`RELEASE.md` 写入这一步;与 T-D1、T-D2 修改 `RELEASE.md` 的不同段落,合并时注意冲突。

---

## §7 测试路径

### 7.1 测试分层

| 层 | 内容 | 运行方式 | 能证明什么 | 不能证明什么 |
|---|---|---|---|---|
| **L0 静态门** | IPC 契约、静默失败、文件规模、CSS 形态、文本编码、(新)`check-app-split`、(新)`check-lock-unwrap`、`node --check`、凭据扫描 | 零依赖,本机与 CI | 结构约束、契约一致 | 运行时行为 |
| **L1 core 单测** | `cargo test --locked -- --test-threads=1`(另加 `sync_fixtures`) | 本机与 CI | 纯逻辑、调度决策、差分等价 | 原生窗口、真实上游 |
| **L2 jsdom 冒烟** | `jsdom-smoke.js` 与 `verify-vault/codex-reset/google-cloud/gateway.js` 等 | 需 `npm install jsdom --no-save` | 前端渲染路径、IPC 调用、哨兵 | 真实布局、真实字体 |
| **L3 无头 Edge** | `verify-refresh-stability.js`、`verify-tray.js`(需 `playwright-core` + 系统 Edge) | 本机 | 真实 `<script>` 加载顺序、真实布局 | WebView2 特有行为 |
| **L4 调试实例 CDP** | `run-native-checks.ps1`(T-D1)或手工 `launch-debug.ps1` + 各 `verify-*.js` | 本机,需构建产物 | 原生窗口、DPI、工作区、焦点 | 真实账号数据 |
| **L5 安装版 + 真实账号** | `docs/report/acceptance-backlog.md` 中的各项 | 用户参与 | 端到端 | — |

`src-tauri` 没有 L1(本机无法运行 `cargo test`),所以:**能放进 core 的逻辑一律放进 core**(T-B1 的 `gc_tick` 就是这样设计的)。

### 7.2 每张任务卡的必跑测试

| 卡 | L0 | L1 | L2 | L3 | L4 | 新增测试 / 哨兵 |
|---|---|---|---|---|---|---|
| T-00 | ✔ | ✔ | ✔ | — | — | `check-encoding.js --self-test` |
| T-A1 | ✔ | ✔(对比基线) | ✔(`jsdom-smoke`、`thumbnail-regression`) | — | — | 测试名集合对比、搬家等价性 |
| T-A2a | ✔ | — | ✔(全部) | ✔ | — | `replaceOnce` 未命中抛错;每个哨兵单独变红 |
| T-A2b | ✔ | — | ✔ | ✔ | ✔ | SHA-256 拼接校验;`check-app-split --self-test` |
| T-A3 | ✔ | — | ✔ | — | ✔ | 搬家等价性脚本;IPC 定义数 / 注册数不变 |
| T-A4 / A5 | ✔ | ✔ | ✔ | — | — | 搬家等价性 |
| T-B1 | ✔ | ✔ | — | — | ✔(原生验收) | `gc_tick` 表驱动 + 等价性 + 反向 |
| T-B2 | ✔ | — | ✔ | — | — | 固定时钟;`CODEX_ADVICE_FLICKER_SENTINEL` |
| T-B3 | ✔ | — | ✔ | ✔ | — | `MutationObserver childList = 0`;`ROW_SIG_SENTINEL` |
| T-B4 | ✔ | — | ✔ | — | ✔(胶囊宽度) | 探针计数;去缓存反向 |
| T-B5 | ✔ | ✔ | — | — | — | 双路径差分;丢写入反向;事务失败注入 |
| T-B6a/b | ✔ | ✔ | — | — | — | `integrity_check`;副本逐行哈希 |
| T-C1 | ✔ | ✔ | — | — | — | 锁中毒测试;`check-lock-unwrap --self-test` |
| T-C2 | ✔ | ✔(Windows) | — | — | — | 孙进程夹具 |
| T-D1 | ✔ | — | — | — | ✔ | 无实例时失败;单项失败汇总为 FAIL;配置逐字节恢复 |
| T-D2 | ✔ | — | — | — | — | 临时密钥正反样本 |

### 7.3 新增的双向验证样本一览

| 名称 | 位置 | 坏样本做了什么 | 期望 |
|---|---|---|---|
| `check-encoding --self-test` | `build/check-encoding.js` | GBK 往返损坏、U+FFFD、截断、代理区、过长编码 | 全部判红 |
| `check-app-split --self-test` | `build/check-app-split.js` | 少一个脚本标签、多一个孤儿文件、函数中间截断、跨文件重名、加载期前向调用 | 全部判红 |
| `replaceOnce` | `build/lib/app-sources.js` | 标记字符串不存在 / 出现两次 | 抛错 |
| `check-lock-unwrap --self-test` | `build/check-lock-unwrap.js` | 临时文件含 `.lock().unwrap()` | 判红 |
| `gc_tick` 反向 | core 单测 | pending 分支改用 CAP | 等价性测试变红 |
| `CODEX_ADVICE_FLICKER_SENTINEL` | `verify-codex-reset.js` | 刷新时清空旧结果 | 「无加载中」断言变红 |
| `ROW_SIG_SENTINEL` | `verify-refresh-stability.js` | `snapAge` 写回 HTML | `childList` 断言变红 |
| `fitPill` 反向 | `jsdom-smoke.js` | 去掉缓存判断 | 计数断言变红 |
| SQLite 差分反向 | core 单测 | 新路径丢一条写入 | 差分测试变红 |
| `agy` 孙进程夹具 | core 单测(Windows) | 旧实现 | 若复现,超时断言变红 |
| 验签反向 | `verify-signature.js --local` | 改动一个字节 / 用错公钥 | 验签失败 |

### 7.4 通用测试纪律

- 涉及时间的测试一律使用固定时钟,覆盖跨本地午夜、周日到周一、跨月跨年、夏令时(AGENTS.md §8.1);
- 验证进程设置 `NO_PROXY=127.0.0.1,localhost,::1`;
- core 测试用 `--test-threads=1` 复核;出现 10035 / 10054 这类网络错误时保留原始记录再复核;
- 新哨兵要核对**具体功能的 FAIL 行**,不能只看退出码(哨兵环境变量会被子进程继承而串扰,AGENTS.md §6.6);
- 「只打印不判定」的工具(如 `verify-refresh-stability.js --compare`)不能作为通过的依据。

---

## §8 排期、并行与文件独占

### 8.1 依赖图

```
T-00 ─┬─► T-A1 ─┬─► T-C1 ─► T-A4 ─► (T-B5,需拍板)
      │         └─► T-A5
      ├─► T-A2a ─► T-A2b(冻结 app.js)─┬─► T-B3
      │                               └─► T-B4
      ├─► T-B1 ──(与 T-A3 互斥)──► T-A3
      ├─► T-B2
      ├─► T-C2
      ├─► T-D1(需拍板)
      ├─► T-D2 ─► 通知用户执行 D4
      └─► T-D3
T-B6a / T-B6b:用户同意后随时可做
```

### 8.2 建议的波次

| 波次 | 可以并行的卡 | 说明 |
|---|---|---|
| 0 | T-00 | 必须最先,单独完成 |
| 1 | T-A1、T-A2a、T-B1、T-B2、T-C2、T-D2、T-D3 | 独占文件互不重叠。T-D2 和 T-D3 都改 `RELEASE.md` 的不同段落,先后合并 |
| 2 | T-A2b、T-A3、T-C1、T-A5 | T-A2b 当天冻结 `app.js`;T-A3 必须在 T-B1 合并之后开始 |
| 3 | T-A4、T-B3、T-B4、T-D1 | T-B3 与 T-B4 共用 `jsdom-smoke.js`,先后合并 |
| 待定 | T-B5、T-B6a、T-B6b | 等用户拍板 |

### 8.3 文件独占规则

- 一个文件同一时间只能属于一张进行中的任务卡。领卡时在 PROGRESS.md 文末写一行「T-xx 进行中,独占文件:…」,完成时改成完成记录;
- 多张卡都要改的公共文件(`build/check-file-size.js`、`ci.yml`、`release.yml`、`AGENTS.md`、`RELEASE.md`、`jsdom-smoke.js`)**按合并顺序串行修改**,后合并的那张卡先 rebase 到最新代码;
- `check-file-size.js` 的预算:每张卡只调整自己负责的那几行。

---

## §9 需要用户拍板的事项

| # | 事项 | 影响的卡 | 建议 |
|---|---|---|---|
| 1 | `app.js` 拆分方案的 5 项(方案 §9:多个普通脚本按原顺序切片 / 14 个文件的切法与命名 / 合计预算 / 分两次提交 / 冻结窗口) | T-A2a、T-A2b | 按方案执行 |
| 2 | Rust 测试外移:`fetch` 与 `inject` 的测试各拆成两个文件,会让一部分测试的模块路径从 `tests::` 变成 `tests_xxx::` | T-A1 | 同意 |
| 3 | `lib.rs` 的新模块划分(§2 A3 的表) | T-A3 | 按表执行,允许执行者按实际依赖微调,但需在提交说明里列出调整 |
| 4 | SQLite 单事务(11-11 曾决定跳过) | T-B5 | 只同意「先测量」;测量后再决定 |
| 5 | 数据库:①在副本上只读测量;②WAL checkpoint;③是否做手动「压缩数据库」功能 | T-B6a、T-B6b | ①②同意;③看测量结果 |
| 6 | 原生检查的数据隔离:如果产品不支持指定数据目录,是否允许新增一个**仅调试构建生效**的环境变量 | T-D1 | 倾向不新增,先用备份 / 恢复配置的方式 |
| 7 | Codex 顾问第二步:是否把每分钟重算改为按时间边界重算 | T-B2 第二步 | 先看耗时数据 |
| 8 | 锁中毒处理:恢复继续运行(与 `src-tauri` 的 `lock_rw` 一致),还是改为明确报错 | T-C1 | 与 `lock_rw` 保持一致,恢复并写诊断日志 |

---

## 附录 A 证据索引(2026-11-22 快照)

| 主题 | 位置 |
|---|---|
| 文件规模预算 | `build/check-file-size.js` 的 `BUDGETS`;软上限 1500 |
| 测试外移先例 | `app/core/src/codex_reset.rs` L19–22、L771–776(`#[path]`);`app/core/src/gateway/mod.rs` L10 |
| 测试模块起点 | `fetch.rs` L3703、`inject.rs` L5050、`store.rs` L1950、`checkin.rs` L1516、`providers.rs` L1503、`workbuddy.rs` L1751 |
| 读 Rust 源码文本的脚本 | `build/jsdom-smoke.js` L105–107(`CHECKINS`)、`build/thumbnail-regression.js` L14–16(`PROVIDERS`) |
| IPC 门扫描方式 | `build/check-ipc-contract.js` L22–56(`readdirSync` 一层、`generate_handler!` 按 `::` 取末段) |
| `lib.rs` 命令注册 | `lib.rs` L6117 起 `generate_handler![` |
| `lock_rw` | `app/src-tauri/src/lib.rs` 约 L1598 |
| `fetch.rs` 的锁 | `Fetcher` 字段约 L2575–2618;43 处 `.lock().unwrap()` 起于约 L2629 |
| 推送合批(已完成) | `app/core/src/progress.rs` |
| Google Cloud 后台 loop | `app/src-tauri/src/google_cloud_web.rs` 约 L261–340 |
| Codex 顾问 | `app/src/codex-reset.js` `inputKey` 约 L544、`loadAdvice` 约 L601、定时器约 L863 |
| 行签名 | `app/src/app.js` `QSIG` 约 L1950;`snapAge` 调用约 L817/819/842;`peakCountdown` 约 L833 |
| `fitPill` | `app/src/app.js` 约 L2782;调用点约 L2711、L2770 |
| SQLite 写入 | `app/core/src/store.rs` `record_fp` 约 L400、`prune_grants` 约 L728、`prune` 约 L1161、WAL 设置约 L289 |
| `agy` 超时 | `app/core/src/antigravity.rs` `run_command_capped` 约 L201–261,仅用于 `--version`(约 L269);Job Object 在 `antigravity_console.rs` 约 L369–414 |
| CDP 脚本端口 | 多数默认 9222(`CDP_PORT` 可覆盖),`visual-forms.js` 默认 9333;`launch-debug.ps1` 约 L40 |
| 凭据扫描夹具 | `build/scan-secrets.ps1` `$knownFixtures` 约 L115(按字符串匹配) |
| 历史报告 | `docs/report/performance-audit-2026-11-16.md`、`local-gateway-audit-2026-11-19.md`、`code-quality-2026-11-12-b.md`、`android-sync-remaining-2026-11-02.md`、`test-run-2026-11-18.md` |

## 附录 B 已完成 / 已排除

**已完成,不要重复做**:

- 推送合批、隐藏态降载、WorkBuddy 每轮单次扫描、Google Cloud 账本缓存、签到凭据探测并发、`fit()` 读写分离、`#cfgPill` / `#mIndexList` 节点复用(均为 v0.3.59);
- 本地网关 G01–G16 及 SSE 边界问题(v0.3.68);
- 后台轮询精确唤醒、HTTP 客户端单例、全渠道并发、5 秒刷新闸门、批量历史读取、子进程超时(10-27 / 11-10 两轮)。

**已排除的「伪优化」,不要再提**(依据 `performance-audit-2026-11-16.md` §1、`local-gateway-audit-2026-11-19.md` §5.2):

降低轮询频率;优化 `Vec` / `clone` 字节数;减少 WebView2 进程数;更换 SQLite 连接池;修改 LTO 等 release 配置(已调优);升级依赖;合并 `get_channels` 与 `home_view`;重建网关 HTTP 客户端或更换缓存框架;用缓存真实凭据换性能。
