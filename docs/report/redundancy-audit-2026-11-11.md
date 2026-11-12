# PC 端冗余 / 无效代码排查清单

> 排查日期：2026-11-11 · 复核日期：2026-11-11 · 代码基线：v0.3.35（commit `3bc4d8d`，工作树除本报告外干净）
> **本次排查与复核均未修改任何源码、配置或构建脚本**；复核仅修订本报告文字。
> 结论：**已二次确认 21 组冗余点**；另有 6 项需人工裁决，11 类误报已剔除。
> **复核结论（见第八节）**：21 组中 20 组可按 A 类清单执行；#9 须先裁决 B3 再动、#12 须查清保留意图后再定。相关处置与第七节执行顺序已按复核结果修订。

---

## 一、范围与判定口径

| 项 | 说明 |
|---|---|
| 纳入 | `app/core`（`app/core/src/**/*.rs`，36.3k 行）、`app/src-tauri`（同口径 9.1k 行）、`app/src`（app.js / index.html / styles.css，11.7k 行）、`build/`（构建与验证脚本）、`Cargo.toml`、`tauri.conf.json`、`.github/workflows` |
| 排除 | `android/`（手机端，独立 Kotlin 工程）、`docs/`（设计稿/报告）、`app/*/target`（构建产物）、`build/qa` 抓包数据（非代码） |
| 「冗余/无效」定义 | ① 定义后运行时无任何引用；② 恒为假/真的死分支；③ 功能已被新实现取代的旧实现；④ 注册了却没有调用方的 IPC 命令；⑤ 只写不读的配置字段；⑥ 被后续同名规则完全覆盖的样式声明 |
| 不算 | 单元测试函数（由 test harness 发现）、文档留档、按设计显式保留的兼容代码、留作协议镜像的常量表 |

## 二、方法与二次确认

1. **编译期**：`cargo check --all-targets --locked`（MinGW GNU 工具链）分别跑 core 与 src-tauri —— **两者均 `EXIT=0`，0 个 `dead_code` / `unused_*` 警告**（复核时重跑确认）。前置条件：`gcc.exe` 必须在 `PATH` 中（本机为 `C:/Users/wacil/mingw64/bin`），否则 `libsqlite3-sys` 构建直接失败、检查无法完成。说明冗余全部落在编译器看不见的层面：`pub` 符号、Tauri 命令注册、前端字符串引用、CSS 级联。
2. **静态扫描**：提取 1922 个 Rust 符号、308 个 JS 顶层函数、476 个 CSS 类、HTML id、`data-act`、CSS 自定义属性、114 处 `invoke()`、Tauri 命令定义/注册三角比对、依赖声明、静态资源引用。复核重跑 IPC 三角：114 处 `invoke()` → 90 个唯一命令名，注册 94 个，**注册而未调用恰好 4 个（正是 A2 的 #9–#12）**，无「调用未注册命令」，A2 无漏报。
3. **二次确认**（逐项剔除误报）：
   - 全仓交叉 grep（含 `android/`、`contract/`、文档），排除跨端协议与文档提及导致的误判；
   - 排除动态调用路径：114 处 `invoke()` **全部是字符串字面量**；app.js 中 `window[` / `eval(` / `new Function` / `globalThis[` **均为 0**；`class="${...}"` / `classList.add` 全部检查过值域；`<use href="#${icon}">` 的实参全为字面常量；
   - 排除「同名子串」误命中（如 `cklogin` 里的 `cklog`、`setBackdropHidden` 里的 `ckd`、CSS 十六进制颜色被当成 id 选择器）；
   - 排除 457 个 `#[test]` 测试函数（零显式引用属正常），并核对 `#[cfg(test)]` 边界；
   - 前端候选由独立第二路全文通读复核，两路结论一致。

---

## 三、A 类 · 已二次确认（可直接安排清理）

### A1. Rust 逻辑层

