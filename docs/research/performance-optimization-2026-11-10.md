# Grandettoken PC 端资源占用优化 · 技术方案（第二轮）

> 日期：2026-11-10　代码基线：`a60535e`（v0.3.33，工作树干净）
> 目标：**降低资源占用与消耗，不改变任何功能**
> 性质：**仅规划，未修改任何产品代码**
> 前置阅读：`docs/research/performance-optimization-2026-10-27.md`（Phase A–D 已落地于 v0.1.70）

---

## 0. 先把目标说准：什么能降，什么不能降

这一节是方案的前提。若期待的是「常驻内存明显下降」，需要先接受实测事实。

| 指标 | 基线（10-27 实测） | 判断 |
|---|---|---|
| 主进程工作集 | 53.1 MB（私有 21.9 MB） | 可小幅优化，空间有限 |
| 进程树工作集 | **428.6 MB**，其中 6 个 `msedgewebview2.exe` 占 **375.5 MB** | **大头是 WebView2 架构，Tauri 2 单窗口下无安全、可验证的压缩路径** |
| 8 秒空闲 CPU | 0 秒（采样精度内） | 空闲已接近零，压轮询频率无收益且违背产品定位 |
| 生命周期平均 CPU | 约 0.1% 单核（1.5 天累计 92.7 秒） | 同上 |
| 句柄数 | 530 | 可小幅优化 |

README:171 承诺的「常驻内存约 40 MB」是**主进程口径**，不等于任务管理器里看到的进程树。

因此本方案把「降低资源占用」重新定义为五个**可测量、且不触碰功能**的方向：

1. **空闲唤醒次数** —— 现在后台循环每秒醒一次，与注释写的「睡满整个间隔」不符；
2. **每轮刷新的 CPU / 磁盘 / 跨进程 IPC 尖峰** —— 一轮取数会推十几次全量数据、重复扫描本机凭据；
3. **隐藏态（挂件常态）的无谓重绘** —— 窗口收进托盘后仍跑完整面板渲染链；
4. **启动主线程阻塞** —— 归档 SQL、注册表写、TLS 客户端构造都在 setup 同步路径；
5. **连接池 / 句柄 / TLS 握手与开发机磁盘** —— 多处每次请求新建 HTTP 客户端；构建缓存 81 GB。

**明确不承诺**：进程树内存减半、WebView2 子进程数减少、空闲 CPU 从 0.1% 降到 0。

---

## 1. 与既有方案的关系

10-27 那份方案的 Phase A–D 已全部落地（`performance-optimization-2026-10-27.md:448-493`）：
历史窗口单次读取 + 覆盖索引、自定义渠道并行、前端分钟对齐定时器与 `KEY_META` 缓存。
实测 4 个金额渠道历史计算从约 455 ms 降到 **8.9 ms**。这些路已走过，**本方案不重复**。

同时严格遵守该文档第 2.5 节「已排除的伪优化」与第 6 节「遗留观察项」。第 6 节里三项
（WorkBuddy 重复扫描、启动期 `store.prune`、合并 IPC）在本方案中重新评估，理由见 §4.2 / §4.5 / §6，
其中合并 IPC 仍判定为不做。

### 本方案的硬约束（来自 PROGRESS / DESIGN / RELEASE / CI）

| 约束 | 来源 |
|---|---|
| 轮询间隔 60s / 300s / 900s 退避是产品定位，不得降频 | `DESIGN.md:425-431`、10-27 文档 `:127` |
| 前端无打包器、无 node_modules、CSP 只放行自身 | `README.md:173`、`PROGRESS.md:1755` |
| GNU 工具链 + native-tls/SChannel，不得切 rustls / MSVC | `PROGRESS.md:1438`、`:1459-1462` |
| 单 exe、零 WebView2Loader.dll 依赖 | `README.md:146`、CI `:108-113` |
| 核心逻辑层不依赖 Tauri，性能基准放 `app/core` | `README.md:172` |
| 锁序「先 store 后 config」、`MutexGuard` 不跨 await、阻塞 IO 必须 `spawn_blocking` | `bug-report-2026-10-13.html:603`、`:705` |
| 密钥不进 JSON/SQLite/日志，通知节流与失败降级口径不可动 | `DESIGN.md:435-438`、10-27 文档 `:21` |
| 更新不可降级、无灰度开关 → 小步分批发布 | 10-27 文档 `:405-406` |

---

## 2. 本轮新发现的热点（按收益排序）

全部带代码证据。下列结论均由 2026-11-10 对 `lib.rs` / `fetch.rs` / `app.js` / `styles.css` / `Cargo.toml` 的只读核对得出。

### 2.1 后台轮询每秒自旋（最高确定性）

`app/src-tauri/src/lib.rs:2496-2509`：

```rust
let deadline = Instant::now() + Duration::from_secs(wait + jitter);
while Instant::now() < deadline {
    tokio::time::sleep(Duration::from_secs(1)).await;      // 每 1 秒醒一次
    if UPDATING.load(...) { break; }
    if let Some(views) = fetcher.fetch_retries(&store).await {
        emit_channels(&app, &views); ...                    // 命中到期重试时全量推送
    }
}
```

等待窗口是 60 / 300 / 900 秒，但**每轮唤醒 `wait` 次**（约 86,400 次/天），
每次都锁 `state`、克隆 `Arc`、遍历 `recovery` HashMap。
`lib.rs:2550-2551` 的注释写的是「睡满整个间隔」——**实现与注释不符**，
所以这一项是「让代码回到原设计意图」，不是改功能。

`fetch_retries`（`fetch.rs:2613-2623`）只在 `recovery.next <= now` 时返回 `Some`；
退避序列实测为 5 / 15 / 60 / 300 / 900 秒（`recovery.rs:126`）。

> **关键陷阱**：不能简单换成 `sleep_until(deadline)` 一次睡到底 —— 那会把 5/15/60 秒的
> 短重试推迟到下一轮主取数，**属于改功能**。正确做法是取 `min(deadline, 下一次重试到期时刻)`。

### 2.2 一轮取数十几次全量 IPC 推送

`fetch.rs:2567-2576` 的 `publish()` 每完成一个渠道就 `progress.clone()` 并调 sink；
调用点 `:3070`（无凭据分支）、`:3402`（**每个内置渠道一次**）、`:3426`（自定义渠道），
再加主循环 `lib.rs:2539` 与重试分支 `:2505`。
sink 实现在 `lib.rs:6163-6165` → `emit_channels`（`lib.rs:104-117`）：

```rust
let c = lock_rw(&state.config).clone();          // 整份 Config 深拷贝
let home = home_view_of(&cfg, views, now_ts());  // 全渠道重算 HomeView
app.emit("channels-updated", ChannelsPush { views: views.to_vec(), home })
```

即 **约 11～14 次**「整份 `ChannelView` + `HomeView` 序列化跨进程」，
前端每次都走完整 `renderInner()`（`app.js:2201-2348`）。
`fetch_shared` 用 5 秒闸门省下的网络请求，被这一层放大成了十几次前端全链重绘。

