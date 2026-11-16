# Grandettoken 性能排查报告（2026-11-16）

> 性质：**只读排查，未修改任何产品代码**。
> 基线：`9465a83`（v0.3.58 发布后，工作树状态见 `git log`）。
> 方法：静态代码走查 + 对照既有性能方案与实测记录；**本报告不含本次运行的 profiler 采样**。
> 因此每项都标注「代码可证」还是「需实测」，不把推测写成结论。
> **轮次**：第一轮覆盖取数/渲染/凭据/数据库主链路（§0–§2）；
> 第二轮复查常驻后台任务、登录观察器、签到状态探测与强制布局回流（§3）；
> 第三轮复查实测磁盘、行签名墙钟文本、渲染期测量与「无问题清单」（§3.5）；
> §4 为门禁要求、§5 为拍板清单。

---

## 0. 结论摘要

项目**不是「没做过性能治理」**：10-27 与 11-10 两轮方案已经落地了大部分高确定性热点
（后台精确唤醒、HTTP 客户端单例、全渠道并发取数、批量历史读取、子进程超时修复等），
实测空闲 CPU 已接近零、主进程工作集约 44 MB、进程树约 430 MB（其中 WebView2 约 390 MB）。

**当前仍存在的性能问题集中在「每轮取数的放大效应」**，按确定性排序：

| 级别 | 问题 | 证据 | 状态 |
|---|---|---|---|
| 高 | 一轮取数触发**十余次**全量 `channels-updated` IPC + 前端整链重绘 | `fetch.rs:2620-2628,3465,3489` + `lib.rs:6133-6135,109-122` + `app.js:2202-2350` | 代码可证；11-10 方案 W2-1 曾列出，11-11 用户决定 Wave 2 全部跳过 |
| 中高 | WorkBuddy 凭据**每轮扫 3 遍**全盘/注册表/解密 | `fetch.rs:1586,3068` → `workbuddy.rs:1339-1341,1419-1420` | 代码可证；10-27/11-10 均评估过，后者 W2-2 被跳过 |
| 中 | 每渠道独立 SQLite 事务写快照/流水 | `fetch.rs:3424,3578,3658` + `store.rs:400-418` | 代码可证；W2-3 因消耗推算风险未纳入实施 |
| 中 | 隐藏/非面板态仍跑完整渲染链 | `app.js:2339-2349,3678-3684` | 代码可证；W2-4 被跳过 |
| 中低 | 重置顾问**每分钟固定 2 次 IPC** 重算建议 | `codex-reset.js:544-557,862-866,601-645` | 代码可证；后端开销需实测 |
| 待证 | Antigravity CLI 超时后仍 `join()` 读线程，可能超过 35 s 预算 | `antigravity.rs:232-292` | 与已修复的 WorkBuddy `fetch_key_payload` 同形，需复现确认 |
| 中 | Google Cloud 后台任务**每 5 s** 全量读配置 + SQLite 账本 + 解析 JSON | `google_cloud_web.rs:258-338`（`lib.rs:6132` 常驻） | 第二轮新增，代码可证 |
| 中低 | `checkin_state` 对未完成且开启的渠道**串行** `spawn_blocking` 探测凭据（首屏/手动刷新路径） | `lib.rs:4535-4556` + `app.js:7594-7598` | 第二轮新增，代码可证；10-27 §6 曾记为观察项 |
| 低 | 缩略布局/紧凑条/渠道卡存在**读写交替**的强制回流，三档 `offsetWidth` × 6 行 | `panel-layout.js:193-209`、`app.js:2393-2455,3248-3250,3441-3444` | 第二轮新增，代码可证；有 `requestAnimationFrame`/形态门控，收益需实测 |
| 中 | `tokenscope.db` **实测 68.5 MB**（10-27 记录 23.4 MB、11-10 记录 54.1 MB），WAL 4 MB | 第三轮现场测量 | 数据实测，非推测；`VACUUM` 历史裁决需重新评估 |
| 中低 | 失败/峰谷/到期行的 **签名含墙钟文本**，`qReconcile` 每分钟整行重解析 | `app.js:869-870,854-856,879,980-988` | 第三轮新增，代码可证；收益需测量 |
| 低 | `renderPillFace` → `fitPill` **每轮** getComputedStyle + 离屏 span + `getBoundingClientRect` | `app.js:2834-2870,2764,2822` | 第三轮新增；可随 P1-1 合批 |
| 低 | 设置控制台/`manage-index` 打开时，`renderManageIndex` 等**每轮整块 `innerHTML` 重建** | `app.js:2349,4554-4590` | 第三轮新增；无输入焦点风险，纯 DOM churn |

**一句话**：真正值得动的是第 1 项（每轮十余次全量 IPC 重绘），它把「5 秒内复用取数结果」省下的网络请求，
在跨进程 IPC 和前端渲染层又放大成了十余倍；第二轮新增的 Google Cloud 5 s 轮询是另一个确定性的常驻唤醒点；
第三轮现场实测到 `tokenscope.db` 已达 68.5 MB（约 2.3 MB/天），需要重新评估 VACUUM 裁决。
其余多为中低收益或需先实测。

---

## 1. 已落地优化（不要重复提，也不要当作本次发现）

为避免下一个 agent 把已做的事当新问题，先列出已核实的现状：