| # | 位置 | 冗余内容 | 确认证据 | 建议 |
|---|---|---|---|---|
| 1 | `app/core/src/fetch.rs:1397, 1467-1484, 3995-3999` | `set_desktop_reader` / `disable_desktop_reader` / `NO_DESKTOP_READER` / `_init_desktop_off` 整条测试开关链 | 两个函数挂 `#[cfg(test)] #[allow(dead_code)]`，注释自承「目前没有用例需要打开它」；唯一调用方 `_init_desktop_off` 本身零调用；`NO_DESKTOP_READER` 只被这条死链引用。**`DESKTOP_READER` 静态本身在 1410 行有真实读取，必须保留** | 删 4 处约 22 行，保留静态与其读取路径；**复核补充**：`fetch.rs:1388`、`3996` 两处注释仍写「用例自己 `set_desktop_reader`」，实际全仓零用例调用，须一并修正，避免留下新的误导性文档 |
| 2 | `app/core/src/diaglog.rs:524-529` | `pub fn set_session()` | 注释「显式设定会话 id(测试用)」，但全仓（含测试）零引用 | 删 |
| 3 | `app/core/src/models.rs:98-100` | `is_openai_chat()` | 零引用；`OpenAiChatCompletions` 变体在 12 处被直接 `match` | 删 |
| 4 | `app/core/src/store.rs:1086-1096` | `series_fp()` | 零引用，只是 `series_scoped(..., Some(key_fp), ...)` 的一层转发 | 删 |
| 5 | `app/core/src/store.rs:1699` | `vault_ref_channels()` 方法 | 仅 **5 处**单元测试调用（2065/2066/2075/2078/2090），生产无调用；Rust 侧反查走 `lib.rs:3181 ref_channels_by_pointer` | 与单元测试一并删（或保留为纯测试夹具，二选一）。**复核补充**：`vault_ref_pairs` / `vault_ref_map` 这条索引链仍在使用，删的只是这一个便捷方法，不要把整个索引当死代码 |
| 6 | `app/core/src/sync/builder.rs:417-423` | `sum_amounts()` | 零引用；注释「给手机端做合计重算用」，但 Android 是独立 Kotlin 实现，不链接 Rust | 删（内部 `sum_decimal_strings` 仍被使用，保留） |
| 7 | `app/core/src/sync/code.rs:169-172` | `new_sync_code()` | 零引用；生产路径在 9 处直接调 `SyncCode::new(...)` | 删 |
| 8 | `app/core/src/workbuddy.rs:1571-1577` | `pub struct Attempt` | 类型及其 3 个字段全仓零引用 | 删 |

### A2. Tauri 命令与配置

| # | 位置 | 冗余内容 | 确认证据 | 建议 |
|---|---|---|---|---|
| 9 | `app/src-tauri/src/lib.rs:2866-2890` + `app/core/src/config.rs:142-143, 283-284` + `lib.rs:299-303` | `save_window_size` 命令 + `Config.win_w` / `win_h` | 命令前端零调用（114 处 `invoke()` 全是字面量，无此串）；两个字段只有「定义 / 默认值 / set_config 合并保留 / 命令写入」，**无任何业务读取点**（299-303 读 `cur.win_*` 只是合并时的自我复制）。app.js:3810 注释已写「不再有用户拖拽 resize」，固定尺寸改版后的整套遗留 | **⚠️ 须先裁决 B3 再动**：命令除 `w` / `h` 外还有第三个参数 `compact_max_w`（2873、2883-2884 写入 `c.compact_max_w`），它是 B3 的唯一写入路径。若 B3 决定**保留**该配置，本条只能删 `win_w` / `win_h` 相关部分（字段、默认值、299-303 合并分支、命令内 w/h 分支），**必须保留 305-306 的 `compact_max_w` 合并分支**并另给写入口；若 B3 决定**废弃** `compact_max_w`，才可整条删命令 + 该字段 + 305-306。注释「前端在 resize 结束 250ms 防抖后调用」与现状不符，同步修 |
| 10 | `app/src-tauri/src/sync_cmds.rs:64-72, 396-407` | `sync_github_status` 命令 + `GithubStatus` 结构体 | 命令前端零调用；`SyncStatus`（75-97 行）已含 `has_github` / `github_login` / `github_user_id` / `oauth_client_configured`，与本结构体 4 字段语义等价（复核核对构造逻辑：`has_github = github_token_opt().is_some()`、`oauth_client_configured = oauth_client_id(&cfg).is_some()`），且前端确实走 `invoke("sync_status")` | 删命令、结构体及注册项 |
| 11 | `app/src-tauri/src/sync_cmds.rs:1262-1290` | `sync_delete_relay` 命令 | 前端零调用、无 UI 入口（app.js 全文无 `delete_relay` / 删除中转字样）、Rust 内部也无调用；底层 `GistClient::delete_gist` 在 `publish.rs:272/279/292` 仍被内部使用 | 只删命令包装与注册项，**保留** `delete_gist` |
| 12 | `app/src-tauri/src/lib.rs:657-663` | `vault_ref_channels` 命令 | 前端零调用；同一底层 `ref_channels_by_pointer` 已被 `vault_delete`（3268 行）内部调用，功能已内置。命令体本身就是 `ref_channels_by_pointer`（**扫真实指针、不查索引**）；注释首句「密钥箱列表与删除前拦截都要它」确与「没人调用」矛盾，但同段已有 N30 备注自承「以前注册了却没人调……实现却在浏览器里自己过滤」——即**开发者明知无调用方仍保留注册**，保留意图未查清。历史报告 `docs/report/bug-report-2026-10-21-full-audit.html` 与 `docs/product/product-vault-ref-reconcile-2026-10-21.md` 早已点名 | **复核后转为需裁决**：先查发版记录 / 设计稿，确认是否刻意留作外部或未来入口；确认无保留意图后才删命令与注册项，并修正注释首句。不要按「纯死命令」直接删 |