> 10-27 文档排除的是「优化 Rust `Vec`/`clone` 本身」（收益低于噪声）。本项针对的是
> **跨进程 IPC + 前端整轮重绘的放大效应**，不是 clone 的字节数，两者不是同一件事。

### 2.3 隐藏态仍跑完整面板渲染链

`app.js:2337-2347` 无条件调用 `renderPill / renderDock / renderPillPick / fitCompact /
refreshChCards / renderDetailSheet / tickPeakCountdowns / updateHeaderLive / renderManageIndex`；
其中 `renderPillPick`（`:3666-3672`）每轮无条件 `$("cfgPill").innerHTML = ...`，
而 `#cfgPill` 只在「显示」设置子页可见。
面板/胶囊/贴边态下大量 DOM 是 `display:none`（`styles.css:829-833`、`875-886`），这些重绘 100% 不可见。

`app.js:239-274` 的 `tickPeakCountdowns` 每分钟做 4 次全文档 `querySelectorAll` + `JSON.parse`，
被 T1 定时器、每次 `renderInner` 结尾、`window-shown` 三处触发，且 `:272` 可能递归 `render({})`。
`app.js:4497-4501` 的 `updateHeaderLive` 无条件写 `textContent`，未复用现成的 `setTextIfChanged`（`:11-14`）。

**不能踩的坑**：`app.js:5511-5512` 明确写了不在 `document.hidden` 短路胶囊轮播 ——
收进托盘时它是托盘数字轮播的唯一驱动（O-10 体检有意偏离，见 `bug-report-2026-09-22.html:267-268`）。
**本方案只门控「不可见面板的纯视觉重绘」，绝不门控轮播、托盘绘制与数据接收。**

另一个坑：`app.js:3029` 的 30 秒 `refreshTraySize` 不能改成纯事件驱动 ——
`:3028` 注释写明「任务栏可以在主窗口隐藏时移动到另一台显示器，不能只监听窗口的 DPI 变化」。

### 2.4 每轮重复扫盘 / 注册表 / 解密

`fetch.rs:1531-1546`（两个 WorkBuddy 渠道各一次 `read_desktop_tokens_region`）与
`:3021`（`region_account_uins` 再调一次 `collect_desktop_credentials`），
链路走到 `workbuddy.rs:152-199` `auth_dirs` → `:439-481` `app_roots`（26 个盘符 `is_dir`）→
`:491-559` `registry_app_roots`（枚举 Uninstall 全部子键）→ `:595-661` `find_app_exe`（**无缓存**）→
`:871-873` 逐文件读 + AES-GCM 解密。
`appauth.rs:78-150` 的 Trae / Qoder 凭据同样每轮读文件 + AES/DPAPI 解密。
`workbuddy.rs:416` 的 `KEY_CACHE` 只缓存**派生密钥**，不缓存路径扫描与解密结果 ——
10-27 文档 `:435` 说的「key 有 6 小时缓存」指的就是它，不等于这条链路已经缓存。

> 10-27 文档因此把这一项列为「暂不做」，理由是收益低 + **缓存凭据有登录切换后的陈旧风险**。
> 本方案用「文件 mtime + 显式失效」解决陈旧风险，见 §4.2。

### 2.5 HTTP 客户端反复新建 / 代理快照每次请求重读

| 位置 | 现状 |
|---|---|
| `checkin.rs:1172-1179` `direct_client()` | 每次新建；调用点 `lib.rs:3917`、`:4070`（5 分钟活动监测）、`:4137`（签到） |
| `lib.rs:5803-5805` `fetch_promo_feed` | 每 30 分钟新建 |
| `sync_cmds.rs:353-359` `http_client()` | 每个 GitHub 请求新建 |
| `download.rs:16-19`、`core/src/sync/gist.rs:52-55` | 每次新建 |
| `proxy.rs:15-50` + `:110-118` | 每个网关请求读 9 个环境变量 + 3 次注册表读 |

`checkin.rs:1171` 注释写「不用连接池 —— 一天就几次请求」，**这个前提已不成立**：
它现在同时被 5 分钟节拍器的活动监测调用。改成进程内单例不引入任何依赖、不改 builder 参数，
但**必须保留 `.no_proxy()`** —— 签到强制绕开系统代理是设计裁决 4A，属于功能。

主取数 `reqwest::Client` 是单例复用的（`fetch.rs:2651-2657`），`Store` 是单连接 + WAL，
日志有 1MB×3 代轮转与 5 分钟签名去重 —— 这三项**已经做对，不动**。

### 2.6 启动主线程同步阻塞

`lib.rs:6033-6255` 的 setup 全在主线程：`Store::open`（`:6038`）、
`autostart::sync` 注册表读写（`:6071`）、窗口几何与显示器枚举（`:6076-6107`）、
**`store.prune` 90 天归档聚合 + 3 条 DELETE（`:6112`）**、
`diaglog::startup_banner`（`:6117`）、
`Gateway::new` → `ProxyClients::new` 构造 2 个 TLS 客户端（`:6131`）。
`store.rs:1246` 的归档 SQL 用 `LAG(...) OVER (...)` 且 `WHERE ts < ?`，现有索引前缀是
`provider_id`，对纯 `ts` 谓词无用 → 全表扫描。
启动期 `prune` 与 `poll_loop` 里每天一次的 `prune`（`lib.rs:2518-2524`）是重复工作。

### 2.7 其它（低风险 / 可选）

- `styles.css:1146`、`:1646` 仍有 `backdrop-filter:blur(...)`，与 `:826-827` 自己的注释
  「透明窗口采不到桌面，这属性本来就是空转，还会单开一层合成层」直接矛盾；
  两处遮罩本身已有 `#0b0d12cc` / `#00000066` 半透明底色，**删掉即零功能变化的纯收益**。
- `Cargo.lock` 有 reqwest 0.12.28 + 0.13.5 两套 TLS 栈（native-tls / rustls），
  但**不能统一**：会把 app/core 切到 rustls，违反 GNU/SChannel 约束（§6）。
- `Cargo.toml:25` 的 `image-png` 在 Windows 上可能可裁（`Image::new_owned` 走原始 RGBA，
  默认图标走 ICO 资源），需 `cargo check` 验证，属体积项。
- 构建缓存 `app/src-tauri/target` 66 GB（其中 `debug` 45 GB）+ `app/core/target` 15 GB，
  已被 `.gitignore:2-4` 排除、不进交付包（安装包 5,740,907 B）；仅影响开发机磁盘。

---

## 3. 基线固化（Wave 0，不改产品代码）

先量后改。沿用 10-27 文档 `:386-396` 的验收口径，补两项本轮新增指标：