| 项 | 现状 | 证据 |
|---|---|---|
| 后台轮询自旋 | 已改为「睡到 `min(deadline, 下次重试到期)`」 | `lib.rs:2481-2583`，11-10 报告 W1-1 |
| 更新期响应 | `UPDATING_NOTIFY` 主动唤醒，长睡不延迟 | `lib.rs:2535-2539` |
| HTTP 客户端 | 主取数 / 签到 `direct_client` / promo / sync / gist / download 均单例 | `fetch.rs:2720`、`checkin.rs:1203-1207`、`proxy.rs:53-120` |
| 全渠道并发 | `join_all` + 自定义渠道并行 + 内置自定义 `join!` | `fetch.rs:2985,3491,3498` |
| 刷新去重 | `fetch_shared` 5 秒闸门，手动刷新与轮询合流 | `fetch.rs:2929-2947` |
| 历史窗口查询 | 一次 `history_between` 切出 6 窗口 + 3 缺口 + 逐日覆盖 | `fetch.rs:1896-1915,2012-2034`；4 渠道约 455 ms → 8.9 ms |
| 快照读索引 | `idx_snap_lookup` / `idx_snap_fp_cover` 覆盖扫描 | `store.rs:188,236,301` |
| SQLite 写模式 | WAL + `synchronous=NORMAL` | `store.rs:285-290` |
| 子进程超时 | WorkBuddy `fetch_key_payload` 已修（detached 读线程 + 杀进程树） | 11-10 报告 §9.7；104~133 s → 2~3 s |
| 前端最小更新 | `qReconcile` / `setTextIfChanged` / 分钟对齐倒计时 / 托盘帧去重 | `app.js:11-14,1970-2079,230-276,2946-3097` |
| 空闲资源 | 空闲 CPU 约 0；进程树内存 +2% 基线内 | 11-10 报告 §10.5/§10.6 |

另外，10-27 方案 §2.5 与 11-10 方案 §6 明确排除了若干「伪优化」：降轮询频率、
优化 `Vec`/`clone` 字节数、降 WebView2 进程数、换 SQLite 连接池、改 LTO、升级依赖、
合并 `get_channels` + `home_view` 等。**本报告不重提这些方向。**

---

## 2. 发现与修复方案（按优先级）

### P1-1　一轮取数触发十余次全量 IPC + 前端整链重绘 【高】

**代码证据（可证）**

- `Fetcher::publish()`（`fetch.rs:2620-2628`）每完成一个渠道就：
  1. 锁 `progress`，克隆整个 `Vec<ChannelView>`；
  2. 取 `progress_sink` 并调用。
- `publish` 调用点：`fetch.rs:3117`（无凭据分支）、`3465`（**每个内置渠道一次**）、
  `3489`（**每个自定义渠道一次 observer**）。
- 启动时 sink 装的是 `emit_channels`（`lib.rs:6131-6135`），它每次都：
  - 克隆整份 `Config`（`lib.rs:110-116`）；
  - 调 `home_view_of` **重算全部渠道的统一视图**（`lib.rs:60-79,117`）；
  - `app.emit("channels-updated", ChannelsPush{views: views.to_vec(), home})`（`lib.rs:118-121`）。
- 主循环末尾还会再推一次最终态（`lib.rs:2576`），重试分支也可能推（`lib.rs:2542`）。
- 前端 `applyChannelsPush` 每次都走完整 `renderInner()`（`app.js:8779-8788` → `2202-2350`），
  其中 `renderRiskBand / qReconcile / renderPill / renderDock / renderPillPick / fitCompact /
  refreshChCards / renderDetailSheet / tickPeakCountdowns / updateHeaderLive / renderManageIndex` 全跑一遍。

按 13 个内置渠道 + 自定义渠道 + 最终推送估算，**一轮约 15 次**全量 IPC + 整链渲染
（11-10 报告估计 11~14 次，当前渠道数增加后更高）。这部分开销与网络耗时无关，
在取数很快的机器上反而是主要 CPU/延迟来源。

**为什么之前没以「优化 clone」处理**：10-27 已论证单纯优化 Rust `Vec`/`clone` 低于噪声；
这里的成本是**跨进程序列化 + 前端整轮 DOM 计算**的放大，不是字节数。

**修复方案（对应 11-10 方案 W2-1，需用户重新拍板）**

1. `publish()` 仍照常更新内存态 `progress`，但 sink 调用改为 **100~200 ms 合并窗口**：
   窗口内多次 publish 只推最后一次（`tokio::time::Instant` + 一个短 `sleep` 或 debounce）；
2. 三个强制推送点不可省：① 一轮首个渠道完成的即时反馈；② 主循环最终态（`lib.rs:2576`）；
   ③ 手动刷新 / 重试分支；
3. 前端 `applyChannelsPush` 契约不变，收到什么渲染什么；
4. 更保守的替代：只做「payload 相同就不推」的去重（比较 `views` 序列化结果），不改时序。

**验证要求**（遵守 `AGENTS.md` 第 2.1 节）
- 双向验证：注入一个「每个渠道都 publish」的夹具，断言 IPC 次数从 ~N 降到 ≤2；
  再把合并窗口关掉，断言次数回到 ~N（门禁必须会变红）。
- 期望值从代码实时提取，不写死「15 次」这类数字。
- 真机确认慢网络下渐进显示仍存在；若不可接受则退回 payload 去重方案。

---

### P1-2　隐藏/非面板态仍跑完整渲染链 【中高】

**代码证据（可证）**

- `renderInner` 末尾无条件调用 `renderPill / renderDock / renderPillPick / fitCompact /
  refreshChCards / renderDetailSheet / tickPeakCountdowns / updateHeaderLive`（`app.js:2339-2349`）。
- `renderPillPick`（`app.js:3678-3684`）每轮无条件 `$("cfgPill").innerHTML = ...`，
  而 `#cfgPill` 只在「显示」设置子页可见。
- `updateHeaderLive`（`app.js:4487-4491`）无条件写 `textContent`，未复用现成的 `setTextIfChanged`。
- 面板/胶囊/贴边态下大量 DOM 是 `display:none`（`styles.css:372,375,388,722-724,803-809`），
  这些重绘 100% 不可见。