### A3. 前端 JS

| # | 位置 | 冗余内容 | 确认证据 | 建议 |
|---|---|---|---|---|
| 13 | `app/src/app.js:99, 105, 1490, 1956, 3796, 5104` | 6 个零调用顶层函数：`daysUntil` / `expClass` / `scheduleRowReveal` / `otherMoneyTxt` / `savePosSoon` / `syncDisconnect`（合计 52 行） | 每个名字在 app.js + index.html + styles.css 全文**只出现 1 次**（定义行）；无 `window[...]` / `eval` / `new Function` / 对象方法表 / HTML `onclick`。复核补扫 `build/`、`contract/`、Rust 源码与设计稿，除历史文档留档的提及外亦无调用。其中两个有明确替代实现：`savePosSoon` → `scheduleSettleCheck` 内联（4131-4135）；`syncDisconnect` → `syncGithubDisconnectGo`（5261-5270，走 `armConfirm`） | 6 个函数全删 |
| 14 | `app/src/app.js:1489` | `const rowRevealTimers = {}` | 全文 3 次出现，全部在已死的 `scheduleRowReveal` 内部（1491/1492） | 随 #13 一并删 |
| 15 | `app/src/app.js:2524` | `const PILL_ROTATE_MS = 8000` | 全文仅 1 次；5516-5517 行注释明确「轮播节奏用 setTimeout 链…读 `pillDwellSec` 配置，默认 12 秒，原写死 8 秒」 | 删（含 2522-2523 的过时注释） |
| 16 | `app/src/app.js:3695, 4115, 5491` | `RESIZING` 恒为 false 的死分支 | 全篇 3 处：定义 + 两处读取，**除初始化赋值外无任何赋值**（无 `=` / `++` / `+=` / 解构 / 间接赋值）；注释自承「固定尺寸下恒为 false」。两处都在逻辑或链中且无副作用，删除后真值完全不变（对照组 `SIZE_APPLYING` 有 6 处赋值） | 删变量与两处条件项；行为零影响。若将来恢复拖拽缩放再引入 |

### A4. HTML