| 指标 | 采集方式 | 门槛 |
|---|---|---|
| 后台唤醒次数 / 小时 | 加 test-only 计数器或在 `poll_loop` 埋点，release 采样 1 小时 | 目标：从约 3600 次/小时降到 ≈ 每小时轮次数 + 重试次数 |
| 每轮 `channels-updated` 推送次数 | 前端 `listen` 计数 + `verify-refresh-stability.js` | 目标 ≤ 2 次/轮 |
| 单次 `render` 长任务 | 10-27 文档 `:392`：无 >50 ms，p95 不比基线差 10% | 沿用 |
| 进程树工作集 / 句柄 | 同 10-27 文档 `:41-53` 方法 | 不高于基线 +5%（**不以降 WebView2 为目标**） |
| 空闲 CPU | 8 秒采样 + 生命周期累计 | 不高于基线 +0.1 个百分点 |
| 启动到首屏 | 三个冷启动取中位数 | 不慢于基线 10% |
| 二进制体积 | 基线 `Grandettoken.exe` 10,425,344 B | 不高于基线 +2% |

产物落到 `build/qa/perf-baseline-2026-11-10.json`，与 `build/qa/refresh-baseline.json` 并列。

---

## 4. 实施设计

### 4.1 Wave 1 · 纯调度与纯资源（P0，零语义变化，优先发）

> **状态：已实施（2026-11-10，见 §9）。W1-3 经复核后放弃，理由见该条。**

#### W1-1　`poll_loop` 精确唤醒

- 在 `Fetcher` 上新增只读方法 `next_retry_at() -> Option<Instant>`，取
  `recovery.values().map(|r| r.next).min()`，换算为 `tokio::time::Instant`；
- 用 `sleep_until(min(deadline, next_retry_at))` 替换 `:2497-2509` 的 1 秒 `while` 循环：
  每轮醒来后，若 `now >= next_retry_at` 则执行一次原有 `fetch_retries` 分支
  （`emit_channels` / `maybe_notify_crit` / `settle_arrivals` 原样保留），再重算下一次到期时刻；
- 保留 `UPDATING` 检查（`:2471-2475`、`:2499`、`:2510`）：更新期间仍以 10 秒粒度跳过；
- 保留 jitter、失败计数、可见性判据、主取数时刻，**一个都不改**。

**为什么不算改功能**：重试的触发条件仍是 `recovery.next <= now` 且与原来同为秒级精度；
主取数仍在同一 `deadline` 之后；从「每秒检查一次」变成「到期才检查」，
改变的只是检查次数，不是检查结果或时机。

**风险**：极低。唯一需要验证的是 `UPDATING` 在长睡眠期间的响应（原本 ≤1 秒，改后可能
最多等一个 `deadline`）——若要求保持响应性，用 `tokio::select!` 监听一个
`UPDATING` 变化通知；**这点必须在实施时明确，否则属于轻微行为变化**。

#### W1-2　HTTP 客户端按策略进程内单例

- 用 `std::sync::OnceLock<reqwest::Client>` 分别缓存：
  `checkin::direct_client`（no_proxy + 20s + BROWSER_UA）、
  `fetch_promo_feed`（15s）、`sync_cmds::http_client`、`download::download_link`、
  `sync/gist.rs::GistClient` 持有的 `Arc<Client>`；
- 所有 builder 参数**逐字不变**，只把「每次构造」改成「首次构造、之后复用」；
- `checkin.rs:1171` 的注释同步更新（说明前提已变化），但 `.no_proxy()` 保留；
- `Fetcher::http` / `ProxyClients` 现有单例不动。

**为什么不算改功能**：请求参数、超时、UA、代理策略、目标域名全部一致，
仅连接池与 TLS 上下文跨调用复用。同类做法项目已在 Android 端落地
（`PROGRESS.md:4276-4277` 共享 `OkHttpClient` 单例）。

#### W1-3　`ProxySnapshot::current()` 加 TTL 采样 —— **复核后放弃**

原设想：给代理快照加 30～60 秒 TTL，省掉「每个网关请求读 9 个环境变量 + 3 次注册表」。
实施前复核否掉了，理由是**收益与行为风险不成比例**：

- `ProxySnapshot::current()` 的调用频率与网关请求量相同（`gateway.rs:296` / `:624`），
  只有真正高频调用本地 API 的用户才会放大它；每次成本是 9 次 `var_os` + 3 次小注册表读，
  在微秒量级。
- 加 TTL 会让「用户刚改完系统代理、立刻用网关」延迟最长一个 TTL 才生效。
  要消掉这个延迟就必须补一串显式失效点（配置变更、网关启动、系统代理通知），
  **等于用一个新增的时序依赖去换一个测不出来的收益** —— 不符合「不改变功能」的边界。
- 现状已经是「快照变了才重建连接池」（`proxy.rs:58-71`），真正的重活（TLS 初始化）
  本来就没有每请求重做。

结论：不做。若将来有网关高频调用的实测样本，再重新评估。

#### W1-4　删两处残留 `backdrop-filter`

`styles.css:1146`、`:1646` 删除属性，保留半透明遮罩与 `fadeIn` 动画。
与 `:826-827` 既有裁决一致，纯收益。

**Wave 1 验收**：4 个金额渠道数值逐值一致、`build.ps1 test / check`、
`verify-tray.js`（托盘数字轮播与 tooltip 不受影响）、`verify-refresh-stability.js --compare`、
隐藏 10 分钟唤回、断网重试时序（重点验 5/15/60 秒短重试仍准点）。

---

### 4.2 Wave 2 · 刷新尖峰（P1，中低风险）

#### W2-1　progress emit 合并

- `publish()`（`fetch.rs:2567-2576`）仍照常更新内存态 `progress`；
- sink 调用改为 **100～200 ms 合并窗口**：窗口内多次 publish 只推最后一次；
- **三个强制推送点不可省**：① 一轮开始的首个过场视图（保持即时反馈）、
  ② 主循环 `lib.rs:2539` 的最终态、③ 手动刷新 `refresh_now` / 重试分支；
- 前端 `applyChannelsPush`（`app.js:8872`）契约不变，收到什么就渲染什么。

**为什么不算改功能**：渠道逐个完成的渐进显示仍存在（合并窗口远小于一轮取数耗时），
最终态与原来逐值一致；只是把「每渠道一次」压成「每批次一次」。
**风险点**：慢网络下渐进感会变弱，需真机确认；若不可接受则只做「相同 payload 去重」
（`views` 序列化结果未变就不推），不改时序。

#### W2-2　本机凭据扫描结果缓存（重新打开 10-27 遗留项）

不缓存**密钥明文值**，只缓存「路径发现 + 文件解析的结构化结果」：

- `find_app_exe()`（`workbuddy.rs:595-661`）与 `app_roots()`（`:439-481`）结果按 60 秒 TTL 缓存；
- `collect_desktop_credentials()`（`:871-873`）与 `appauth::candidates()` 的解析结果按
  `(文件路径, mtime, len)` 做键缓存；**凭据文件一变立即重读**，解密结果不进缓存；