- `tickPeakCountdowns`（`app.js:230-265`）每分钟做 4 次全文档 `querySelectorAll`，
  且 `renderInner` 也调用它 —— 与 P1-1 叠加后每轮最多 15 次。

**不能踩的坑**（11-10 方案 §2.4 已写明，必须沿用）
- 不能给胶囊轮播加 `document.hidden` 短路：收进托盘时它是托盘数字轮播的唯一驱动（`app.js:5486-5487`）。
- 不能把 `refreshTraySize` 的 30 秒轮询改纯事件驱动：任务栏可在主窗口隐藏时移到另一台显示器（`app.js:3027-3028`）。

**修复方案（对应 11-10 方案 W2-4，需用户重新拍板）**

1. Rust 侧补发 `window-hidden`（现有只有 `window-shown`）；前端维护 `WINDOW_VISIBLE`；
2. 只门控**纯视觉重绘**：`#list` / `#sum` / `#cfgPill` / `#fstat` / `fitCompact` 前置遍历 /
   `tickPeakCountdowns` 的 DOM 扫描 / 详情弹层重绘；
3. **不门控**：数据接收、`pillSchedule` 定时器、`renderPillFace + drawTrayIcon + sendTrayTip`、
   签到/通知/同步/更新检查、`refreshTraySize`、所有网络请求；
4. 唤回时补一次全量 `render()` + `tickPeakCountdowns()`；
5. `updateHeaderLive` 与 `#fstat` 改用 `setTextIfChanged`；
6. `tickPeakCountdowns` 的 `expCrossed` 分支改为只更新到期行，不递归 `render({})`。

**验证**：四形态 × 隐藏 10 分钟唤回矩阵；托盘数字轮播与 tooltip 不受影响（已有 `verify-tray.js`）。

---

### P2-1　WorkBuddy 凭据每轮扫 3 遍全盘/注册表/解密 【中高】

**代码证据（可证）**

- `credentials()`（`fetch.rs:1579-1594`）对 `workbuddy`（国内）和 `workbuddy-ai`（国际版）
  各调一次 `read_desktop_tokens_region`（`fetch.rs:1479-1491`）→ `desktop_tokens_region`
  → `collect_desktop_credentials`（`workbuddy.rs:1339-1341`）。
- 国际版算账指纹时又调一次 `region_account_uins`（`fetch.rs:3067-3068`）→
  再一次 `collect_desktop_credentials`（`workbuddy.rs:1419-1420`）。
- `collect_desktop_credentials` 的链路包含：`credential_files()` → `find_app_exe()`
  （26 个盘符探测 + 注册表 Uninstall 枚举，`workbuddy.rs:588,432,491-559`）→
  逐文件读 + AES-GCM 解密（`workbuddy.rs:960-...`）。
- `KEY_CACHE`（`workbuddy.rs:416`）只缓存**派生密钥**，不缓存路径扫描与解析结果。

即**每轮 3 遍**「26 盘符探测 + 全量 Uninstall 注册表枚举 + 多目录 read_dir + 解密」。
在装了多个客户端、盘符多、注册表庞大的机器上成本明显；且 3 个调用都包在
`spawn_blocking` 里（`fetch.rs:3035-3038`），会占用阻塞线程池。

**风险约束**：10-27 方案暂不做的核心理由是「缓存凭据有登录切换后的陈旧风险」，不能简单缓存明文。

**修复方案（对应 11-10 方案 W2-2，需用户重新拍板）**

1. 不缓存密钥明文，只缓存**路径发现 + 文件解析的结构化结果**：
   - `find_app_exe()` / `app_roots()` 按 60 秒 TTL 缓存；
   - `collect_desktop_credentials()` 的解析结果按 `(文件路径, mtime, len)` 做键缓存，
     文件一变立即重读，解密结果不进缓存；
2. `region_account_uins` 改为复用本轮已算出的 `creds`，去掉第 3 遍扫描；
3. 显式失效点：登录成功 / `wb_finish_login` / `set_key` / `vault_save` / `clear_key_ref` /
   `set_web_credential_source`（与 10-27 Phase D 同一套纪律）。

**验证**：换账号登录 / 客户端升级换安装目录 / 凭据文件被外部改写三种场景必须真机验。
门禁需注入「文件 mtime 变化但缓存未失效」的坏样本证明断言会红。

---

### P2-2　每渠道独立 SQLite 事务写 【中】

**代码证据（可证）**

- 成功渠道：`Snapshot::save`（`fetch.rs:3424`）独立写；`record_fp`（`fetch.rs:3578`）独立写。
- 积分渠道：`prev_remaining`（`:3540`）、`insert_grant`（`:3552`）、`prune_grants`（`:3566`）各自写。
- 自定义渠道：`record_fp`（`:3658`）、`Snapshot::save`（`:3688`）各自写。
- `record_fp` 实现是裸 `conn.execute`（`store.rs:400-418`），WAL + `synchronous=NORMAL`
  虽已开启（`store.rs:289-290`），但每个 `execute` 仍是一次独立提交。
- 一轮 13+ 渠道 ⇒ 约 20~30 次提交，叠加在 `poll_loop` 每分钟一轮上。

**修复方案（对应 11-10 方案 W2-3，原方案判定高风险，仅列为候选）**

1. 把一轮内的 `Snapshot::save` / `record_fp` / `prune_grants` 收进一个
   `unchecked_transaction()`（`store.rs:1192,1656` 已有用法），末尾一次提交；
2. **高风险点**：不能顺手做「快照值未变不写」——今日/本周/本月消耗推算依赖快照序列
   （README 消耗推算口径），除非差分验证逐值一致，否则放弃该子项；