| # | 位置 | 冗余内容 | 确认证据 | 建议 |
|---|---|---|---|---|
| 17 | `app/src/index.html:23` | `<symbol id="i-check">` 整个元素 | 全文 `i-check` 仅 1 次（定义处）；所有 `<use href="#i-…">` 引用集合不含它；`app.js:4584` 的 `<use href="#${icon}">` 实参在 4588-4598 全为字面常量，无 `i-check`；`${railIcon}`（1017）值仅 `i-clock`/`i-info`。symbol 唯一用途就是被 `<use>` 引用 | 删整个 `<symbol>` |
| 18 | `app/src/index.html:32, 133, 493` | 3 个冗余 `id` 属性：`prevLine` / `cForm` / `syncChannelsHint` | 元素本身有效且按 class 使用：`.prevline`（CSS 985-988，app.js:5484-5485 拨 `dock-prev-*`）、`.cform`（CSS 1357-1360，内部字段由 `$()` 读写）、`.khint`（CSS 450，静态说明文案）。JS/CSS 均不按这 3 个 id 取元素 | 仅删 `id` 属性，保留元素（低优先级） |

### A5. CSS

| # | 位置 | 冗余内容 | 确认证据 | 建议 |
|---|---|---|---|---|
| 19 | `app/src/styles.css` 共 17 处 | 17 个零引用类规则：`.cksi`(1208-1211)、`.ckd`(1192)、`.cklog`(1190-1191)、`.core-empty`(265)、`.ea/.ed`(254-257, 261-262)、`.rn`(169)、`.pf`(1312-1317)、`.pcount`(1316)、`.mdlprev`(1127-1128)、`.quota-label`(199, 802)、`.expiry-empty`(1712)、`.expiry-account-note`(1714)、`.expiry-link`(1716)、`.promo-detail`/`.promo-description`(1782)、`.tb.spin`(115) | 17 个名字在 app.js / index.html 全文**独立词 0 命中**，`class="[^"]*\b…\b` 0 命中；逐一核对渲染函数后都有现行替代实现（签到中心 `ck-*` 系、到期区 `expiry-footnote`/`expiry-entry`、密钥箱 `vgroup`/`vitem`、promo 列表系、刷新按钮 `loading`/`done`）；动态拼接值域不含这些名字。复核补扫 `build/` 验证脚本：仅 `verify-codex-layout.js` 用 `.quota-label` 作 querySelector（针对其自注入 fixture，真实 DOM 无此元素），删规则不影响断言。**注意 `@keyframes spin`(108) 仍被 104 行使用、`@keyframes pulse`(116) 仍被 955 行使用，两者都不能删** | 删 17 组规则，但：① `.rn`（169 行 `.sum .vv .rn`）与 B5 旧摘要段是同一条规则，**合并处理、勿重复动手**；② 只删 `.tb.spin` 这一条，别动 keyframes —— 但 `pulse` 的唯一使用方是 955 行 `.tb.loading`，**若 B6 裁决删它，`@keyframes pulse` 应一并删** |
| 20 | `app/src/styles.css:15-29, 1501-1517` | 15 个零引用自定义属性：`--bg4`、`--fs-display/value/body/meta/cap`、`--lh-display/value/body/meta/cap`、`--lime`、`--raised`、`--shadow`、`--surface` | 全篇 `var()` 引用 0 次，JS 只 `setProperty("--u")`（app.js:50），HTML/Rust 均不引用。属 V2 改版后遗留的设计变量。复核补充：`--bg4` 有 3 处定义、`--lime/--raised/--shadow/--surface` 各 2 处（含 `body.light` 主题覆盖），删除时要覆盖全部定义点 | 删定义，保留 `--u` 与在用的主题变量 |
| 21 | `app/src/styles.css:181` | `.rhead` 的 `padding` 声明被 1372 行完全覆盖 | 181 行 `padding:9px 12px 8px`；1372 行 `padding:5px 12px` 后加载且同特异性 → 前者永远不生效（903/922 行追加的是 `position`/`transition`，不冲突，保留） | 删 181 行的 padding 声明 |

**A 类粗估**：约 250–350 行源码、1 个 SVG symbol（不含 B4 的依赖声明与第六节的非代码资产）；另需同步修正 4 处过时注释（#1 两处、#9、#12）。

---

## 四、B 类 · 需人工裁决（不建议直接删）