- 显式失效点：登录成功 / `wb_finish_login` / `set_key` / `vault_save` / `clear_key_ref` /
  `set_web_credential_source`（与 10-27 Phase D 的 7 个失效点同一套纪律）；
- `region_account_uins`（`:1372-1382`）改为复用本轮已算出的 `creds`，去掉第 3 遍扫描。

**为什么现在可以做**：10-27 文档暂不做的核心顾虑是「缓存凭据有登录切换后的陈旧风险」，
本设计用 mtime + 显式失效消除它；另一顾虑「收益低」是当时基于「文件很小」的估计，
而本轮核对发现一轮要跑 2～3 遍「26 次盘符探测 + 全量 Uninstall 注册表枚举 + 多目录 read_dir」。

**风险**：中。必须验「换账号登录 / 客户端升级换安装目录 / 凭据文件被外部改写」三种场景。

#### W2-3　一轮内 DB 写合并为单事务 + 快照未变不写

`fetch.rs:3361`（`Snapshot::save`）、`:3514-3519`（`record_fp`）、`:3503`（`prune_grants`）
收进一个 `unchecked_transaction()`（`store.rs:1204` 已有用法）；
值未变化的轮次跳过 `Snapshot::save`。
**注意**：这会改变「每轮都有新快照行」的现状，而今日/本周消耗推算依赖快照序列
（`README.md:85-93`）—— **属高风险项，本方案仅列为「需先做差分验证的候选」，
不放入前两批实施**。若无法证明 90 天消耗口径逐值一致，直接放弃。

#### W2-4　前端隐藏态门控（需先补一个信号）

- Rust 侧目前**只有 `window-shown`**（`lib.rs:5998,6024,6202,6227`），没有对应的 hidden 事件；
  在 `window_cmd("hide")` 与主窗口 `hide()` 出口补发 `window-hidden`（新增信号，不改现有行为）；
- 前端维护 `WINDOW_VISIBLE`，由 `window-shown` / `window-hidden` 切换；
- **门控范围（只门控纯视觉重绘）**：`renderInner` 里 `#list` / `#sum` / `#cfgPill` /
  `#fstat` / `fitCompact` 前置遍历 / `tickPeakCountdowns` 的 DOM 扫描 / 详情弹层重绘；
- **不门控（必须照跑）**：`applyChannelsPush` 数据接收、`pillSchedule` 定时器、
  `renderPillFace` + `drawTrayIcon` + `sendTrayTip`（托盘数字轮播）、签到 / 通知 / 同步 / 更新检查、
  `refreshTraySize` 30 秒轮询、所有网络请求；
- 唤回时在现有 `window-shown` 回调（`app.js:9048-9052`）补一次全量 `render()` + `tickPeakCountdowns()`；
- `updateHeaderLive`（`:4497-4501`）、`fstat`（`:2302`）改用现成的 `setTextIfChanged`（`:11-14`）；
- `tickPeakCountdowns` 的 `:272` `expCrossed` 分支改为只更新到期行，不递归整页 `render({})`。

**为什么不算改功能**：隐藏期间面板本来就不可见；数据照收、托盘照转、唤回立刻补齐，
与 10-27 文档 `:381` 的人工回归用例「关闭进托盘 10 分钟再唤回：数值、胶囊、托盘数字一致」对齐。

**风险**：中低，主要靠四形态 × 隐藏唤回矩阵回归覆盖。

---

### 4.3 Wave 3 · 启动与进程级（P2，需先量后有条件做）

| 项 | 设计 | 前置条件 |
|---|---|---|
| 启动期 `store.prune` 下沉 `spawn_blocking` | 与 `poll_loop` 每日 prune 合并，避免启动 + 当天重复归档；归档 SQL 加 `snapshots(ts)` 索引需评估 +DB 体积 | 10-27 文档 `:436` 判定「169k 行下不构成瓶颈」→ **先测首屏时间，超阈值才做** |
| `autostart::sync` / `diaglog::startup_banner` 下沉 | 参照 `lib.rs:1585-1589` 的 `set_autostart` 先例 | 同上 |
| `ProxyClients` 惰性构造 | `Gateway::new`（`gateway.rs:81`）只存配置，首次 `client()` 才建 2 个 TLS 客户端 | 网关 autostart 语义不变（`/v1/models` 仍可用） |
| 自定义 tokio runtime `worker_threads(2~4)` | `main.rs` 里 `async_runtime::set` 注入；**只压 worker，不动阻塞池**（`lib.rs` 有 40+ 处 `spawn_blocking`） | 需完整基准；风险中，最后做 |
| `image-png` feature 裁剪 | 需 `build.ps1 check` 验证 Windows 图标路径仍正常 | 体积收益，非运行时 |
| `check_update` 10 分钟 IPC → Rust 侧按 24h 精确调度 | 保留前端 `force=true` 手动检查 | 低收益，可选 |

---

### 4.4 Wave 4 · 开发机磁盘（非产品代码，随时可做）

- `app/src-tauri/target/debug` 约 45 GB（545 个 debug exe / 9.57 GB）、
  `release-pre-bundle-*` 残留、旧 `*.pdb`、`app/core/target` 约 15 GB；
- **不能全删**：`RELEASE.md:7-11` 明确依赖 `target/release` 的增量缓存把本地打包压到 8–12 分钟；
  保留最近一次 `release/.fingerprint` 与 `deps`；
- 只做「列出待清理清单 → 人工确认 → 小批删除」，且这些目录已被 `.gitignore` 排除、不进交付包。

---

## 5. 分批发布与回滚

遵守 10-27 文档 `:405-406`「一次只发布一项、更新不可降级、无灰度开关」：

| 批次 | 内容 | 风险 | 回退方式 |
|---|---|---|---|
| v0.3.34 | Wave 1（W1-1 ~ W1-4） | 低 | 恢复 1 秒循环 / 恢复每次新建 Client / 恢复 CSS 属性 |
| v0.3.35 | Wave 2 的 W2-1 + W2-4 | 中低 | 去掉合并窗口；`WINDOW_VISIBLE` 常量置 true |
| v0.3.36 | Wave 2 的 W2-2（凭据扫描缓存） | 中 | 缓存开关关掉即回旧路径 |
| v0.3.37+ | Wave 3 各项按实测结果逐项决定 | 中 | 各项独立 |
| — | Wave 4 磁盘清理 | 无 | 不涉及发布 |

**每批必过门禁**（`RELEASE.md:35-42`、10-27 文档 `:365-374`）：
`build.ps1 test`、`build.ps1 check`、`node --check app/src/app.js`、
`jsdom-smoke.js`（现 177/177）、`probe-frontend.js`、`check-css-forms.js`、
`verify-refresh-stability.js --compare`、`verify-tray.js`、
`cargo test --release bench_history_batch -- --ignored`（本地跑，不进 CI）。