3. 先做「单事务」这一小步，不动快照写入频率。

**验证**：`cargo test --locked` 中快照/消耗相关用例全绿；真机确认消耗数字与改动前逐值一致；
事务提交次数用一个 test-only 计数器断言（双向验证）。

---

### P3-1　重置顾问每分钟固定 2 次 IPC 【中低】

**代码证据（可证）**

- `codex-reset.js:862-866` 每 60 s 调 `ensureAdvice(c)`。
- `inputKey(c)`（`codex-reset.js:544-557`）包含 `Math.floor(nowSec() / 60)`，
  **键每分钟必变** → `ensureAdvice` 的早退条件（`codex-reset.js:647-651`）永远不成立
  → `loadAdvice` → `codex_reset_profile_get` + `codex_reset_advice` 两次 IPC（`:618,632`）。
- 即便顾问折叠、Codex 没刷新也照跑（只要 DOM 里有 `[data-codex-reset-root]`）。

这不一定是 bug（建议含「今天要用」等时间敏感判定），但**时间键粒度是分钟**，
意味着最坏每 60 s 重算一次；后端算一次多贵需实测。

**修复方案**

1. 先量：给 `codex_reset_advice` 加耗时采样（或已有诊断日志则直接读），确认单次成本；
2. 若成本高：把 `inputKey` 的时间项从「分钟」改为与实际时间敏感字段对齐的边界
   （例如 `dataUpdatedAt` 跨本地自然日 / `resetCredits.updatedAt` 变化时再重算），
   或后端对同一 `input` 做短 TTL 结果缓存；
3. 保留跨日/跨到期时刻必须重算的语义（`AGENTS.md` 第 7 节时钟夹具纪律）。

**验证**：固定时钟覆盖跨本地 0 点、跨到期时刻、日期编辑中（`isEditingDate`）不重算。

---

### P3-2　SQLite 文件长期增长 【低，需实测】

历史观察：11-10 报告 §9.7 记录 `tokenscope.db` 已 54.1 MB（10-27 记录 23.4 MB），
`DELETE` 不回收文件页，`prune` 只删行不缩文件。90 天保留期下这是有界增长，
但文件页不回收会持续增大。

→ **现场测量见 §3.5.1**：2026-11-16 实测 `tokenscope.db` 68.5 MB，WAL ≈ 4 MB，
约 +2.3 MB/天；11-10「不做 VACUUM」的裁决需要结合碎片率数据重新评估。

**修复方案**：先重新测量当前文件大小与行数；若确实显著增长，评估
`PRAGMA incremental_vacuum` 或定期 `VACUUM INTO` 后原子替换。
**注意**：`AGENTS.md` 与 11-10 方案 §10.1 已判定「VACUUM 动用户数据，风险不可控，不做」，
本项仅作观察记录，**不建议本次实施**。

---

### P3-3　Antigravity CLI 超时后仍 `join()` 读线程 【待证】

**代码证据（可证）**

- `run_command_capped`（`antigravity.rs:232-292`）在 35 s 超时后 `child.kill()` + `child.wait()`，
  然后 `stdout_h.join()` / `stderr_h.join()`（`antigravity.rs:273-284`）等读线程看到 EOF。
- 这与 WorkBuddy `fetch_key_payload` **修复前**的形态完全相同：
  若 CLI 派生的子进程继承了管道写端，EOF 要等所有写端关闭 → `join()` 远超时预算。
- 已有缓解：单飞 + 120 s 冷却（`antigravity.rs:28,362-372`），
  所以最坏是「每轮多挂一会儿」而非无限堆积。

**修复方案**：先复现确认 `agy` 是否派生持有管道的子进程。若是，套用 11-10 §9.7
的同款修法：detached 读线程 + `recv_timeout(剩余预算)` + Windows 杀进程树（`taskkill /T /F`）。
**未复现前不要改**。

---

## 3. 第二轮复查：常驻后台任务 / 登录观察器 / 签到探测 / 布局回流

这一轮针对第一轮未覆盖的「非取数主链路」展开，复核了第一轮的关键断言（见 §3.4）。

### 3.1　Google Cloud 后台任务每 5 秒全量读配置 + SQLite 账本 【中】

**代码证据（可证）**

- `lib.rs:6132` 在 setup 里无条件常驻 `google_cloud_web::start(app.handle())`。
- `google_cloud_web.rs:258-338` 是一个 `loop`，**每 5 秒**（`:335`）醒来一次，每轮都：
  - `lock_rw(&st.config).clone()` —— **克隆整份 Config**（`:263`）；
  - `gc::load(&lock_rw(&st.store))` —— 读 SQLite `settings` 表的 Google 账本并
    **反序列化整份 JSON**（`:264`，`google_cloud.rs:69-75`）；
  - 读 `SESSION` 状态（`:282-289`）后做一串条件判断。
- 真正需要打开赠金页的条件是 `!pending && account.is_some() && now - updated >= interval`
  （`:330`），而 `interval = cfg.idle_interval_sec.max(60)`（`:277`）**至少 60 秒**。
  也就是说每 12 次唤醒里约 11 次只是在重复「读配置 + 读库 + 解析 JSON」，什么都不做。

**与既有治理的关系**：这正是 11-10 方案 W1-1 修 `poll_loop` 1 秒自旋的同类问题，
只是频率是 5 秒而非 1 秒；11-10 那份方案只审了 `poll_loop`，没审这个 setup 期常驻任务。

**修复方案**

1. 把循环改成「睡到下次该刷新的到期时刻」：算 `updated + interval`，
   用 `tokio::time::sleep_until` 一次睡到位； needing re-check 的最短粒度用
   `min(deadline, session_deadline)`，与 `poll_loop` 的 `next_retry_at` 同构；