| # | 位置 | 情况 | 为什么不能直接判死 |
|---|---|---|---|
| B1 | `app/core/src/sync/error.rs:22-23, 58-60` | `NOT_CONFIGURED` / `NO_KEY` 常量、`SyncError::code_only()` 在 PC 端零引用 | 它们是**跨端同步协议码表**：`contract/README.md:356-357`、`android/.../SyncException.kt:26` 有同名定义；`sync_cmds.rs:478` 的 `code: "SYNC_NOT_CONFIGURED".into()` 是码值硬编码，建议**保留常量并改成引用常量**。复核更正：`sync_cmds.rs:416` 是面向用户的提示文案（`"SYNC_NOT_CONFIGURED: 桌面 OAuth App 未配置…"`），不是码表重复定义，不应改写。这是重复定义问题，不是删除问题 |
| B2 | `app/core/src/workbuddy.rs:418-423` | `clear_key_cache()` 零引用 | 注释写「测试与『换了账号』时用」，但换账号/清登录路径并未调用它 → **可能是漏调用导致凭据缓存不失效的功能缺陷**，而非纯冗余。需先判断是补调用还是删 API |
| B3 | `app/core/src/config.rs:146` + `app.js:3709` | `compact_max_w` 前端有读、无写 | 前端读的是 camelCase `CFG.compactMaxW`（app.js:3709），唯一写入路径是已死的 `save_window_size`（其第三参数 2883-2884），设置页也没有入口 → 用户实际上改不了这个值。属「半死配置」，需决定是补设置入口还是连同字段废弃。**⚠️ 本项必须先于 A2 #9 裁决**：若按 #9 先整条删掉命令，唯一写入口会永久消失，「补设置入口」将从改前端变成重建命令；#9 的删除范围由本项结论决定 |
| B4 | `app/core/Cargo.toml:24` | `generic-array = "0.14"` 源码零引用 | 唯一 grep 命中是测试函数名 `accepts_generic_array_shape`。复核用 `cargo tree -i generic-array` 确认 0.14.7 经 `aead` / `cipher` / `digest` 等**多条传递路径**引入（非 `aes 0.8` 直接传递），删掉直接声明后 Cargo 仍解析 0.14、类型仍一致；但**必须 `cargo check --locked` + `cargo test` 验证后再移除** |
| B5 | `app/src/styles.css:132-165` | 旧版三格摘要整段（约 30 条规则）被 1532-1565 新版同名选择器覆盖 | 大部分声明确属无效，但旧段有独有规则（`.sum .risk-cell:focus-visible`、`.sum .vv .rn` 等）且级联特异性有细微差别，**需逐条视觉核对后删**，不要整段盲删。复核补充：其中 `.sum .vv .rn` 已由 A5 #19 判定 DOM 零引用，可随本段核对一并处理，不要按两个条目重复动手 |
| B6 | `app/src/styles.css:955` | `.tb.loading{animation:pulse…}` 是否曾匹配真实元素 | 文本证据倾向「否」（刷新按钮是 `.tbi`、JS 只加 `loading` 给 `#btnR`），但涉及运行时形态，建议实测后再定。**若判否并删除，需联动删 `@keyframes pulse`**（见 A5 #19） |

---

## 五、已剔除的误报（二次确认的价值）

> 表格共 **11 项**。原摘要写「8 类误报」，复核按实际条目数更正为 11 类。

| 初筛候选 | 剔除理由 |
|---|---|
| 457 个 Rust「零引用」函数 | 全是 `#[test]` / `#[cfg(test)]` 单元测试函数，由 test harness 自动发现，不能算死代码 |
| `app/core/examples/probe_wbai.rs`、`gen_sync_fixture.rs` | 开发期探针/夹具生成器；后者仍在生成 `contract/fixtures/sync-v1.json` |
| `build/verify-*.js` 中「当前前端找不到的 id」（`btnMenu`/`hmenu`/`rzGrip`/`rzFrame`/`btnS`/`lkReset`/`grid` 等） | **是断言这些元素不存在**，例如 `verify-settings-entry.js:46-67` 明确检查旧按钮已移除、`verify-resize.js:43-44` 检查拖动缩放入口已移除；`grid` 是脚本自己注入的 DOM。脚本有效，不是过时死代码 |
| CSS 里 60+ 个 `#xxxxxx` 「无对应 id 选择器」 | 是十六进制颜色值 |
| `.ckd` / `.cklog` 的 JS 命中 | 分别是 `setBackdropHidden`、`cklogin` 的子串 |
| `app.js` 中 13 个 `let X = false` | 逐一核对均有读写（如 `SIZE_APPLYING` 有 6 处赋值）；只有 `RESIZING` 例外 |
| 40 组重复 CSS 选择器 | 多数是不同属性分层追加或 `body.light` / `layout-single` 主题覆盖，属合理组织；仅 `.rhead` 的 padding 确属被覆盖 |
| `proxy.rs` 等模块 | `gateway.rs:17` 引用 `proxy::ProxyClients`，模块有效 |
| `peak.rs:79` 的 `days_from_civil` | 显式 `#[cfg(test)]` 且被测试使用，注释已说明用途 |
| Tauri 事件 `emit` ↔ 前端 `listen` | 全部配对，无单端孤儿事件 |
| `data-act` 69 个动作值 | 全部有 JS 分支处理，无孤儿动作、无死 handler |