**每批人工回归**（10-27 文档 `:376-384`）：四形态、设置页停留两个轮询周期、
隐藏 10 分钟唤回、断网 / 单渠道超时、跨零点、覆盖升级后数据与凭据保留；
新增：代理改动后立刻用网关、WorkBuddy 换账号登录、失败渠道 5/15/60 秒重试时序。

---

## 6. 明确不做（避免重复踩坑）

| 不做 | 依据 |
|---|---|
| 降低轮询频率（1min → 5min 等） | `DESIGN.md:425-431`；空闲 CPU 已≈0，会牺牲实时性 |
| 降 WebView2 进程数 / 加启动参数 / 改进程模型 | 10-27 文档 `:129`、`:438`；375 MB 是 Chromium 正常成本，Windows 上 `backgroundThrottling` 配置本身不受支持 |
| 把 `hide()` 改 `close()` | 会销毁 WebView、丢登录会话与前端状态 |
| 给胶囊轮播加 `document.hidden` 短路 | O-10 有意偏离：会停掉托盘数字轮播（`app.js:5511-5512`） |
| `refreshTraySize` 30 秒轮询改纯事件驱动 | `app.js:3028` 注释：任务栏可在窗口隐藏时移到另一台显示器 |
| 缓存凭据明文值 / 改密钥来源 | 安全敏感，收益低于回归面；本方案只缓存路径与解析结构 |
| 一轮内快照「值未变不写」 | 会影响消耗推算的快照基线，**除非差分证明逐值一致，否则放弃** |
| 统一 reqwest 0.12 / 0.13 双版本 | 会把 app/core 切到 rustls，违反 GNU 工具链 + SChannel 硬约束 |
| 改 `opt-level` / LTO / 换 SQLite 连接池 / 自动 VACUUM·ANALYZE | 10-27 文档 `:130-131` 已排除 |
| 合并 `get_channels` + `home_view` IPC | 10-27 文档 `:437` 判定低收益、暂不做 |
| 关掉网关 autostart、把 `lan:true` 改回环 | 会改变 `/v1/models` 行为，属改功能 |
| 改 release profile、引入前端依赖或新依赖 | 项目硬约束，见 §1 |

---

## 7. 预期收益（待 Wave 0 实测校准，不预先承诺百分比）

| 维度 | 现状 | 目标 |
|---|---|---|
| 后台唤醒 | 约 86,400 次/天（1 秒自旋） | 每轮 1 次 + 实际重试次数 |
| 每轮全量 IPC | 约 11～14 次 | 1～2 次 |
| 隐藏态整轮 DOM 重建 | 每轮 1 次 + 每 12 秒胶囊脸链 | 面板部分 0 次（唤回补 1 次）；托盘链路保持 |
| 每轮本机凭据扫描 | 2～3 遍全盘 + 注册表枚举 | 缓存命中，文件变更时即时失效 |
| TLS 客户端 / 连接池 | 多处每次新建 | 每策略 1 个进程内单例 |
| 启动主线程 | 归档 SQL + 注册表 + 2 个 TLS 客户端同步执行 | 有条件下沉到后台（需先测） |
| 进程树内存 | 428.6 MB（WebView2 375.5 MB） | **不作为目标**；仅保证不高于基线 +5% |
| 开发机磁盘 | 构建缓存约 81 GB | 保留增量缓存前提下清理陈旧产物 |

---

## 8. 建议的下一步

1. 先做 **Wave 0**：固化基线，特别是「唤醒次数 / 每轮 emit 次数 / 首屏时间」三项当前没有现成数字；
2. 确认 `poll_loop` 的 `UPDATING` 响应性要求（决定 W1-1 用 `sleep_until` 还是 `select!`）；
3. 按 **v0.3.34 = Wave 1** 起步 —— 四项都是「让实现回到设计意图 / 只改对象生命周期 / 删空转属性」，
   是整份方案里唯一一批可以论证「零功能变化」的改动；
4. Wave 2 之后每一项都需要真机回归结论才能进下一批。

---

## 9. 实施记录：Wave 1（2026-11-10）

### 9.1 代码改动

| 文件 | 改动 |
|---|---|
| `app/core/src/fetch.rs` | 新增 `Fetcher::next_retry_at()`（只读，取 `recovery` 里最小的 `next`）；新增单测 `next_retry_at_is_the_earliest_due_channel` |
| `app/src-tauri/src/lib.rs` | `poll_loop` 的 1 秒自旋改为「睡到 `min(deadline, 下一次重试到期)`」，重试仍走原 `fetch_retries` 分支；新增 `UPDATING_NOTIFY`，在 `install_update` 置位 `UPDATING` 时主动唤醒，保证「更新期间停取数」不因长睡眠而延迟 |
| `app/core/src/checkin.rs` | `direct_client()` 改 `OnceLock` 单例；`no_proxy` / 20s / UA 逐字不变；注释更新（原「一天几次请求」的前提已不成立） |
| `app/src-tauri/src/lib.rs` | `fetch_promo_feed` 的客户端改 `OnceLock<Result<Client, String>>`，构造失败仍返回原错误文案 |
| `app/src-tauri/src/sync_cmds.rs` | `http_client()` 改 `OnceLock` 单例，返回 `&'static Client`，参数与 `SYNC_NETWORK` 错误码不变 |
| `app/src-tauri/src/download.rs` | 新增 `release_client()` 单例，替换每次新建 |
| `app/core/src/sync/gist.rs` | `GistClient::new` 复用进程内客户端（一次同步会连开 7 个 `GistClient`） |
| `app/src/styles.css` | 删除 `.modl-ov`（`:1146`）与 `.shade`（`:1646`）残留的 `backdrop-filter`，与 `:826-827` 既有裁决一致；遮罩底色与动画保留 |

### 9.2 行为等价性

| 改动 | 为什么不是改功能 |
|---|---|
| 精确唤醒 | 是否重试仍由 `fetch_retries` 内 `r.next <= now_ts()` 判定；已到期时最小睡 1 秒，等于原来的 1 秒检查粒度；主取数仍在同一 `deadline` 后；jitter / 失败计数 / 可见性判据一个没动；`UPDATING` 另有 `Notify` 保证立即响应 |
| Client 单例 | 请求参数、超时、UA、`no_proxy` 策略、目标域名全部一致，只是连接池与 TLS 上下文跨调用复用；错误文案与错误类型原样返回 |
| 删 `backdrop-filter` | 两处遮罩本身已有 `#0b0d12cc` / `#00000066` 半透明底色；`fadeIn` 动画与层级不变。这是删除一个在透明窗口里本就采不到 backdrop 的空转属性 |

### 9.3 门禁结果