2. 把 `gc::load` 的账本读取挪进「确实到了刷新时刻」的分支里，不要在每 5 秒的
   探测段先读一遍；`hidden` / `account.is_none()` 时不读库；
3. 配置只读需要的三个字段（hidden / idle_interval），避免整份 `Config` 深拷贝；
4. 保留 `hidden` 分支「使迟到结果失效 + destroy 窗口」和 90 秒超时兜底的语义不变。

**验证**：保留 §4 的门禁清单；重点真机验「连接赠金账号后到点自动打开页」「隐藏渠道不打开」
「登录后 90 秒未读到表格的提示」三条不受影响；加一个 test-only 计数器断言
「空闲 N 秒内账本读取次数 ≤ 预计刷新次数」，并注入坏样本证明它会红。

### 3.2　`checkin_state` 串行探测凭据 【中低】

**代码证据（可证）**

- `app.js:8765` 的 `refresh()` 每轮都 `await loadCheckin()`，后者调 `checkin_state`（`:7598`）。
- `lib.rs:4535-4556` 对每个「开启 + 今天没签完 + 未隐藏」的渠道，
  **逐个** `tokio::task::spawn_blocking(move || checkin_credential(&a2, def)).await`
  ——串行等待每个阻塞探测完成，才进下一个。
- `checkin_credential`（`lib.rs` 签到凭据链）会读第三方登录文件 / 问 webview Cookie /
  keyring RPC，最坏 2~3 秒一次（`lib.rs:3849` 旁分支已有同形注释）。N 个渠道就是 N× 串行。

**历史状态**：10-27 方案 §6「遗留观察项」已记为「暂不动」（它承担把失败原因说清的职责），
本轮复核确认**代码仍在、未实施**。本次只登记，不建议顺手改。

**修复方案（候选）**

1. 用 `futures::future::join_all` 把 N 个 `spawn_blocking` 并发驱动，再按索引把 `note`
   写回 `list[i]`，结果逐项等价；
2. 阻塞池容量要确认（Tauri 默认阻塞线程池足够 N≈4~5 个渠道），否则并发反而排队；
3. 保留「只对开启/未完成/未隐藏渠道探测」和「不进日志外发凭据」的口径。

### 3.3　布局读写交替导致的强制回流 【低】

**代码证据（可证，收益需实测）**

- `panel-layout.js:193-209` `fit()`：先 `getComputedStyle(list)` 读 `rowGap/padding`、
  `getBoundingClientRect()` 读 extra 高度、`clientHeight` 读房间高度，
  然后**写** `list.style.setProperty("--thumb-card-h", ...)`，
  又在 `rows.forEach` 里**逐个读** `value.scrollWidth/clientWidth` 并**写**
  `--thumb-value-font` —— 读写交替会强制浏览器同步计算布局。
- `app.js:2446-2455` `fitCompact()`：`wAt(lv)` 三档各读一遍 `offsetWidth`（6 行 × 3 = 18 次），
  注释明确写「读 offsetWidth 会强制回流，6 行 × 3 趟完全够快」。
- `app.js:3248-3250 / 3441-3444` `renderChCards()`：滚动锚点恢复时每个卡片读
  `getBoundingClientRect()`。
- 缓解因素：`fit()` 走 `requestAnimationFrame`（`panel-layout.js:212-214`）已去重；
  `fitCompact` 只在紧凑形态、且只算展开态。影响面有限。

**修复方案（低优先级，先量后改）**

1. `fit()` 把「读」全部提到前面、`写`集中到最后一批，避免在 forEach 里读写交替；
2. `wAt` 三档改成一次批量读 `getBoundingClientRect` 或只量一次基准宽度再按比例推算
   ——但必须保留现有像素级宽度精度（注释记录了「写死宽导致末尾 chip 被裁」的教训），
   改动前用 `verify-*` 的 width 断言做双向保护；
3. 真机用 Performance 面板量 `render` 长任务，确认 >50 ms 才值得动。

### 3.4 第一轮关键断言复核（结论：均成立）

| 第一轮断言 | 复核方式 | 结论 |
|---|---|---|
| `publish` 每渠道一次 → sink 每次 `emit_channels` | `fetch.rs:2620-2628,3117,3465,3489` + `lib.rs:6131-6135` | ✅ 成立 |
| `emit_channels` 每次克隆 Config + 重算 HomeView | `lib.rs:109-122,60-79` | ✅ 成立 |
| WorkBuddy 每轮 3 遍扫凭据 | `fetch.rs:1586,3068` → `workbuddy.rs:1339-1341,1419-1420` | ✅ 成立 |
| 快照写是逐条 `conn.execute`（独立事务） | `store.rs:400-418` + `fetch.rs:3424,3578,3658` | ✅ 成立 |
| `renderPillPick` 每轮无条件 `innerHTML` | `app.js:2339-2349,3678-3684` | ✅ 成立 |
| 重置顾问时间键含「分钟」，每分钟必重算 | `codex-reset.js:544-557,862-866` | ✅ 成立 |
| Antigravity 读线程 join 同形风险 | `antigravity.rs:232-292` | ✅ 代码形态成立，是否真卡需复现 |

**复核时新注意到（非问题，避免误报）**：

- `maybe_publish`（`lib.rs:2582` → `sync_cmds.rs:1241-1277`）有 `sync_enabled` / relay /
  401 / `next_allowed_at` / interval / `SYNC_SCHEDULED` 多重闸，**每轮调用不会每轮发请求**，
  不是性能问题；`settle_arrivals`（`lib.rs:2580,4374-4413`）只查 pending 单、零网络，可忽略。