---

## 六、非代码资产冗余（可选，不属于「无效代码」）

| 位置 | 情况 |
|---|---|
| `build/qa/refresh-*.json` **7 份** | 各约 1.2 MB 的抓包留档（含 `refresh-summary.json`），**已被 git 跟踪**，合计约 **8.5 MB**；另有多份 `.apk` / 截图。属留档数据，可考虑移出版本库 |
| `app/src/brand/logo.png`（11.8 KB） | `build/gen-icons.js:215` 生成，但前端只引用 `mark.png` / `mark-ink.png` / `logo-tray.png`，此文件零引用 |
| `build/dl/winlibs.zip`（233 MB）、`rustup-init.exe`（14 MB） | 本地工具链缓存，**已 gitignore**，不占仓库体积，无需处理 |
| `build/release-notes-v*.md`（30 份）、`docs/design/*.html`（21 份） | 历史发布说明与设计稿留档，项目约定保留 |

---

## 七、建议处理顺序

> 复核后重排：**B 类裁决前置**，因为 B3 的结论直接决定 A2 #9 能删到什么程度。

0. **先裁决再动手**：B3（`compact_max_w` 去留）与 A2 #12（`vault_ref_channels` 注册意图）必须先定；B 类其余 4 项也在动 A2 前过一遍。B3 若决定「补设置入口」，#9 不得整删命令。
1. **零风险先做**：A3（JS 死函数/常量/恒假分支）、A4（HTML）、A5（CSS）——纯前端、无 ABI、无数据兼容问题，删完跑 `node --check app.js` + `build/check-css-forms.js` + `build/jsdom-smoke.js`；A5 动过 `.quota-label` 等类后建议补跑 `build/verify-codex-layout.js`（它读真实 styles.css 做布局断言）。
2. **低风险**：A1（Rust 私有/公开但零引用的函数与类型）、A2 的 #10 / #11 两个与配置无关的 IPC 命令——删完跑 core `cargo test --locked`（当前 421 passed / 8 ignored）与 src-tauri `cargo test`、`cargo check --all-targets`（注意 gcc 前置条件，见第二节）。
3. **按裁决结果处理 A2 #9 / #12**：#9 的两种删除范围见该行表格；`win_w` / `win_h`（以及若 B3 决定废弃时的 `compactMaxW`）字段删除后，老配置里的这些字段会被 serde 默认忽略，不影响启动，但仍建议随版本发布说明一句。
4. **回归门禁**：`node --check app.js`、CSS 形态门（1079 条）、jsdom 冒烟（177/177）、`build/verify-release.js` 的发版链路检查。

---

## 八、复核记录（2026-11-11 二次复核）

> 由独立第二路复验，**只修订本报告文字，未改任何源码**。范围：A/B 类全部条目的证据、方法学前提、自述数字与处理顺序。

### 8.1 实测通过项