| 门 | 结果 |
|---|---|
| `cargo check --locked`（core） | 通过，8.96 s，0 警告 |
| `cargo check --all-targets --locked`（src-tauri） | 通过，13.13 s，0 警告（`tokio::sync::Notify` 由 Tauri 依赖树已启用的 feature 提供，未改 Cargo.toml） |
| `cargo test --locked`（core） | **423 passed / 0 failed / 8 ignored**（基线 422 + 新增 1；§9.7 追加 2 个单测后为 425） |
| `node --check app/src/app.js` | 通过 |
| `build/check-css-forms.js` | PASS，受管容器 7 个 / 规则 1081 条 |
| `build/jsdom-smoke.js` | **182/182 通过** |
| `build/verify-refresh-stability.js` | 20 个场景全部 `ghost=0 / sweep=0 / 节点身份全部保留`；与 `refresh-baseline.json` 对比无新增回归（`08c` 的焦点丢失与 −40 滚动差在 11-08 的基线里就存在，属场景固有） |

### 9.4 本轮没做的

- **Wave 0 的运行时基线未采集**：需要运行中的 release 包做 8 秒空闲采样、进程树工作集、
  1 小时唤醒计数，属于发版前的验证动作，不在代码改动范围内。建议在 v0.3.34 打包后补。
- Wave 2 / Wave 3 未动（emit 合并、凭据扫描缓存、隐藏态门控、启动下沉）。
- W1-3（代理快照 TTL）经复核放弃，理由见 §4.1。

### 9.5 发版前必须补的真机验证

1. **失败渠道的短重试时序**：断网后确认渠道仍按 5 / 15 秒、随后 1 / 5 / 15 分钟重试，
   而不是拖到下一轮主取数（这是本次唯一有实质时序风险的改动）；
   → 已由 §9.7 的真实网络抖动（`qoder` 失败 → 5.1 s 后成功）覆盖第一段（5 秒档）。
   15 秒档与后续 1 / 5 / 15 分钟档仍建议手动断网确认一次。
2. **更新期间停取数**：点「立即更新」后，日志里不应再出现新一轮取数；
3. **隐藏 10 分钟再唤回**：数值、胶囊、托盘数字一致；
4. **签到与活动监测**：确认 `direct_client` 单例后仍走直连（不跟随系统代理）；
5. **同步**：GitHub 一轮读写（用户信息 / gist 拉取 / 推送）正常，错误码未变。

### 9.6 运行时观察（2026-11-10 23:24 起，本机）

**新版功能正常**：23:24:28 启动，23:24:29–32 完成首轮取数（约 4 秒），
`trae` 先 401 再换下一份凭据成功、`qoder` / `qoder-cn` / `deepseek` / `hapi` / `codex` /
`4sapi` / `opencode-go` / `workbuddy` 全部 `取数成功`，与旧版行为一致。

| 指标 | 旧版（v0.3.33，已运行一段时间） | 新版（启动 90 秒后） |
|---|---|---|
| 主进程工作集 | 67.9 MB | 42.5 MB |
| WebView2 子进程 | 6 个，396.0 MB | 6 个，377.6 MB |
| 句柄 | 726 | 511 |
| 线程 | 39 | 32 |
| 10 秒空闲 CPU | 0.078 s（8 秒采样） | **0.000 s** |

> 两组不是严格 A/B：旧进程已运行较久、窗口形态/缓存状态不同。这组数字只能说明
> 「新版在这台机器上启动后更轻、空闲确实不空转」，不能当作改动的因果证明。
> 空闲 CPU 从 0.078 → 0.000 与「去掉 1 秒自旋」方向一致，但单次采样不足以定论。

**新发现（与 Wave 1 无关，量级更大）**：新版启动后留下 **2 个 `WorkBuddy.exe`
子进程，各 105.4 MB，合计 210.8 MB**，创建于 23:24:29，直到 23:29 前后才自行退出（约 5 分钟）。

日志给出了比「进程残留」更严重的证据（`diag.log`）：

```text
23:26:14 问不出 at-rest key(...桌面端 15 秒内没有应答)... · 耗时 104714 ms
23:26:43 问不出 at-rest key(...桌面端 15 秒内没有应答)... · 耗时 133845 ms
23:29:24 问不出 at-rest key(...桌面端 15 秒内没有应答)... · 耗时 128466 ms
```

`workbuddy.rs:57` 的 `KEY_TIMEOUT` 是 **15 秒**，但实际单次调用耗时 **104～133 秒**。
根因在 `fetch_key_payload`（`workbuddy.rs:695-738`）：

1. 主线程 15 秒到期后 `child.kill()` + `child.wait()`，然后 `break`；
2. 但紧接着 `h_out.join()` / `h_err.join()` 要等两个读线程；
3. 读线程阻塞在 `read_to_string`，**EOF 要等管道所有写端关闭**；
4. Electron launcher 被杀后，它派生的进程仍持有写端 → EOF 迟迟不来 →
   `join()` 又等了 80～120 秒。

**超时形同虚设**：函数确实在 15 秒时放弃等待子进程，却没有按时返回，
把「超时」变成了「卡满 100 多秒」。连带后果：

- 每次冷启动（`KEY_CACHE` 为空）都有 2～3 个并发调用各卡 100 多秒，
  占住同样数量的 `spawn_blocking` 线程；
- 每个调用挂 2 个 105 MB 的 Electron 进程，存活数分钟；
- 这期间 WorkBuddy 渠道无法解密凭据，只能沿用上次快照。

**处置**：不在 Wave 1 范围内顺手改（涉及子进程生命周期与线程收尾，需单独取证与反复真机验证），
列为下一轮最高优先级候选。修复方向三选一或组合：

1. 读线程改成带截止时间的读（`try_wait` 之外再加一层读超时），超时后不再 `join`；
2. 杀进程树而非单进程（Windows `taskkill /T /PID`，或创建时挂 Job Object 保证整棵树随父退出）；
3. 把 `fetch_key_payload` 整体放进一次性线程，超时后直接返回、不等子进程收尾
   （会泄漏线程，但不再阻塞取数链路）。

推荐 **2 + 1**：既不留进程，也不让收尾阻塞返回值。

> 该项已于同日修复并复验，见 §9.7。

### 9.7 `fetch_key_payload` 超时缺陷：修复与复验（2026-11-10 23:39）

#### 改动

| 文件 | 改动 |
|---|---|
| `app/core/src/workbuddy.rs` | `fetch_key_payload` 的进程调度抽成可单测的 `run_child_with_timeout(child, timeout)` |
| 同上 | 读线程改 **detached**：`std::thread::spawn` + `mpsc::channel` 回传，删除 `thread::scope` + `h_out.join()` / `h_err.join()` |
| 同上 | 到期后先 `kill_process_tree(pid)`（Windows：`taskkill /T /F /PID`，带 `CREATE_NO_WINDOW` 0x0800_0000，stdio 全 null），再 `child.kill()` / `child.wait()` |
| 同上 | 收 stdout/stderr 改为「**剩余预算**内 `recv_timeout`」，超预算即用已收到的部分返回，绝不再无限等 EOF |
| 同上 | 新增 2 个 `#[cfg(windows)]` 单测：`child_output_is_read_back`（`cmd /c echo hello` → `"hello"`）、`child_timeout_returns_within_budget`（`ping -n 20`、800 ms 预算，断言返回错误且耗时 `< 10 s`） |