- 密钥元数据 `KEY_META` 有 5 分钟 TTL + 单飞（`app.js:3495-3605`），**不是**每轮 RPC。
- `checkin_ticker` 5 分钟一轮（`lib.rs:4761`），但 `monitor_*_activity` 内部有
  `last_check` 1 小时间隔闸 + enabled/hidden 门控（`lib.rs:3824-3841,3976-3988`），
  绝大多数 tick 空转即返回，不构成热点。
- `doubao_web.rs:95-144` 的 1.2 秒 watcher 只在**登录窗口打开期间**运行
  （`LOGIN_CLOSED` 为真即 return，`:102-104`），且同一会话每 6 秒才补验一次、最多 10 次
  （`:120`），有明确生命周期，不是常驻热点。

---

## 3.5 第三轮复查：实测磁盘 / 行签名墙钟文本 / 渲染期测量 / 无问题清单

### 3.5.1　`tokenscope.db` 现场实测 68.5 MB，增长约 14 MB / 6 天 【中，数据实测】

**实测（本机文件，只读列举，未打开数据库）**

| 项 | 数值 |
|---|---|
| `tokenscope.db` | **68,513,792 B ≈ 65.34 MiB**（2026-11-16 21:18 最后写入） |
| `tokenscope.db-wal` | ≈ 4 MB |
| `tokenscope.db-shm` | ≈ 0.03 MB |
| 历史记录 | 10-27 文档记 23.4 MB → 11-10 记 54.1 MB → 现在约 68.5 MB |

折算约 **+2.3 MB/天**；90 天保留窗口下理论上限 ≈ 200 MB 量级
（但 `daily_summary` 归档与 `DELETE` 会削弱上限，实测值应每周复测）。

**与已有裁决的冲突**：11-10 §10.1 以「归档链路 60–70 ms、不构成瓶颈」判定不做 VACUUM /
不做 `incremental_vacuum`。本次**新增的是字节增长趋势**：文件页不回收导致
读放大（`idx_snap_lookup` 覆盖率下降）与备份/同步体积上升，且 WAL 文件也在增长。

**修复方案（需用户重新拍板，不能自动执行）**

1. 先量后改：用 `sqlite3 tokenscope.db "PRAGMA page_count; PRAGMA freelist_count;"`
   量出实际碎片率；碎片率 <10% 则维持不做；
2. 若碎片率高：`PRAGMA incremental_vacuum`（可分段、不锁库）优先于 `VACUUM INTO`
   （需预留同等空闲空间）；
3. 无论哪种，都要在**副本**上验证消耗推算/签到流水/密钥箱数据逐条一致后再替换，
   并保留升级前备份；绝不自动在启动时运行。
4. 本机数据库不属于代码仓库，所有上述工作前都要先询问用户。

### 3.5.2　行签名含墙钟文本 → `qReconcile` 每分钟整行重解析 【中低】

**代码证据（可证）**

- 签名 = 该行最终输出的 **HTML 字符串**（`app.js:1970-1980` 的注释与 `QSIG` 语义）。
- 失败 / 陈旧行写入 `snapAge(c.updatedAt)`（`app.js:854-856`）；Codex 双额度
  且 `isOld` 时年龄进左侧副标（`app.js:879`）。
- 峰谷渠道：`peakCountdown(c.peak, Date.now())` **在渲染时刻求值**并写进 `subVal`
  （`app.js:869-870`），中文分钟级文案随墙钟变化；
- 到期渠道：`expiryRelativeSpan(...)`（`app.js:980-988`）分钟级文本进签名。
- `expiryRelative` / `relTime` 的精度就是**分钟**（`app.js:102-140`），
  所以这些行的签名每分钟必变；变一次 `qReconcile` 就 `qParse(it.html)`
  整块解析新子树（`app.js:2055-2068`），再对照旧行提交 diff。

**为什么不是 P1-1 的重复**：P1-1 是「每轮十余次推送」放大渲染；这里是「即使只有
1 次推送、数据没变，行也因墙钟文本重解析」。叠加效果：每轮 IPC × 每行的签名翻转。

**修复方案**

1. 短期：给 `snapAge` / `expiryRelative` / `peakCountdown` 的**签名输入**改为绝对时刻
   （如 `data-exp-at` 已携带绝对毫秒，签名可用 `exp@绝对ts` 代替分钟文本），
   展示文本仍可随分钟更新（已经由 `tickPeakCountdowns` 以 `setTextIfChanged` 原地更新）；
2. 长期：把签名从「HTML 字符串」改为「数据域哈希」（窗口状态/余额/时刻指纹），
   彻底去掉墙钟依赖——但这是 render 层较大重构，先做第 1 步并量测；
3. 验证：用固定时钟跑 `build/verify-refresh-stability.js --compare`，断言
   「签名只随数据变化、墙钟分钟前进不切换节点身份」的双向夹具。

### 3.5.3　`renderPillFace` → `fitPill` 每轮测量 【低】

**代码证据（可证）**

- `renderInner` 每次推送后都调 `renderPill` → `renderPillFace`（`app.js:2339`）；
- `renderPillFace` 无条件调 `fitPill()`（`:2763,2822`）；
- `fitPill` 每次创建离屏 span、拷贝 8 个字体属性（`getComputedStyle`）、
  `getBoundingClientRect` 量宽（`app.js:2834-2860`）→ 强制布局；
- `renderChCards` / `renderManageIndex` 也在每次 `renderInner` 末尾无条件调用
  （`:2343,2349`），后者在设置控制台打开时**整块 `innerHTML` 重建**
  （`app.js:4554-4590`），每轮约 9 个按钮 + SVG use 引用被重建。

**修复方案**