| 复核项 | 结果 |
|---|---|
| 基线 | `3bc4d8d` / v0.3.35；工作树仅多出本报告，与报告自述一致 |
| 编译期零警告 | core 与 src-tauri `cargo check --all-targets --locked` 均 `EXIT=0`、无 `dead_code`/`unused_*` 警告（补 gcc 到 PATH 后） |
| IPC 三角 | 114 处 `invoke()` 全为字面量 → 90 个唯一命令名；注册 94 个；注册未调用恰好 4 个 = A2 #9–#12；无「调用未注册命令」，**A2 无漏报** |
| 动态调用路径 | `window[` / `eval(` / `new Function` / `globalThis[` 均 0；`<use>` 实参全为字面常量 |
| A3 / A4 | 6 个函数定义行号、`RESIZING` 仅 1 处初始化赋值、4 个 id 零引用、`<use>` 值域，全部属实；补扫 `build/`、`contract/`、Rust 源码、设计稿，除历史文档留档外无调用 |
| A5 | 17 个类名在 app.js + index.html 独立词 0 命中；15 个自定义属性 `var()` 0 引用；`.rhead` 181 行 padding 确被 1372 行同特异性覆盖 |
| 过程数字 / 门禁 | 457 个 `#[test]`、421 passed / 8 ignored、CSS 形态门 1079 条、jsdom 177/177，与 `PROGRESS.md` 一致；报告引用的两份 2026-10-21 历史报告均存在 |
| 非代码资产 | `logo.png` 前端零引用、`build/dl/` 已 gitignore、`refresh-*.json` 确被 git 跟踪 |

### 8.2 复核发现的问题与本次修订

| # | 问题 | 修订位置 |
|---|---|---|
| 1 | `save_window_size` 还写 `compact_max_w`，A2 #9 未提，B3 又把它当唯一写入路径；按原顺序执行会永久删掉写入点 | A2 #9、B3、第七节顺序、第二节粗估 |
| 2 | #12 证据「也不查索引」与当前实现相反；注释已有 N30 自述「明知没人调仍保留注册」，意图未查 | A2 #12 转为需裁决、第七节第 0 步 |
| 3 | `.rn`（169 行）在 #19 判可删、B5 又列为需视觉核对的独有规则 | A5 #19、B5 互相标注「合并处理」 |
| 4 | `@keyframes pulse` 的唯一使用方 955 行正是 B6 待裁决对象，删除存在联动 | A5 #19、B6 互相标注 |
| 5 | B1 的 416 行是用户提示文案而非码值硬编码 | B1 只保留 478 行 |
| 6 | A1 #5 测试调用是 5 处非 6 处；索引链仍在使用 | A1 #5 |
| 7 | A1 #1 遗漏 `fetch.rs:1388` / `3996` 两处过时注释 | A1 #1、A 类粗估 |
| 8 | 汇总数字：22 组 → 21 组（表格实际行数）；8 类误报 → 11 类；core 36.9k → 36.3k（注明口径）；A 类粗估混入 B4 与第六节资产 | 摘要、第一节、第五节、A 类粗估、第六节 |
| 9 | `refresh-*.json` 实际 7 份 / 8.5 MB；`generic-array` 是经 `cipher`/`aead` 多路径传递而非 aes 直接传递 | 第六节、B4 |
| 10 | #9「无任何读取点」不严谨（299-303 有合并时自我复制读）；#16「无任何赋值」不严谨（有初始化）；#10「完全等价」补上构造逻辑证据 | A2 #9 / #10、A3 #16 |
| 11 | 编译检查缺 gcc 前置条件说明；#19 的 `.quota-label` 在 `verify-codex-layout.js` 被用作选择器（不影响结论） | 第二节、A5 #19、第七节门禁 |

### 8.3 结论

- **A 类 21 组中 20 组可按清单执行**；#9 为「先裁决 B3 再按两种范围之一删」、#12 为「先查保留意图再决定删否」。
- **B 类 6 项裁决整体前置到 A2 之前**，其中 B3 与 #9 强耦合、B6 与 #19 的 `@keyframes pulse` 联动。
- 复核未执行任何删除；报告仍然只做识别、确认与裁决建议。

---

## 九、执行记录（2026-11-12）

> 依第七节顺序执行完毕，删除前逐项复核证据与报告一致。**净变更：+41 / −384 行（18 个文件）**。

### 9.1 人工裁决结果