返回语义逐字未变：stdout 非空即成功；空则 `KeyUnavailable(原始 stderr)`，stderr 也空才是
`"桌面端没有输出"`；超时文案仍是 `桌面端 {} 秒内没有应答`。

`taskkill /T /PID <pid>` 不存在「PID 被回收后误杀新进程」的竞态：调用点紧跟在
`try_wait() == Ok(None)` 之后、且尚未 `wait()`，`Child` 仍持有进程句柄，
Windows 不会回收该 PID。

#### 复验（重编译 release 后同机运行）

| 指标 | 修复前 | 修复后 |
|---|---|---|
| 密钥单次耗时 | 104714 / 133845 / 128466 ms | **2114 ms / 3112 ms** |
| 遗留 `WorkBuddy.exe` 子进程 | 2 × 105.4 MB，存活约 5 分钟 | **0**（进程树只有 `tokenscope` + 6 个 `msedgewebview2`） |
| 渠道结果 | `workbuddy` 只能沿用旧快照 | `workbuddy` / `workbuddy-ai` 均 `取数成功` |
| 第二轮 | — | 23:40:22 复用缓存（`用缓存的 at-rest key(57 秒前取到)`） |

#### 顺带拿到 Wave 1 唯一时序风险点的真机证据

```text
23:40:23.022  qoder: 请求失败: error sending request ... / E_CONNECT ... ms=258
23:40:23.025  qoder: 取数失败 · 凭据 [手动令牌/Qoder · seabird→不通]
23:40:28.149  qoder: 取数成功 · 凭据 [手动令牌/Qoder · seabird→通]
```

失败到重试成功间隔 **5.1 秒**，来自新的「睡到下一次重试到期」，而不是拖到下一轮 60 秒主取数。
这覆盖了 §9.5 第 1 条的正向路径，且是真实网络抖动触发（到 `openapi.qoder.sh` 的偶发连接失败），
不是人为断网。该失败与本轮改动无关，重试即恢复。

#### 门禁

| 门 | 结果 |
|---|---|
| `cargo test --locked`（core） | **425 passed / 0 failed / 8 ignored**（423 + 新增 2） |
| `cargo check --all-targets --locked`（src-tauri） | 通过，4.93 s，0 警告 |
| `cargo build --release` | 通过，5 min 15 s（仅既有的 `corrupt .drectve` 链接器告警） |

#### 遗留观察

- **内存**：修复后运行约 2 分钟采样，进程树 **441.3 MB**（`tokenscope` 50.9 MB + 6 个
  `msedgewebview2` 390.4 MB）。与 §9.6 的 428.6 MB 同量级，WebView2 仍占约 88%，
  与 §1「本轮不承诺降内存」的界定一致。
- **磁盘**：`tokenscope.db` 已 **54.1 MB**（10-27 记录 23.4 MB），`DELETE` 不回收文件页，
  属历史遗留项 P3-14，本轮未动。
- 每次冷启动仍会并发问 2 次密钥（23:39:21.95 同时发起，国际版 / 国内版各一次，
  分别在 2.1 s 与 3.1 s 后返回）。这是既有设计（两个区域各一套 at-rest 凭据，
  `KEY_CACHE` 命中后第二份直接复用），本轮只保证「不再卡 100 秒」。

### 9.8 当前构建状态（写于 23:50，已被 §10 的发版覆盖）

> **后续进展**：00:12 提交 `6552d3b`（版本 0.3.34），00:16 由 `build/build.ps1 bundle` 出正式包 ——
> `app/src-tauri/target/release/Grandettoken.exe` **10,302,464 B**（−122,880 B / −1.18%，
> 主要来自裁掉 `image-png`）、`bundle/nsis/Grandettoken_0.3.34_x64-setup.exe` **5,699,297 B** + `.sig`。
> 二进制里能检索到 `taskkill` / `桌面端没有输出` / `秒内没有应答`，确认 §9.7 的修复进了发布包。
> 下面三条随之失效，仅作「只看 `cargo build` 时的陷阱」留档。

- 只跑 `cargo build --release` 时，产物叫 `tokenscope.exe`（Cargo package name），
  `Grandettoken.exe` **不会**被覆盖 —— 后者只在 `cargo tauri build` / bundle 阶段按
  `mainBinaryName` 生成。所以「看到 exe 时间戳没变」不等于构建失败。
- 只跑 `cargo build` 时安装包不存在，正式安装的版本也不会被替换；升级要靠
  `build/build.ps1 bundle` 出的 NSIS 包。
- `build.ps1` 的 `build` 分支在 PowerShell 5.1 + `$ErrorActionPreference='Stop'` 下会被 cargo 的
  stderr 进度行判成致命错误（`bundle` 分支早已包 `Continue`）。**该缺口已在 §10.2 修掉**。

---

## 10. 实施记录：Wave 2 跳过 · Wave 3 逐项评估与收尾（2026-11-11）

**用户决策**：Wave 2 全部跳过（W2-1 / W2-2 / W2-4 不做）；其余各项按「收益高、风险小」
的标准逐项取证评估，通过的实施，其余明确记录不做的原因。

### 10.1 逐项评估结论（全部带实测或代码证据）

| 项 | 结论 | 依据 |
|---|---|---|
| 启动期 `store.prune` 下沉 | **不做** | **实测推翻 §2.6 的前提**：归档 SQL 实际走覆盖索引 `idx_snap_lookup`（`(provider_id, ts DESC)`，纯 ts 谓词是整条索引扫描而非全表扫描，代价小）。对真实库副本（296,386 行快照、54.1 MB）计时：覆盖聚合 27ms + LAG 聚合 34-44ms，**合计 60-70ms**（缓存热）；`daily_summary` 0 行。未达 §4.3「超阈值才做」的门槛 |
| `autostart::sync` / `startup_banner` 下沉 | 不做 | 注册表读写 + 日志追加，毫秒级，下沉徒增复杂度 |
| `ProxyClients` 惰性构造 | 不做 | TLS 客户端构造约 10-40ms；惰性会把 `Gateway::new` 的失败从**启动横幅**（`lib.rs:6186` startup_error 通道）挪到**首次请求时**浮现 —— 错误浮现时点变化属于行为变化 |
| 自定义 tokio `worker_threads` | 不做 | 方案自评「风险中，需完整基准，最后做」，不满足「风险小」 |
| `check_update` 改 Rust 侧 24h 调度 | 不做 | 改轮询时序 = 行为变化，收益低 |
| `image-png` 裁剪 | **做** | 全项目唯一 `Image` 用法是 `lib.rs:1688` 的 `new_owned`（原始 RGBA），**无任何运行时 PNG 解码调用点**（grep `Image::` / `from_png` / `load_from_memory` / `include_image` 全空）；托盘/窗口图标由 tauri-codegen 构建期内嵌。裁掉后 Cargo.lock 减少 `image` / `pxfm` / `moxcms` / `byteorder-lite` 4 个包 |
| `build.ps1` `build` 分支 PS5.1 缺口 | **做** | §9.8 指出的脚本缺口：`bundle` 分支已有 Continue 包裹（`build.ps1:170-177`），`build` 分支补同款（`$LASTEXITCODE` 为唯一失败判据）。脚本层，零产品风险 |
| Wave 4 磁盘清理 | **做（debug 部分）** | 用户确认：删 `src-tauri/target/debug`（45 GB）+ `core/target/debug`（15 GB），G 盘可用空间增至 434 GB；`target/release` 21 GB 按 RELEASE.md 保留；残留 `release-pre-bundle-*`（20 MB）/ `x86_64-pc-windows-msvc`（30 MB）用户未选，保留 |
| `tokenscope.db` 54 MB（P3-14） | 不做 | VACUUM 动用户数据，风险不可控；296k 行下归档链路 60-70ms 不是瓶颈，继续观察 |