1. `fitPill` 结果按 `(fontScale, 字体串, 文本内容, traySize)` 元组缓存，内容没变不重量；
   或改为「文本宽变化 >1 px 才重算 + 窗口宽变化才 setSize」（现有 6 px 阈值保留）；
2. `renderManageIndex` 的副标题（计数/同步状态）改用 `setTextIfChanged`，
   按钮行不再重建——它不受输入焦点保护，当前每轮重建只造成纯 DOM churn；
3. 与 P1-1 合批，一次性减少「单轮渲染链」的总工作量。

### 3.5.4　第三轮复核：未发现新的高确定性热点（记录避免重提）

- **Git 未跟踪大型机器产物**：`build/dl/`、`build/tmp/`、`build/webview2-static/out/` 均无跟踪文件；
  全仓库跟踪 471 个文件 ≈ **12.28 MB**，最大单个文件 `PROGRESS.md` 只有 0.4 MB——**没有 APK /
  安装包入库**。`build/qa/` 有 **24 个跟踪文件 ≈ 0.87 MB**（小体积 QA 截图 / android
  发布清单等，分多个 v0.3.x 提交加入，属有意留档），`build/webview2-static/` 跟踪
  `msvc-shim.c` / `msvc-alias.S`（构建必需源码，2b6615d 起）。
  `refresh-*.json` / `perf-*.json` / `*.apk` / `Cookies.*` 均被 `.gitignore` 覆盖且未入库。
- **release profile 已调优**：`opt-level="s"`、`lto=true`、`codegen-units=1`、
  `panic="abort"`、`strip=true`（`app/src-tauri/Cargo.toml:58-63`），不需要改。
- **CSS**：`backdrop-filter` 已全部清除（仅注释残留）；动画均为 one-shot 或
  门控（`body.pulse` 默认关，`sk` skeleton / `sweep` 只在瞬态，
  `breathe` 需用户开「低余量呼吸」）。
- **Android**：`GroupRepository` 用 ETag + `If-None-Match` + `next_allowed` 限流 +
  `refreshLocked` 互斥锁；`WidgetRefreshWorker` 是一次性 `OneTimeWorkRequest`；
  `GistClient` 复用 `SHARED_HTTP` OkHttpClient；前台 `startPolling` 60 s/300 s 合理。
  未见 ANR/主线程阻塞点。
- **`view::build_home`**（`view.rs:671-782`）线性工作，无嵌套循环。
- **`checkin_ticker` 5 分钟**（`lib.rs:4761`）：内部 `enabled` + `hidden` +
  `last_activity_check_at` 三重门控（`lib.rs:3824-3841,3976-3988`），多数 tick 直接返回。
- **诊断日志**：`diaglog::write_keyed` 有 5 分钟签名去重 + `WriteAfterRepeat` 聚合
  （`diaglog.rs:78,963-1005`），写入量有界且有 1 MB×2 代轮转。
- **`peakTickTimer` 分钟对齐**（`app.js:269-277`）旧问题已修，不重提。

---

## 4. 验证与门禁建议（必须遵守）

若后续决定实施上述任何一项，按 `AGENTS.md` 第 2、4 节：

1. **双向验证**：门禁既要当前代码全绿，也要注入已知坏样本时变红。
   新门禁带 `--self-test`，照 `check-ipc-contract.js` 的哨兵命令模式。
2. **期望值从源码实时提取**：IPC 次数、渠道数、用例数不写死在文档或门禁里。
3. **门禁必须零外部依赖**或写明安装方式；缺依赖就如实写「未跑」。
4. **验证脚本自身也验证**：改了渲染/CSS/命令后，先确认 `build/verify-*.js`
   引用的选择器、命令、路径还存在（11-10 的 `verify-codex-layout.js` 腐烂教训）。
5. **门禁串行跑、单独复跑一次**：并行跑 core / src-tauri / jsdom 曾因资源竞争假红。
6. **提交前清单**（`AGENTS.md` 第 4 节）：`check-ipc-contract.js`、
   `check-silent-failures.js`、`check-file-size.js`、`node --check app/src/app.js`、
   `check-css-forms.js`、`jsdom-smoke.js`、`verify-codex-reset.js`、
   `core cargo test --locked`、`src-tauri cargo check --all-targets --locked`、
   `build/scan-secrets.ps1`。
7. **性能专项基线**：沿用 11-10 方案 §3 的 `build/qa/perf-baseline-2026-11-11.json`
   口径（进程树工作集、主进程 CPU、句柄数、首屏时间），新增两项当前没有的指标：
   - 每轮 `channels-updated` IPC 次数（目标：从十余次降到 ≤2，来自 P1-1）；
   - 每轮 WorkBuddy 全盘扫描次数（目标：3 → 1，来自 P2-1）。

---

## 5. 明确不做 / 需用户拍板

| 项 | 处置 |
|---|---|
| 降轮询频率、优化 `Vec`/`clone` 字节数、降 WebView2 进程数、换连接池、改 LTO | 10-27/11-10 已排除，不重提 |
| P1-1 / P1-2 / P2-1（11-10 的 W2-1 / W2-4 / W2-2） | 2026-11-11 用户决定 Wave 2 全部跳过；本报告只重新登记，**实施需重新拍板** |
| P2-2（W2-3 单事务） | 原判定高风险，需先做消耗数字差分验证 |
| P3-2（VACUUM） | 原判定风险不可控，仅观察 |
| P3-3（Antigravity） | 未复现，不改 |
| §3.1（Google Cloud 5 s 轮询） | 第二轮新增，确定性常驻唤醒点，修复方案见 §3.1；**实施需拍板** |
| §3.2（checkin_state 串行探测） | 第二轮新增，10-27 §6 曾判定暂不动；本次只登记 |
| §3.3（布局回流） | 第二轮新增，收益需真机测量，先量后改 |
| §3.5.1（数据库碎片/VACUUM） | 第三轮新增**实测数据**：68.5 MB、~2.3 MB/天；需用户拍板 + 副本验证，绝不自动运行 |
| §3.5.2（签名墙钟文本） | 第三轮新增，改动面在渲染层，需固定时钟夹具双向验证 |
| §3.5.3（fitPill/控制台重建） | 第三轮新增，可随 P1-1 合批 |