| 项 | 裁决 |
|---|---|
| B3 + A2 #9 | **整条废弃**：删 `save_window_size` 命令、`win_w`/`win_h`/`compact_max_w` 字段与合并分支；前端 `CFG.compactMaxW` 读取随之移除（`compactMaxPx` 固定用默认 380·u，行为与未设置过该值的用户一致） |
| A2 #12 | **删除** `vault_ref_channels` 命令与注册项，并修正过时注释 |
| B2 | **删除** `clear_key_cache()`（KEY_CACHE 有 TTL 自过期，换账号场景从未出过实际故障） |
| B5 | **暂缓**（旧摘要段 132-170 留待下一批逐条视觉核对） |
| B6 | **删除** `.tb.loading` + `.tb.spin`，并联动删除唯一使用方已消失的 `@keyframes pulse` |
| B1 | 按 报告建议执行：`sync_cmds.rs` 的 `SYNC_NOT_CONFIGURED` 硬编码改为引用 `sync::error::NOT_CONFIGURED` 常量；`code_only()`（零引用）一并删除；协议码表常量保留 |

### 9.2 执行明细

- **A1（8 组）**：#1–#8 全部删除。#1 超出报告范围的一点：`DESKTOP_READER` 静态与读取分支也一并删除 —— 注入函数删除后该锁永为 `None`，保留只会留下恒假死分支（定义②）；测试隔离由 `cfg!(test)` 早退承担，理由并入原注释。#5 的 5 处测试断言改用现役 `vault_ref_pairs()` 表达，测试意图与覆盖不变。
- **A2（#9–#12）**：按 9.1 裁决全部删除。
- **A3/A4/A5**：#13–#21 全部删除，含 `--shadow`/`--surface`/`--raised`/`--lime`/`--bg4`/`--fs-*`/`--lh-*` 的**全部**定义点（含 `body.light` 覆盖处）；`.rhead` 仅删 padding 声明、保留 `cursor:pointer`；802 行选择器组只删 `.quota-label` 半边（`.quota-secondary` 仍在用）。
- **报告漏列的同类项**：`resize_hint_seen`（配置字段，零读写，随 #9 一并删除）。
- **B4**：`generic-array = "0.14"` 已从 `app/core/Cargo.toml` 移除；`cargo check/test --locked` 通过，Cargo.lock 相应更新（0.14 仍经 `aead`/`cipher`/`digest` 传递存在，类型不变）。

### 9.3 回归门禁结果

| 门禁 | 结果 |
|---|---|
| `node --check app.js` | PASS |
| `build/check-css-forms.js` | PASS（7 容器 / 1052 条规则） |
| `build/jsdom-smoke.js` | **182/182 通过** |
| `build/verify-codex-layout.js` | **48/48 通过**（见 9.4） |
| `build/verify-refresh-stability.js` | EXIT=0，节点全部保留 |
| core `cargo check --all-targets --locked` | EXIT=0，无 `dead_code`/`unused_*` 警告 |
| core `cargo test --locked` | **425 passed / 0 failed / 8 ignored** |
| src-tauri `cargo check --all-targets --locked` | EXIT=0，零警告 |
| src-tauri `cargo test` | **19 passed / 0 failed** |
| IPC 三角复验 | 114 处 `invoke()` → 90 个唯一命令，全部已注册；被删 4 命令零残留 |
| 被删符号全仓残留扫描 | 仅余有意保留的说明注释与 `cklogin` 等既有子串误报 |

> CDP 连真机的脚本（`verify-checkin-center` / `verify-settings-entry` / `verify-resize` / `verify-qoder-expiry` / `verify-ui-0923` / `verify-focus-ring`）需运行中的应用会话，本次环境未跑；它们覆盖的入口/元素均不在本次改动面内。`verify-release.js` 属发版后核验，本次未发版不适用。

### 9.4 执行期新发现（已修复）

- `build/verify-codex-layout.js` 在 v0.3.35 基线即已损坏（经 HEAD 快照复跑确认，与本次改动无关）：渲染改版后 `.quota-number` 类已不存在，脚本对它无条件 `getComputedStyle` 必然抛 TypeError。已改为现行主数字 `.val .v` 并恢复可运行（48/48）。

---

> 第八节之前的内容为 2026-11-11 排查与复核留档；第九节起为本报告的执行记录。