### 10.2 代码改动

| 文件 | 改动 |
|---|---|
| `app/src-tauri/Cargo.toml` | tauri features 去掉 `image-png`（附理由注释）；版本号 0.3.34 |
| `build/build.ps1` | `build` 分支按 bundle 分支同款处理 PS 5.1 native-stderr 陷阱 |
| `app/src-tauri/tauri.conf.json`、`Cargo.lock` | 版本号三处同步（`cargo fetch`） |

### 10.3 门禁（在 §9.3 / §9.7 基础上，本批改动后全量重跑）

| 门 | 结果 |
|---|---|
| `cargo test --locked`（core） | **425 passed / 0 failed / 8 ignored** |
| `cargo check --all-targets --locked`（src-tauri） | 通过，0 警告 |
| `cargo test --release bench_history_batch -- --ignored` | ok |
| `node --check app.js` | 通过 |
| `build/check-css-forms.js` | PASS，规则 1081 条 |
| `build/jsdom-smoke.js` | **182/182** |
| `verify-refresh-stability --compare`（对 `refresh-baseline.json`） | 20 场景无新增回归（08c 的 −40 滚动差为基线固有）；产物 `build/qa/refresh-20261111-v0334.json` |
| `verify-tray.js` | **128/128** |

### 10.4 运行时补充证据（§9.7 修复后的实例持续运行至发版前）

- `4sapi` 23:57:44.734 取数失败（请求没发出去）→ 23:57:48.442 重试 —— **5 秒档再次真实命中**；
- 修复后 diag.log **无任何新的 >100 s** 密钥耗时记录（23:31 之后全部正常）；
- §9.5 剩余人工项（不影响本批发版，升级后继续观察）：15s / 1min / 5min / 15min 档手动断网确认、
  更新期间停取数、隐藏 10 分钟唤回、签到直连、同步一轮读写。

### 10.5 发版后基线采样（已补，2026-11-11 00:24）

按用户要求从 1 小时缩短为 **5 分钟**：release 产物（`Grandettoken.exe`，v0.3.34）启动后
**10 × 30 秒**自动采样（主进程 `TotalProcessorTime` 增量 + 进程树 `WorkingSetSize`），
产物 `build/qa/perf-baseline-2026-11-11.json`。

| 指标 | 结果 | 对照 |
|---|---|---|
| 进程树工作集 | 431.3 → 439.7 MB，末值 438.6 MB | 基线 428.6 MB 的 **+2.3%**，低于 +5% 上限；WebView2 6 个约 390 MB，仍是大头（§1 的界定不变） |
| 主进程 | 43.7 - 45.5 MB | 与 §9.6 的 42.5 MB 同量级 |
| CPU（主进程，30 秒窗口） | 10 窗口中 **4 个精确 0%**，其余 0.05 - 0.89%（对应取数轮，含启动后首轮与缓存预热） | 空闲窗口不再自旋，与 §9.6 的 8 秒 0 采样方向一致；5 分钟均值不代表长期均值 |

与 §10.6（同一实例、同时段的并行人工采样，整树 8 秒口径）结论一致：内存基线 +2% 上下、
空闲接近零、零 `WorkBuddy.exe` 残留。仅 5 分钟样本，够复核「空闲不空转 +
内存不高于基线 +5%」，不足以支撑 Wave 3 的启动类决策 —— 那批本来就因收益不成立而全部搁置了。

### 10.6 发版后基线实测（v0.3.34 正式包，2026-11-11 00:17）

跑的是 `build.ps1 bundle` 出的 `Grandettoken.exe`（10,302,464 B，含 §9.7 修复）。
（注：本节数字是同时段的人工采样，t+60 / t+293 两点；盘上的
`build/qa/perf-baseline-2026-11-11.json` 是 §10.5 的 10×30 秒采样器产物，两口径并存。）

| 指标 | t+60 s | t+293 s | 对照（旧版 v0.3.33，§9.6） |
|---|---|---|---|
| 进程树工作集 | 432.3 MB | 433.2 MB | 428.6 MB |
| 主进程工作集 | 44.4 MB | 44.1 MB | 67.9 MB |
| 主进程线程 | 37 | 27 | 39 |
| 主进程句柄 | 536 | 502 | 726 |
| 进程树线程 | 226 | 213 | — |
| 进程树句柄 | 3,726 | 3,671 | — |
| 8 秒内 CPU（整棵树） | 0.188 s | **0.062 s** | 0.078 s（8 秒采样） |

- **内存**：进程树 +0.9%（432.3 vs 428.6），在 §2.1 定的 +5% 以内；主进程 −35%
  （44.4 vs 67.9）。WebView2 仍占 88%，再次印证「降内存」在本项目不成立。
- **空闲**：t+293 的 8 秒只烧 0.062 秒 CPU（≈0.8% 单核），比旧版 0.078 秒略低；
  但这台机器上没有旧版可对照跑同一时刻，差值不足以单独证明「精确唤醒」的因果，
  只能说「没有变差」。
- **密钥链路**：本次会话 00:17:23.464 启动 → 00:17:23.937 / 00:17:23.969 两份 at-rest key
  分别 **211 ms / 243 ms** 返回（同一台机器，修复前是 104714 / 133845 / 128466 ms）。
  这是发布包里的实测，不是开发构建。
- **无残留**：t+60 与 t+293 都是 7 个进程（1 主 + 6 个 `msedgewebview2`），
  没有任何 `WorkBuddy.exe` 子进程。

> 采样脚本口径：`cpu_8s` = 进程树内所有进程 8 秒窗口的 `KernelModeTime+UserModeTime` 增量之和；
> 工作集取 `Win32_Process.WorkingSetSize`。与 §9.6 的旧版数字同口径，但**不是同时刻对照**。

至此本批（Wave 1 + `fetch_key_payload` 修复 + `image-png` 裁剪 + 脚本修复）的代码、门禁、
发版包与发版后基线都已闭环。剩余欠账只有 §9.5 的人工项，不影响 v0.3.34 已发。