**下一步建议**：若用户想动手，优先做 P1-1（收益最高、风险可控、可双向验证），
第二轮新增项里优先 §3.1（Google Cloud 5 s 轮询，与 11-10 已修的 `poll_loop` 自旋同构）。
第三轮新增项中 §3.5.2 / §3.5.3 可与 P1-1 合批发版（同属渲染层、回归面小、可双向夹具）；
§3.5.1（VACUUM）必须等用户拍板且先副本验证，不进任何批次。所有批次单独发版、单独跑基线。

---

## 6. 2026-11-16 实施记录（v0.3.59）

用户拍板后，本轮只实现高确定性、可双向验证的批次；下表按报告条目对齐。

| 报告条目 | 实施 | 位置 |
|---|---|---|
| P1-1 每轮十余次全量 IPC | `Progress` 合并同一轮渐进结果：首个渠道立即推、其余 150 ms 合批、终态由调用方推送；`finish()` 清脏，迟到任务不会晚到覆盖 | `app/core/src/progress.rs`、`fetch.rs` |
| P1-2 隐藏态整链重绘 | Rust 在三种隐藏入口发 `window-hidden`；前端维护 `WINDOW_VISIBLE`，隐藏时只更新托盘胶囊，唤回时补一次全量 `render()` + ticker | `app/src-tauri/src/lib.rs`、`app/src/app.js` |
| P2-1 WorkBuddy 每轮 3 遍扫描 | 每轮取数共享一份桌面端凭据快照，令牌分区与账户指纹同源；实测日志由每轮 6 条「解开 live 凭据」降为 2 条（3 遍 → 1 遍） | `app/core/src/workbuddy_cycle.rs`、`workbuddy.rs`、`fetch.rs` |
| §3.1 Google Cloud 5 s 轮询 | 账本读取加 60 s 兜底 TTL + 写入修订号，配置只读 hidden / interval 两个字段；5 s 唤醒保留（保留 pending / 90 s 超时的既有语义） | `app/core/src/google_cloud.rs`、`app/src-tauri/src/google_cloud_web.rs` |
| §3.2 checkin_state 串行探测 | `join_all` 并发驱动，按索引回写 note；探测条件与隐藏渠道红线不变 | `app/src-tauri/src/lib.rs` |
| §3.3 布局读写交替 | 缩略卡 `fit()` 先批量读宽高、再集中写字体；像素级宽度算法不变 | `app/src/panel-layout.js` |
| §3.5.3 设置控制台重建 | `#cfgPill` / `#mIndexList` 改按 key 复用；`updateHeaderLive` 改 `setTextIfChanged` | `app/src/app.js` |

**未实施**：P2-2（单事务，原判定高风险）、P3-2（VACUUM，需用户拍板）、P3-3（Antigravity，未复现）、
§3.5.2（签名墙钟文本，改动面在渲染层，留待单独批次）、§3.5.1（数据库碎片，仅观察）。

**回归**：`build/jsdom-smoke.js` 新增隐藏/唤回与按钮复用断言，并内置 `PERFORMANCE_SENTINEL`
反向哨兵（关闭门控、退化 innerHTML 重建时必须变红）；`progress.rs` 覆盖合批与定时器停止，
`google_cloud.rs` 覆盖写入重读与 60 s 兜底，`workbuddy_cycle.rs` 覆盖同轮单读与换账号。
core `cargo test --locked` 566 passed / 8 ignored（+ `sync_fixtures` 6 passed），
jsdom 238/238，真机 release 实测空闲 30 s CPU 0%、工作集约 60 MB。

---

## 附：本次排查覆盖的文件

`app/src/app.js`、`app/src/codex-reset.js`、`app/src/panel-layout.js`、
`app/src-tauri/src/lib.rs`、`app/src-tauri/src/gateway.rs`、`app/src-tauri/src/sync_cmds.rs`、
`app/src-tauri/src/gateway_http.rs`、`app/src-tauri/src/vault_sync.rs`、
`app/src-tauri/src/google_cloud_web.rs`、`app/src-tauri/src/doubao_web.rs`、
`app/core/src/fetch.rs`、`app/core/src/store.rs`、`app/core/src/workbuddy.rs`、
`app/core/src/antigravity.rs`、`app/core/src/proxy.rs`、`app/core/src/checkin.rs`、
`app/core/src/google_cloud.rs`、`app/core/src/models.rs`、`app/core/src/diaglog.rs`、
`app/core/src/view.rs`、`app/src/google-cloud.js`、`app/src/gateway-view.js`、
`app/src/vault-view.js`、`app/src/index.html`、`app/src/styles.css`、
`app/src-tauri/Cargo.toml`（release profile）、
`android/app/src/main/java/.../SyncViewModel.kt`、`.../widget/WidgetRefreshWorker.kt`、
`.../sync/GroupRepository.kt`、`.../sync/GistClient.kt`
（Android 侧仅快速核对，未见明显问题）。本机 `%APPDATA%\com.wacil.tokenscope\tokenscope.db*`
以只读方式测量了文件大小（未打开数据库、未做任何写入）。
`docs/research/performance-optimization-{2026-10-27,2026-11-10}.md` 与 `PROGRESS.md`
相关章节用于对照既有决策。
