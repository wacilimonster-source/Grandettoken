# Grandettoken 开发进展

> 最后更新:2026-11-23
> 状态:**桌面 v0.3.73、Android 0.1.14 已发布**(以文末最新条目与 `git log` 为准)。
> 2026-11-12 做过两轮**代码治理**:产物出库 / 门禁补强 / 静默失败归零 / 经验沉淀,
> 见 `docs/report/code-governance-2026-11-12.md`、`docs/report/code-quality-2026-11-12-b.md`
> 与根目录 `AGENTS.md`。
>
> ⚠️ 新条目一律**追加在文末**;头部状态只是「写它那一刻」的快照,最容易过时 ——
> **当前版本与改动一律以文末最新条目和 `git log` 为准**,不要拿头部当事实源。
>
> **归档**:2026-11-14 v0.3.50 之前的条目,连同早期「一句话现状」「产物约束」「关键设计决策」
> 「环境与构建」「Git」「单 exe / WebView2Loader」「尚未实现 / 待验证」等章节,已于 2026-11-22
> 原样移至 `docs/report/progress-archive-2026-11-22.md`。其它文档里「见 PROGRESS.md 某章节」的旧引用请到那里查。
> 跨版本未关闭的验收项集中在 `docs/report/acceptance-backlog.md`。
>
> 产品原名 TokenScope,2026-09 下旬改名 Grandettoken。凭据服务名(`TokenScope`)
> 与 bundle identifier(`com.wacil.tokenscope`)仍沿用旧名 —— 改了会让已装用户收不到更新、
> 凭据条目全丢。

## 2026-11-14 v0.3.50 顾问时长统计改「N 天 M 小时」

- 用户反馈 WAIT 结论「等 约 146.4 小时 的自然重置」不直观:≥24 小时改折「N 天 M 小时」
  (146.4h → 约 6 天 2 小时),不足一天保留 0.1 小时精度,不足一小时按分钟;
  小时段四舍五入满 24 进位成整天(47.7h → 约 2 天)。
- 后端抽 `format_gap_hours`(codex_reset.rs,headline 与 Codex 卡内嵌结论同源),
  前端 `fmtGap`(codex-reset.js 指标行)保持同口径;两处注释互相标注必须同步修改。
- 双向验证:Rust `gap_copy_splits_days_and_hours` 边界单测(分钟/0.1h/23.96h 不折天/
  24h 整天/47.7h 进位/天+小时);顾问冒烟 gapHours 夹具变量化 + 两条 DOM 断言,
  改夹具必须同时推 uiNow 失效建议缓存(时钟失效棘轮落地)。
- 发版前门禁串行全绿:静态四门及自检、五个 JS 语法、jsdom 201/201、顾问/密钥箱/
  Google Cloud 冒烟、core 535 passed / 8 ignored、src-tauri 全目标编译(--locked)、
  凭据扫描 CLEAN。版本号三处同步 0.3.50 并 `cargo fetch --locked` 复跑通过。
- 待验收:顾问 headline 与指标行的实际显示效果(需真实账号数据下查看);
  日期控件回填的人工复核仍开放(v0.3.49 遗留)。

## 2026-11-14 v0.3.50 发布结果

- tag `v0.3.50` 指向 `9dc5118`(时长口径发版 commit)。release run `43574206929` 全部成功;
  main CI 对 9dc5118 与 e16d535 均成功。
- Release 于北京时间 2026-11-14 16:30:05 公开发布,三件套齐全(安装包 5,893,505 字节,
  SHA-256 本地匿名下载回验 `fb251a1bc204fd6123d588005f8e37f83330367d44bc634cbef5c1cb5ee16a1f`;
  签名 444 字节;更新清单 1,702 字节),非 draft。
- 本机 `node --use-env-proxy build/verify-release.js` 二次核验 PASS(14 项),固定更新端点
  返回 0.3.50;签名/安装包逐字比对按既有口径 SKIP,由 CI 端完成。
- 待验收:安装包内顾问 headline 与指标行的「N 天 M 小时」实际显示;日期控件回填的
  人工复核(v0.3.49 遗留,见验收报告问题 #2)。

## 2026-11-15 v0.3.51 + android-v0.1.14：Antigravity 配额监控与 Codex 重置顾问视觉改版

- **桌面端新增 Antigravity (`antigravity`) 内置配额渠道**（设计稿 `docs/product/product-antigravity-quota-2026-11-15.md`）：
  - 安全采集链（`app/core/src/antigravity.rs`）：仅调用用户本机官方 `agy` CLI（`agy -p /usage --output-format json --print-timeout 25s`），
    零桌面端内部数据库/端口探测、零凭据/OAuth 密钥提取；子进程隔离环境（`CREATE_NO_WINDOW`、自动清理临时目录、`AGY_CLI_DISABLE_AUTO_UPDATE=true`、
    `BROWSER=NUL`、`1 MiB` 输出上限、`35s` 硬超时强杀）、`mtime` 缓存版本探测、`120s` 自动冷却（手刷/清除/改路径时清空）。
  - 口径与展示：P0 不读取/不展示 AI Credits，主额度固定钉住 `gemini-weekly`（缺省依次回落 `gemini-5h` / 首个有效窗口），
    `view.rs` 同时保留 `binding_window` 的限流/告警与二次行；独立模块 `app/src/antigravity.js` 提供可选自定义 `agy` 路径与失败回滚保护，
    搭配 256×256 官方透明图标（`app/src/logos/antigravity.png`），同步在 `README.md` / `app/README.md` 披露子进程边界。
- **Android 端同步支持 Antigravity（`android-v0.1.14`）**：
  - 新增透明图标 `channel_antigravity.png`、`ChannelPresentation.kt` 映射、`WidgetFormatter.kt` 多配额窗简标（`G周`/`G5h`/`AI周`/`AI5h`/`Ag周`）、
    `WidgetUpdater.kt` 着色规则与 `WidgetConfigActivity.kt` 多窗选择器，附 `WidgetFormatterTest.kt` 单测。
- **Codex 重置顾问视觉优化**（另一会话协作改动）：
  - 更新 `app/src/codex-reset.css`、`app/src/codex-reset.js`、`app/src/styles.css` 与设计原型 `docs/design/design-codex-reset-visual-2026-11-15.html`：
    重构内嵌入口卡色调（`tone-wait`/`tone-now`/`tone-hold`）、重置卡明细徽章与页脚布局、详情头部返回与状态胶囊、指标区与配置块层级、
    判定路径高亮及用量重置快捷键焦点定位，并微调详情弹层 `.shade` 高度锚点。
- **版本号与工作流**：桌面端同步 `0.3.50` → `0.3.51`（`Cargo.toml` / `Cargo.lock` / `tauri.conf.json`）；
  `ci.yml` 与 `release.yml` 补充 `app/src/antigravity.js` 语法检查。

## 2026-11-15 v0.3.51 + android-v0.1.14 发布结果

- tag `v0.3.51` 指向 `f7b4969`,tag `android-v0.1.14` 同一 commit。Android release run
  `43627948065` 与桌面 CI `43627943817` success;桌面 release run `43627948084` success
  (另一会话执行发布,本会话接手核验与记录)。
- **桌面** Release 于北京时间 2026-11-15 02:58:24 公开发布,三件套齐全(安装包
  5,945,166 字节,SHA-256 本地匿名下载回验
  `9ba779cbfa6abacb9bac53f22904aae8c49db6e13303c1e79b11f2b541ddaafa`;签名 444 字节;
  更新清单 1,500 字节),非 draft。本机 `verify-release.js` 二次核验 PASS(14 项),
  固定更新端点返回 0.3.51。
- **Android** Release `android-v0.1.14` 公开发布(资产 `Grandettoken-android-v0.1.14.apk`,
  非 draft);`android-stable/android-latest.json` 清单解码确认 versionName 0.1.14 /
  versionCode 114 / minSdk 26,keyId+payload+signature 结构完整(CI 端
  verify-android-release.py 已做密码学校验);匿名拉取成功。
- **桌面 `releases/latest` 未被 Android 抢占**(仍指向 v0.3.51)—— CI 断言口径在实机复核。
- 待验收:Antigravity 渠道需本机 `agy` CLI 与真实账号下验收;顾问视觉改版的安装包内
  实际观感;日期控件回填的人工复核(v0.3.49 遗留)持续开放。

## 2026-11-15 Antigravity 展示层级调整（工作区，待发版）

- 按用户偏好，首页主数字标明 Gemini 本周剩余，状态轨固定展示 Gemini 5 小时余量、
  耗尽状态及重置倒计时。详情将周窗口大数字和短窗口放在主要区域，其他模型组默认折叠，
  组内完整展示模型说明与窗口状态。适用模型说明可独立展开，时间显示到分钟。
- 展示逻辑复用独立 `antigravity.js`，新增 `antigravity.css`，`app.js` 仅接线。
  已用模式同步标签与百分比，短窗缺失如实提示，跨重置点提示待刷新。
- jsdom 回归覆盖默认层级、展开/收起、刷新后状态与焦点、动态模型组与说明转义、
  缺窗、耗尽、过重置点、失败与陈旧状态及配置写入回滚；加入移除周窗主样式的报红哨兵。
- 首轮冒烟发现公共 qtag 的 title 重复触发既有文案唯一性断言，已收回该修改并独立复跑通过。
  浏览器排版初检在最窄宽度与最大倍率组合发现状态轨越界，允许换行后独立复跑通过。
- 本轮串行门禁及独立复跑通过，包含 IPC、静默失败、文件规模、JS 语法、CSS 形态、
  jsdom、重置顾问/Google Cloud/密钥箱冒烟、core 单线程全量测试、src-tauri 全目标编译、
  凭据扫描。浏览器使用合成数据检查三档宽度、三档倍率和深浅主题，截图仅本地 QA 留档。
- 尚未发版，安装包内真实账号与原生 WebView2 观感待验收。


## 2026-11-15 v0.3.52 Antigravity 展示层级调整随版发布

- 集中发布工作区「Antigravity 展示层级调整」条目:首页主数字标明 Gemini 本周剩余,
  状态轨固定展示 Gemini 5 小时余量、耗尽状态及重置倒计时;详情把周窗大数字与短窗
  放主要区域,其他模型组默认折叠;展示逻辑复用 antigravity.js,新增 antigravity.css
  (CSS 形态门纳入,5 个样式文件),app.js 仅接线。
- 门禁(本会话发版前串行复跑):静态四门及自检(静默失败门 5 个脚本、CSS 门 5 个
  样式文件 1266 条规则)、六个 JS 语法、jsdom 212/212(含移除周窗主样式的报红哨兵)、
  顾问/密钥箱/Google Cloud 冒烟、core 542 passed / 8 ignored、src-tauri 全目标编译
  (--locked)、凭据扫描 CLEAN。版本号三处同步 0.3.52 并 `cargo fetch --locked` 复跑通过。
- 待验收:安装包内真实账号的 Antigravity 取数与三档宽度/倍率下的原生 WebView2 观感;
  日期控件回填的人工复核(v0.3.49 遗留)持续开放。

## 2026-11-15 v0.3.52 发布结果

- tag `v0.3.52` 指向 `4af2ba6`(Antigravity 展示层级发版 commit)。release run
  `43631893224` 全部成功;main CI 对 4af2ba6 成功。
- Release 于北京时间 2026-11-15 03:40:17 公开发布,三件套齐全(安装包 5,947,554 字节,
  SHA-256 本地匿名下载回验
  `0afde86a2e621fedd1044deec7877e0e2e4025a00fd8af71b80d05ba8462a182`;签名 444 字节;
  更新清单 1,784 字节),非 draft。
- 本机 `node --use-env-proxy build/verify-release.js` 二次核验 PASS(14 项),固定更新
  端点返回 0.3.52;签名/安装包逐字比对按既有口径 SKIP,由 CI 端完成。
- 待验收:安装包内真实账号的 Antigravity 取数与三档宽度/倍率下的原生 WebView2 观感;
  顾问时长「N 天 M 小时」的实际显示;日期控件回填的人工复核(v0.3.49 遗留)持续开放。

## 2026-11-15 窄窗双列缩略（工作区，待发版）

- 用户选定在单列尺寸中每排展示两个渠道。详细规格为 docs/design/design-thumbnail-mode-2026-11-15.md，产品比较记录为 docs/product/product-thumbnail-mode-2026-11-15.md。
- 新增 panel-layout.js / panel-layout.css，提供单列、双列、缩略三段选择。缩略沿用单列内容尺寸，两列小卡按实际可用高度在 68–80px×字号倍率间调整，底栏退出收进更多菜单。
- 复用既有主余额、排序、过滤、详情与分钟 ticker，缩略摘要保留 Codex 多窗口和重置卡、Antigravity 短窗口与限流、失效/旧快照/未知到期等状态。core 配置支持 thumbnail 并验证序列化与非法值回退。
- 布局保存期间禁用重复切换，保存失败恢复原选择并显示提示。已通过真实底栏点击、保存成功、保存失败回滚、刷新节点和焦点保留的回归。
- 首轮冒烟检出隐藏的缩略专用文案重复进入原有 HTML 断言，已改为仅在缩略布局生成。经验同步 AGENTS.md。
- 首轮 core 单线程全量中 custom_channels_fetch_in_parallel_keep_order_and_skip_hidden 的耗时断言失败，两个延迟端点实际耗时 1.1105213s。未修改取数或测试阈值，独立重跑耗时 0.26s 通过，随后两次单线程全量均通过。原始失败及复跑日志本地留档。
- 静态 IPC / 静默失败 / 文件规模 / CSS 门及相应自检、全部前端 JS 语法、jsdom、重置顾问 / Google Cloud / 密钥箱冒烟、假 DOM 前端探针、core 单线程全量、src-tauri 全目标编译和凭据扫描均已串行执行并复核通过。文件规模预算随 app.js 的布局逻辑抽离下调。
- 新增缩略回归模块与开发期浏览器排版脚本。真实渲染的合成渠道夹具覆盖三档字号、深浅主题、长金额、增加至 16 个渠道、760px 与 680px 高度钳制；漏行、单列网格和超高卡片坏样本被断言拦截。标准尺寸下 14 张卡全部首屏可见，严重高度钳制时按设计滚动。
- 本轮变更保留在工作区，安装包尚待 CI 发版。新增布局的安装版原生窗口、真实账号、scaleFactor 和实际显示器工作区联合验收仍待完成。已有 v0.3.52 的其他待验收项沿用文末发布记录。

## 2026-11-15 v0.3.53 窄窗双列缩略布局随版发布

- 集中发布工作区「窄窗双列缩略」条目:单列尺寸内每排两个渠道小卡(68–80px×字号倍率
  按实际可用高度钳制),新增 panel-layout.js / panel-layout.css 提供单列、双列、缩略
  三段选择,底栏退出收进更多菜单;core 配置支持 thumbnail 并验证序列化与非法值回退;
  布局保存期间禁用重复切换,保存失败恢复原选择并提示;缩略摘要保留 Codex 多窗口与
  重置卡、Antigravity 短窗口与限流、失效/旧快照/未知到期状态。
- 复用既有主余额、排序、过滤、详情与分钟 ticker;缩略专用名称与状态仅在缩略布局生成
  (AGENTS 7.2 缩略布局棘轮:CSS 隐藏的重复文案仍会进入 textContent 与文案唯一性断言)。
- 发版前门禁串行复跑(本会话):静态四门及自检(静默失败门 6 个脚本、CSS 门 6 个样式
  文件 1299 条规则、app.js 预算随布局抽离下调 9220→9183)、七个 JS 语法、jsdom 225/225
  (含缩略回归:三档字号、深浅主题、16 渠道、760/680px 高度钳制、漏行/单列/超高卡
  坏样本拦截)、顾问/密钥箱/Google Cloud 冒烟、core 542 passed / 8 ignored(工作区
  首轮的耗时断言假红未复现)、src-tauri 全目标编译(--locked)、凭据扫描 CLEAN。
  版本号三处同步 0.3.53 并 `cargo fetch --locked` 复跑通过。
- 待验收:缩略布局在安装包原生窗口的真实观感(三档倍率/深浅主题/实际显示器工作区);
  真实账号 Antigravity 取数;顾问时长「N 天 M 小时」显示;日期控件回填人工复核(遗留)。

## 2026-11-15 v0.3.53 发布结果

- tag `v0.3.53` 指向 `886aa88`(窄窗双列缩略发版 commit)。release run `43637269540` 全部成功;main CI 对 886aa88 成功。
- Release 于北京时间 2026-11-15 04:41:33 公开发布,三件套齐全(安装包 5,950,180 字节,
  SHA-256 本地匿名下载回验
  `d47c219e57231b579d5e6c82bf45c3c140d5e69d8e3aff02872a98c480f3d218`;签名 444 字节;
  更新清单 1,878 字节),非 draft。
- 本机 `node --use-env-proxy build/verify-release.js` 二次核验 PASS(14 项),固定更新
  端点返回 0.3.53;签名/安装包逐字比对按既有口径 SKIP,由 CI 端完成。
- 待验收:缩略布局在安装包原生窗口的真实观感(三档倍率/深浅主题/实际显示器工作区);
  真实账号 Antigravity 取数;顾问时长「N 天 M 小时」显示;日期控件回填人工复核(遗留)。

## 2026-11-15 缩略卡片数值右对齐（工作区，待发版）

- 根据用户对 v0.3.53 安装版的实机反馈，采用选定的「数值右对齐」排版。缩略卡顶部名称靠左、状态点靠右；中部口径标签在左、主数字在右；底部到期金额与日期、重置卡与短窗额度两端分列。普通卡片边框减弱。
- 实现在现有 panel-layout.js / panel-layout.css 内，app.js 仅调整口径标签接线。Google Cloud 的缩略日期展示复用 rail 的赠金筛选，分别呈现日期来源、部分日期未填及已到期状态，完整日期位于右侧。细节更新至 docs/design/design-thumbnail-mode-2026-11-15.md 与 DESIGN.md。
- 倒计时区分「后重置」和「后恢复」，越过重置点保持「待刷新」。缩略回归覆盖左右字段、已用口径、动态渠道类型、日期来源、跨重置点、保存失败回滚及刷新焦点。
- 真实渲染的合成夹具通过三档字号、深浅主题、长金额、增加至 16 渠道及高度钳制的浏览器检查。标准单列尺寸下 14 个渠道均在首屏，标签与数值无重叠，主数字及状态末列对齐右边界。漏行、单列网格、超高卡片及把数值改回左对齐的坏样本均被断言拦截。
- 首轮 core 全量在本地 HTTP 夹具 custom_channels_fetch_in_parallel_keep_order_and_skip_hidden 阻塞，环境存在 HTTP_PROXY 且没有 NO_PROXY。终止本轮测试进程后，仅为验证进程设置 NO_PROXY=127.0.0.1,localhost,::1，独立用例及随后两次单线程全量通过。原始中断、独立及全量复跑日志留在本地 build/qa/thumbnail-right/，经验同步 AGENTS.md。
- 门禁已串行完成并独立复核通过，包括 IPC、静默失败、文件规模、CSS 及自检、前端 JS 与浏览器验证脚本语法、jsdom、重置顾问 / Google Cloud / 密钥箱冒烟、假 DOM 前端探针、core 单线程全量、src-tauri 全目标编译、浏览器排版和凭据扫描。具体用例数量以命令输出为准。
- 改动保留在工作区。发布及安装版原生 WebView2 的真实账号、字号倍率、scaleFactor 与屏幕工作区联合验收待完成。

## 2026-11-15 v0.3.54 缩略卡片数值右对齐随版发布

- 集中发布工作区「缩略卡片数值右对齐」改动:缩略卡顶部名称靠左、状态点靠右；中部口径标签在左、主数字在右；底部到期金额与日期、重置卡与短窗额度两端分列；普通卡片边框减弱；Google Cloud 缩略日期复用赠金筛选提示；倒计时区分「后重置」与「后恢复」并在越过重置点保持「待刷新」。
- 发版前门禁串行复跑(本会话):静态四门及自检(IPC 122 处 invoke / 98 命令、静默失败 6 脚本 74 处 catch、文件规模、CSS 6 样式文件 1303 条规则)、七个前端 JS 与 `verify-thumbnail-layout.py` 语法、jsdom 227/227(含缩略模式 15 项回归)、密钥箱 / Codex 重置顾问 / Google Cloud 冒烟、假 DOM 前端探针、Playwright 浏览器排版校验(三档倍率、深浅主题、16 渠道、760/680px 高度钳制、坏样本拦截及截图核验)、`app/core` `cargo test --locked -- --test-threads=1`（设置 `NO_PROXY=127.0.0.1,localhost,::1`，542 passed / 8 ignored + `sync_fixtures` 6 passed + doc-test 1 ignored）、`app/core` 与 `app/src-tauri` `cargo check --all-targets --locked`、凭据扫描 CLEAN(459 文件)。
- 版本号三处同步 `0.3.54` 并通过 `cargo fetch --locked`。
- 待验收:安装版原生 WebView2 真实账号、字号倍率、scaleFactor 与屏幕工作区联合验收，以及既有遗留人工复核项。

## 2026-11-15 v0.3.54 发布结果

- tag `v0.3.54` 指向 `13dbd5f`(缩略卡片数值右对齐发版 commit)。release run `43641765149` 全部成功；main CI run `43641760129` 全部成功。
- Release 于北京时间 2026-11-15 05:34:13 公开发布，三件套齐全(安装包 5,950,802 字节，SHA-256 本地匿名下载回验 `e8a22114848a577492509aae3b8f82cec26b79f977b57a005986685a9624b1d8`；签名 444 字节；更新清单 1,549 字节)，非 draft。
- 本机 `node --use-env-proxy build/verify-release.js` 二次核验 PASS(14 项检查：12 PASS，2 项本地文件缺失按既有口径 SKIP)，固定更新端点返回 `0.3.54`。
- 待验收:安装版原生 WebView2 真实账号、字号倍率、scaleFactor 与屏幕工作区联合验收，以及既有遗留人工复核项。

## 2026-11-15 Gemini 缩略卡范围与渠道摘要文案调整（工作区，待发版）

- 根据用户实机反馈，Antigravity 缩略卡仅展示 gemini-weekly 与 gemini-5h。主额度、状态点、色调、限流判定和辅助提示在行渲染入口采用相同的 Gemini 范围，避免将 Claude/GPT 本周池的六天限流显示在 Gemini 卡片上。Gemini 自身耗尽时显示恢复倒计时，缺失 Gemini 数据时显示未知。
- Gemini 的展示投影位于 panel-layout.js，原始渠道对象与池数据由回归断言检查。采集失败、登录失效及旧快照沿用对应提示。用户裁决与范围一致性要求同步 AGENTS.md，设计更新至 DESIGN.md 与 docs/design/design-thumbnail-mode-2026-11-15.md。
- 各模式渠道卡的今日消耗及积分获取摘要去除「推算」字样，涵盖单列、双列、缩略、紧凑条及胶囊对应的渠道摘要入口。
- 回归覆盖 Claude/GPT 限流且 Gemini 可用、Gemini 自身耗尽、Gemini 窗口缺失、状态点及色调一致、原始数据未被展示投影修改、各模式摘要文案、刷新焦点及布局保存失败回滚。
- 静态 IPC / 静默失败 / 文件规模 / CSS 门及相应自检、前端 JS 语法、jsdom、重置顾问 / Google Cloud / 密钥箱冒烟、假 DOM 前端探针、core 单线程全量、src-tauri 全目标编译、浏览器排版与凭据扫描已串行完成并独立复核通过。验证进程按现有本机前置设置 NO_PROXY=127.0.0.1,localhost,::1。用例数量以本轮命令输出为准。
- 浏览器合成夹具验证三档字号、深浅主题、长金额、增加至 16 个渠道及高度钳制，标准窄窗的 14 个渠道全部首屏可见；既有排版坏样本被断言拦截。日志与截图仅在本地 build/qa/gemini-card/ 留档。
- 改动保留在工作区，发布和安装版原生 WebView2 验收待完成。

## 2026-11-15 积分到期日期补充星期（工作区，待发版）

- 按用户要求先确定方案并写入 docs/product/product-expiry-weekday-2026-11-15.md，再进行开发。用户随后补充本周必须写「本周X」，已同步实现、方案及回归。规则按本地自然周、周一为起点，使用「本周X / 下周X / 周X」，跨年补年份。
- 缩略卡右下角和单列、双列渠道卡状态轨的最近一笔积分到期日期使用 panel-layout.js 内的同一格式化入口。日期节点的辅助提示提供完整年份、星期和时刻。保留日期未知分支，最近一笔继续由现有尚未到期记录筛选。
- 日期或时区偏移变化时，现有分钟 ticker 触发一次节点复用重算；固定时钟验证周日深夜到周一后「下周三」变为「本周三」，同步更新辅助提示并保留卡片焦点。
- 日期回归覆盖同周、下周、更远日期、跨月、跨年、闰日、未知日期及缺星期坏样本。浏览器追加 America/New_York 的夏令时前后及 UTC 与本地日历边界检查，跨年长日期的几何边界也已验证。
- 浏览器扩展检查首轮因夹具漏载 esc 转义依赖报 ReferenceError。已让夹具从 app.js 同时加载日期格式和转义函数，独立复跑通过；未将该夹具失败当作应用错误修改业务逻辑。
- 静态 IPC、静默失败、文件规模、CSS 门及自检、前端 JS 和浏览器验证脚本语法、jsdom、顾问 / Google Cloud / 密钥箱冒烟、假 DOM 前端探针、core 单线程全量、src-tauri 全目标编译、浏览器排版及凭据扫描已串行完成并独立复核通过。验证进程沿用本机回环直连前置，具体用例数量看命令输出。
- 浏览器合成数据确认三档字号、深浅主题下 14 个渠道首屏可见，日期与状态维持同一行且右对齐。日志和截图仅本地 build/qa/expiry-weekday/ 留档。
- 与上一条 Gemini 缩略卡范围及渠道摘要文案改动一并保留在工作区，发布及安装版原生效果验收待完成。

## 2026-11-15 v0.3.55 积分到期星期与 Gemini 缩略卡范围随版发布

- 集中发布工作区两条「待发版」条目:积分到期日期按本地自然周(周一为起点)显示
  「本周X / 下周X / 周X」,跨年补年份,缩略卡右下角与单列/双列状态轨共用 panel-layout.js
  同一格式化入口,辅助提示带完整年份/星期/时刻;日期或时区偏移变化时分钟 ticker 触发
  节点复用重算。Gemini 缩略卡仅展示 Gemini 周窗与 5 小时窗,状态点/颜色/辅助提示同一
  池范围;各模式渠道卡今日消耗与积分获取摘要移除「推算」字样。
- 发版前门禁串行复跑(本会话,验证进程按 AGENTS 6.7 设 NO_PROXY=127.0.0.1,localhost,::1):
  静态四门及自检、八个 JS 语法、jsdom 232/232(含星期回归:同周/下周/跨月/跨年/闰日/
  未知日期/缺星期坏样本,及周日深夜到周一「下周三」→「本周三」的固定时钟跨日)、
  顾问/密钥箱/Google Cloud 冒烟、core 542 passed / 8 ignored、src-tauri 全目标编译
  (--locked)、凭据扫描 CLEAN。版本号三处同步 0.3.55 并 `cargo fetch --locked` 复跑通过。
- 待验收:星期文案在真实数据与原生窗口的显示;Antigravity 真实账号取数;顾问时长
  「N 天 M 小时」;日期控件回填人工复核(遗留)。

## 2026-11-15 v0.3.55 发布结果

- tag `v0.3.55` 指向 `fd72ac5`(积分到期星期与 Gemini 缩略卡发版 commit)。release run
  `43674534082` 全部成功;main CI 对 fd72ac5 成功。
- Release 于北京时间 2026-11-15 12:18:28 公开发布,三件套齐全(安装包 5,952,444 字节,
  SHA-256 本地匿名下载回验
  `6d3fdc513dd6ff4d3f6f444d69fd28462ee218527267d94abb01fe465a5a7d84`;签名 444 字节;
  更新清单 1,985 字节),非 draft。
- 本机 `node --use-env-proxy build/verify-release.js` 二次核验 PASS(14 项),固定更新
  端点返回 0.3.55;签名/安装包逐字比对按既有口径 SKIP,由 CI 端完成。
- 待验收:「本周X / 下周X / 周X」在真实数据与原生窗口的显示(含跨日翻转);Antigravity
  真实账号取数;顾问时长「N 天 M 小时」;日期控件回填人工复核(遗留)。

## 2026-11-15 v0.3.56 本地模型网关三源统一与思维链折叠随版发布

- 整体重构 PC 端本地模型网关并拆出协议层 `app/core/src/gateway_proto/*`、HTTP/流式引擎
  `app/src-tauri/src/gateway_http.rs`、渠道同步 `app/src-tauri/src/vault_sync.rs` 与前端
  模块 `app/src/gateway-view.{js,css}`，同步接入 `release.yml` 门禁。
- 三类上游接入统一转为 SillyTavern / PocketTavern 自定义源与主流 Agent 兼容的 OpenAI 接口
  (`/healthz`、`/v1/models`、`/v1/models/{*id}`、`/v1/chat/completions`)：
  1) `openai_compat`：支持任意 `/v1` 端点、密钥箱自动带入与模板、本地免密模式及同密钥多别名上架；
  2) `agent_platform`：对齐 PocketTavern 默认 Vertex 端点与 5 个预置模型，鉴权头固定 `x-goog-api-key` 并剥离 `Authorization`，含 `google/gemini-3.1-flash-lite` penalty 白名单；
  3) `gemini_aistudio`：分页拉取 `/v1beta/models`、`thinking_config` 映射与 HTTP 400 单次自动修复重试。
- 流式 SSE 与非流式统一将 `reasoning` / `thinking` / `extra_content.google.thought` 及正文开头
  `<think>` / `<thinking>` / `<thought>` / Harmony 标签归一至 `reasoning_content`；`openai_compat` 默认开启自动切分且可按渠道切换 `auto` / `fields_only` / `passthrough`。
- 每渠道系统代理开关默认开启(`system_proxy: true`)；密钥箱(`vault_save` / `vault_list`)与渠道密钥变更(`set_key` / `delete_key` / `set_key_ref` / `clear_key_ref`)自动同步关联渠道；新增连通测试、流式思维链联调台与脱敏诊断环缓冲区，并在 `check-file-size.js` 下调 `app.js`/`lib.rs`/`styles.css` 上限。
- 发版前门禁已在最终改版后全部串行复跑通过（NO_PROXY=127.0.0.1,localhost,::1）：静态四门及三项 `--self-test`、8 个前端/扩展 JS 语法检查、`jsdom-smoke.js`（232/232）、`verify-vault.js`、`verify-codex-reset.js`、`verify-google-cloud.js`、`verify-gateway.js`（含 cmd/proxy 双向哨兵）、`app/core` `cargo test --locked -- --test-threads=1`（561 passed / 8 ignored + `sync_fixtures` 6 passed）、`app/src-tauri` `cargo check --all-targets --locked`、`cargo fetch --locked` 与凭据扫描 CLEAN。

## 2026-11-15 v0.3.56 发布结果

- tag `v0.3.56` 指向 `273bdd4`（本地模型网关三源统一与思维链折叠发版 commit）。release run `43681946950` 全部成功；main CI run `43681924507` 对 `273bdd4` 全部成功。
- Release 于北京时间 2026-11-15 13:59:21（`2026-11-15T05:59:21Z`）公开发布，三件套齐全（安装包 `Grandettoken_0.3.56_x64-setup.exe` `6,031,502` 字节，本机匿名下载回验 SHA-256 `8bdcd7f7f9cc4b392aa5aaac5b5113bd2672588e609c9edefd4d31cb40beb9a7`；签名 `.sig` `444` 字节；更新清单 `latest.json` `1,819` 字节），非 draft，`notes intact` 665 字符 / 192 中文字符。
- 本机 `node --use-env-proxy build/verify-release.js` 二次核验 PASS（14 项含固定更新端点返回 `0.3.56`；本地无 `.sig` 与安装包文件两项按既有口径 SKIP，由 CI 端完成）。
- 待验收：实机安装版自动更新及新网关面板在 SillyTavern / PocketTavern 与主流 Agent 下的真实流式/思维链联调效果；v0.3.55 遗留真机核验项（`本周X/下周X`、Antigravity 真实账号取数、顾问时长等）。

## 2026-11-15 v0.3.57 本地模型网关路由/思考链/慢首字超时修复与设置页分组精简随版发布

- 修复 PocketTavern / SillyTavern 自定义源拉取模型列表与鉴权兼容性问题：
  1) `gateway_http.rs` 为 `/models`、`/models/{*id}`、`/chat/completions` 补齐无 `/v1` 前缀别名路由，适配不同 Base URL 拼接习惯；
  2) `gateway.rs` 的 `live_channels()` 移除过时的 `v.updated_at == c.updated_at` 过滤、`authorized()` 放宽 `Bearer` 大小写并去空白、`peer_allowed()` 补齐 IPv6 loopback / v4-mapped IPv6，并在 `lib.rs` 启动 `import_channel_keys` 后同步刷新网关渠道快照，防止已上架渠道在密钥更新后从 `/models` 消失。
- 修复 Agent Platform（含 `gemini-4-hydrogen`）在 SillyTavern 自定义源下的思考链传递与慢首字 502 假报错：
  1) `request.rs` 在 `FollowClient` 策略下兼容仅传 `reasoning_effort`（未传被 SillyTavern 剥离的 `include_reasoning`）及 thinking-only 模型 `gemini-4-hydrogen` 未显式关闭时主动注入 `extra_body.google.thinking_config.include_thoughts = true`，同时 `thinking.rs` 的 `consume_google_thought_flag()` 补齐直接 `thought: true` 分片判定；
  2) `proxy.rs` 将 `read_timeout` 由 60s 放宽到 150s（覆盖长上下文如 `gemini-4-hydrogen` 50–90s+ 慢首字），`gateway_http.rs` 将 `CHAT_TIMEOUT_SECS` 放宽到 300s 并按 `e.is_timeout()` 区分 `504 upstream_timeout` 与 `502 upstream_unreachable`。
- 优化「设置 → 本地模型网关」页签布局（`gateway-view.{js,css}`）：拆分为「渠道与接入」「模型与联调」「网络与示例」三栏保留草稿与状态；已有渠道时自动折叠添加抽屉，并将思维链策略/手动模型/自定义请求头收进二级高级折叠区，同步更新 `build/verify-gateway.js` 分栏哨兵与断言。
- 三处版本号已同步至 `0.3.57`（`Cargo.toml` / `tauri.conf.json` / `Cargo.lock`），并在最终改动后串行复跑全部门禁、单测、编译校对与凭据扫描通过。

## 2026-11-15 明后天提示与周额度重置日期（工作区，待发版）

- 按用户要求优化日期表达，保留月日日期，优先按本地自然日显示「明天 / 后天」，其余使用「本周X / 下周X / 周X」。跨月、跨年、闰日及夏令时沿用本地日历日序判断。方案补入 docs/product/product-expiry-weekday-2026-11-15.md。
- 将用户所说的「周下次重启日期」按周额度下次重置日期实现。Codex 读取 weekly / 本周窗口的 resetsAt，Antigravity 读取 gemini-weekly 的 resetsAt。时间缺失显示「周重置未知」，上报时刻已到显示「周重置待刷新」。
- 缩略卡在左侧口径标签下增加周重置日期，两行共用既有数值区高度，右侧主数字保持右对齐。单列、双列卡在状态轨右侧增加周日期，并保留既有短窗倒计时。日期节点悬停显示完整年月日、星期和准确时刻。
- 周日期与积分到期共用日期格式入口并接入现有分钟 ticker。新增仅有周额度渠道时的跨日重算回归，确认可见日期及辅助提示从「下周三」同步变为「后天」。
- 回归覆盖明后天优先级、跨周跨月跨年、闰日、夏令时、Codex 周窗与 Gemini 周池选择、时间缺失、已到重置时刻、辅助提示同步及单列短窗/周窗同时呈现。日期和布局规则同步 AGENTS.md、DESIGN.md 与缩略详细设计。
- 浏览器验证三档字号、深浅主题、14 渠道首屏、增加至 16 渠道、长金额、周日期文字裁切及高度钳制。极值金额夹具按渠道类型区分，百分比渠道使用 100.0%。本地日志和截图位于 build/qa/weekly-reset/，未加入版本库。
- 静态 IPC / 静默失败 / 文件规模 / CSS 门及自检、全部前端 JS 与浏览器脚本语法、jsdom、顾问 / Google Cloud / 密钥箱 / 网关冒烟、假 DOM 前端探针、core 单线程全量、src-tauri 全目标编译、浏览器排版及凭据扫描已串行通过并独立复核。验证进程采用既有回环直连前置，用例数量以命令输出为准。
- 改动保留在工作区，发布与安装版原生 WebView2 真实账号验收待完成。

## 2026-11-15 v0.3.58 明后天提示与周额度重置日期随版发布

- 集中发布工作区「明后天提示与周额度重置日期」条目:积分到期优先按本地自然日显示
  「明天 / 后天」,其余「本周X / 下周X / 周X」,跨月/跨年/闰日/夏令时沿用本地日历;
  Codex 读 weekly 窗、Antigravity 读 gemini-weekly 的 resetsAt 作为周下次重置日期,
  时间缺失显示「周重置未知」、已到显示「周重置待刷新」,不借用短窗时间;缩略卡口径
  标签下增加周重置日期,单列/双列卡状态轨右侧增加周日期,与积分到期共用日期格式
  入口并接入分钟 ticker 跨日重算。
- 发版前门禁串行复跑(本会话,NO_PROXY 回环直连前置):静态四门及自检(静默失败门
  7 个前端脚本)、八个 JS 语法、jsdom 235/235(含明后天优先级、跨周跨月跨年、闰日、
  夏令时、Codex 周窗与 Gemini 周池选择、时间缺失、已到重置时刻、跨日「下周三」→
  「后天」)、顾问/密钥箱/Google Cloud/网关冒烟、core 562 passed / 8 ignored、
  src-tauri 全目标编译(--locked)、凭据扫描 CLEAN。版本号三处同步 0.3.58 并
  `cargo fetch --locked` 复跑通过。
- 待验收:明后天与周重置日期在真实数据与原生窗口的显示(含跨日翻转);Antigravity
  真实账号取数;日期控件回填人工复核(遗留)。

## 2026-11-16 v0.3.58 发布结果

- tag `v0.3.58` 指向 `66b9583`(明后天提示与周重置日期发版 commit)。release run
  `43732308060` 全部成功;main CI 对 66b9583 成功。
- Release 于北京时间 2026-11-16 00:08:17 公开发布,三件套齐全(安装包 6,032,874 字节,
  SHA-256 本地匿名下载回验
  `c640d1bc63fabed340887f70195e5d094fb9efc09aeabd66678328211ff2bba5`;签名 444 字节;
  更新清单 2,020 字节),非 draft。
- 本机 `node --use-env-proxy build/verify-release.js` 二次核验 PASS(14 项),固定更新
  端点返回 0.3.58;签名/安装包逐字比对按既有口径 SKIP,由 CI 端完成。
- CI 耗时诊断(本 run 实测):step 总和 612 秒,其中 Install cargo-tauri 403 秒(66%)
  —— cargo-tauri 缓存因 GitHub 缓存作用域隔离(tag run 只能读 default branch 与当前
  tag 自己的缓存)从未跨 run 命中,每次发版都在 cargo install tauri-cli 现场编译。
  优化方案(换官方预编译 @tauri-apps/cli 等)已分析,待用户拍板后实施。
- 待验收:明后天与周重置日期在真实数据与原生窗口的显示;Antigravity 真实账号取数;
  日期控件回填人工复核(遗留)。

## 2026-11-16 每轮取数合并推送与隐藏窗口降载（v0.3.59）

- 按 `docs/report/performance-audit-2026-11-16.md` 的拍板批次实施高确定性优化，全部落新模块或既有函数内最小改动：
  1) **P1-1**：新增 `app/core/src/progress.rs`，同一轮取数的渐进结果首个渠道立即推送，其余 150 ms 合批，终态仍由 `poll_loop` / `get_channels` 推送；`finish()` 清脏，Drive 定时器随 future 结束停止，不存在迟到覆盖。
  2) **P1-2**：Rust 在 `window_cmd("hide")`、全局热键、托盘菜单三个隐藏入口补发 `window-hidden`；前端维护 `WINDOW_VISIBLE`，隐藏时只更新托盘胶囊/数字，唤回时补一次全量 `render()` 与分钟 ticker；数据接收、托盘、签到、同步、网络请求不门控。
  3) **P2-1**：新增 `app/core/src/workbuddy_cycle.rs`，每轮取数共享一份桌面端凭据快照，国内/国际令牌与账户指纹同源；真机日志每轮「解开 live 凭据」由 6 条（3 遍扫描）降为 2 条（1 遍扫描）。
  4) **§3.1**：`google_cloud::LedgerCache` 加 60 s 兜底 TTL + 写入修订号，后台调度不再每 5 s 克隆整份 Config、也不再每 5 s 解析账本 JSON；hidden / pending / 90 s 超时语义不变。
  5) **§3.2**：`checkin_state` 的凭据探测改 `join_all` 并发，按索引回写 note；隐藏渠道不探测的红线不变。
  6) **§3.3**：缩略卡 `fit()` 先批量读宽高、再集中写字体，保留原有像素级宽度算法。
  7) **§3.5.3**：`#cfgPill` / `#mIndexList` 改 `qReconcile` 按 key 复用节点，`updateHeaderLive` 改 `setTextIfChanged`。
- 测试补充：`progress.rs` 覆盖合批、flush 停止与跨轮 reset；`google_cloud.rs` 覆盖写入重读、60 s 兜底与外部损坏；`workbuddy_cycle.rs` 覆盖同轮单读与换账号；`jsdom-smoke.js` 新增「设置选择与索引复用按钮」「隐藏窗口保留列表且继续更新托盘来源」「唤回窗口显示隐藏期间的新数据」，并内置 `PERFORMANCE_SENTINEL` 反向哨兵（关闭门控或退回 `innerHTML` 重建时必须变红）。
- 修复本轮首跑暴露的测试竞态：`google_cloud` 缓存的修订号是模块级 `AtomicU64`，`cargo test` 默认并行下会被同模块其它用例推进，导致「5 秒内不重读」随机失败；改为 `LedgerCache::get_at(store, now, revision)` 注入修订号，另断言真实写入确实推进全局修订号。结论已写入 `AGENTS.md` §7.3。
- 门禁串行复跑（NO_PROXY=127.0.0.1,localhost,::1）：IPC 契约 133 调用 / 103 命令、静默失败 7 脚本 74 处 catch、文件规模、CSS 形态、`node --check app.js`、jsdom 238/238、顾问/Google Cloud/密钥箱/网关/托盘冒烟、core `cargo test --locked` 566 passed / 8 ignored（`sync_fixtures` 6 passed）、src-tauri `cargo check --all-targets --locked`、凭据扫描 CLEAN。`verify-tray.js` 128/128（临时目录安装 `playwright-core` + 系统 Edge，未改仓库依赖）。
- 真机 release 实测（`build.ps1 build` 产物）：14 渠道完整渲染；设置索引/子页/胶囊选择可点且持久化；隐藏后主窗口不可见、托盘进程存活，二次启动触发单实例回调唤回，唤回后全量重绘、单列/缩略切换正常；手动刷新终态完整；空闲 30 s CPU 0%、工作集约 60 MB；验证后已恢复安装版进程。
- 三处版本号同步至 `0.3.59`（`Cargo.toml` / `tauri.conf.json` / `Cargo.lock`），`cargo fetch --locked` 通过。未实施：P2-2 单事务、P3-2 VACUUM、P3-3 Antigravity、§3.5.2 签名墙钟文本。

## 2026-11-16 v0.3.59 发布结果

- tag `v0.3.59` 指向 `757b139`（取数推送合批与隐藏窗口降载发版 commit）。release run
  `43889962747` 全部成功（job 14:50:20Z → 15:14:35Z）；main CI run `43889935390` 对
  `757b139` 成功。
- Release 于北京时间 2026-11-16 23:14:35 公开发布,三件套齐全(安装包
  `Grandettoken_0.3.59_x64-setup.exe` `6,036,936` 字节;签名 `.sig` `444` 字节;
  更新清单 `latest.json` `2,053` 字节),非 draft。
- 本机 `node --use-env-proxy build/verify-release.js` 二次核验 PASS(13 项,含固定
  更新端点返回 `0.3.59`、安装包匿名下载 200 且 SHA-256
  `347e45a243afea00277a816e0c8ee9ae5e544f9a262182481cc82ed3123569bf`;本地无 `.sig`
  与安装包文件两项按既有口径 SKIP,由 CI 端完成)。
- 说明完整:697 字符 / 269 中文字符,0 替换字符。
- 待验收:真机安装版自动更新;每轮推送合批与隐藏态降载在真实使用中的观感
  (渐进显示、托盘轮播、唤回刷新);P2-2 / P3-2 / P3-3 / §3.5.2 未实施项仍待拍板。

## 2026-11-17 Antigravity 多账号跟随采集与独立卡片（工作区，隐藏采集口径待裁决）

- 设计见 `docs/design/design-antigravity-accounts-2026-11-17.md`。按用户确认的交互，各账号独立成卡，单列上下排列、双列与缩略沿用网格；管理页共用一个分组，账号分别备注、隐藏和排序。当前账号显示采集时间，历史账号保留最后成功额度；跨重置后提示待刷新。
- 本机 `/usage` 实测缺少账号身份，改用官方公开 `statusLine` 同一回调的 email 与 quota。print 模式实测不触发回调，Windows 使用临时 ConPTY、单次 settings 文件和系统 PowerShell EncodedCommand。邮箱在回调内生成指纹，落盘白名单只有指纹与额度；采集保持空输入，结束或超时通过 Job Object 清理子进程树。
- 主体逻辑拆到 `antigravity_console.rs`、`antigravity_accounts.rs` 与 `antigravity_capture.ps1`。成功和失败共用至少 120 秒启动冷却，连续失败使用既有失败退避设置；轮询、手动刷新和测试连接共用采集缓存。阻塞任务内持有单飞锁，调用方取消 await 后仍完成进程清理与冷却记账。
- 账号账本一次落盘，写入失败保留旧账本并反馈到界面；旧版无归属快照以「账号待确认」保留。其它渠道重试时根据账号卡还原 Antigravity 的缓存状态，保持选择性取数边界。各账号同步平台键使用 `antigravity:<指纹>`，Android 品牌与窗口选择按账号前缀兼容；旧胶囊与同步分组选择继续覆盖已识别账号。
- 首轮问题与修正记录保留在本地验证输出。Rust 回调最初因 Windows 多层引号超时，改为 EncodedCommand 与单次 settings 文件后真机通过；cmd 启动器夹具发现系统路径中的正斜杠被当作开关，逐段构造原生路径后单独复测通过。冷却测试最初在设置错误结果前就断言错误，修正夹具顺序后独立用例与全量复跑通过。新增账号哨兵最初被已有签到自检的失败遮盖，修正结果保留并断言具体账号分组 FAIL 行后反向验证成立。
- 浏览器首轮双列检查缺少真实窗口的 `data-w` 前置，补齐几何状态后复核。视觉检查发现缩略卡的账号时间挤掉 5 小时额度，改为额度与账号状态分列，并补上实际文字裁切断言。浏览器覆盖单列、双列、缩略、深浅主题与三档字号；管理页检查账号控件在窄窗大字号下的换行与可达性。截图、夹具与日志保存在 gitignore 的 `build/qa/antigravity-accounts/` 与 `build/tmp/`，一次性探针脚本已删除。
- 已串行完成并独立复核静态 IPC / 静默失败 / 文件规模 / CSS 门与自检、前端 JS 语法、jsdom（含账号分组坏样本）、顾问 / Google Cloud / 密钥箱 / 网关冒烟、回调白名单正反样本、core `cargo test --locked`、src-tauri `cargo check --all-targets --locked`、`cargo fetch --locked`。验证进程设置回环 NO_PROXY。Android 单测与 Kotlin 编译通过，并以 `--rerun-tasks` 独立复跑。当前真实账号的官方回调、账本保存、账号卡展开及探活缓存复用已实测通过；真实双账号切换仍待用户验收。
- 用户待裁决项已通过问题卡明确提出。官方回调把账号身份和额度一起返回，当前工作区实现会在采样后跳过隐藏账号的写入；这与 AGENTS.md 的隐藏后停止探测约束存在口径差异。另一方案是在有账号卡隐藏时暂停整个分组采集。该项获裁决前保留为待审工作区改动。发布基线仍为 v0.3.59，第三方查询仍受 Google 平台条款约束。

## 2026-11-18 Antigravity 账号卡隐藏口径确认（工作区）

- 用户明确确认「隐藏账号卡并冻结其记录，保留当前会话识别；隐藏整个 Antigravity 分组才停止 CLI」。已写入 AGENTS.md 第 5 节，更新设计文档、DESIGN.md 与两份 README，前一条进展中的待裁决项已关闭。
- 管理按钮明确命名为「隐藏账号卡」，账号列表增加冻结记录、保留识别及整组停止采集的说明。恢复条在含 Antigravity 账号卡时同步说明该例外，避免沿用隐藏后停止取数的笼统提示。
- 回归补充隐藏账号 A 后切到可见账号 B，验证 A 的额度和采集时间保持原值，B 可更新并成为当前账号。前端同时检查隐藏说明和恢复提示，沿用既有账号隔离、保存失败回滚、坏样本和分组隐藏覆盖。
- 本轮串行跑过静态 IPC / 静默失败 / 文件规模 / CSS 门及自检、前端 JS 语法、回调白名单、jsdom、顾问 / Google Cloud / 密钥箱 / 网关冒烟、core `cargo test --locked`、src-tauri `cargo check --all-targets --locked` 与 `cargo fetch --locked`。前端冒烟、core 全量与 src-tauri 编译已独立复跑。浏览器复核深浅主题及标准、大字号下新增说明和账号控件的排版，日志与截图仍放在被 gitignore 的本地验证目录。
- 功能改动保留在工作区，发布基线仍为 v0.3.59。真实双账号切换及安装版显示待验收，平台条款风险仍存在。

## 2026-11-18 当天到期日期修正（工作区）

- WorkBuddy 缩略卡的「11-18 本周三」由共享日期格式漏判当天导致。积分卡和周重置日期现在优先显示「今天 / 明天 / 后天」。日期气泡保留完整星期和时刻。
- 用 11 月 18 日 10:26 到期的固定时钟夹具复现旧文案。修正后，跨过午夜的分钟更新把卡片从「明天」改为「今天」，三种布局与首页摘要一致。周额度重置也覆盖当天文案与辅助提示。
- 静态门及自检、前端语法、jsdom、Codex 重置顾问、core 单测、Tauri 全目标编译和凭据扫描通过。

## 2026-11-18 v0.3.60 发版验证

- 发版内容包括 Antigravity 多账号卡与独立隐藏、账号快照同步兼容，以及积分卡和周重置日期的当天显示修正。桌面版本号三处同步至 0.3.60。
- 本地串行复跑静态门及自检、前端语法、jsdom、官方回调白名单、顾问 / Google Cloud / 密钥箱 / 网关冒烟、core 单测、Tauri 全目标编译、`cargo fetch --locked` 和凭据扫描。凭据扫描覆盖待提交文件。
- 当前真实 Antigravity 账号的采集、账本和探活复用通过。Android Debug 单测及 Kotlin 编译通过。真实双账号切换与安装后的界面显示待验收。

## 2026-11-18 v0.3.60 发布结果

- `v0.3.60` 指向 `2edfcb3`，该提交先推送到 `main`。main CI `44222968581` 和桌面发版 `44222979277` 均完成并通过。
- Release 于北京时间 10:49:10 公开发布。Windows x64 安装包为 6,054,805 字节，签名及更新清单均已公开。`releases/latest/download/latest.json` 指向 0.3.60。
- CI 和本地 `verify-release.js` 均通过。本地匿名下载三件套并用应用配置的公钥验签，更新清单内签名与 `.sig` 一致，篡改安装包的坏样本验签失败。下载所得 SHA-256 与 GitHub 资产记录一致，为 `e1a49e84a5fd4a803882bf8f2601298ae5e1cd4c3d3102d73eecbf71973dfe3c`。
- Android 同步兼容改动通过单测与编译，本次发布 Windows 桌面安装包。真实双账号切换与安装后的界面显示待验收。

## 2026-11-18 大字号面板工作区适配（工作区）

- 面板以 Tauri 当前显示器工作区计算最大尺寸和窗口落点。单列、双列与缩略共用计算，贴边展开、解除贴边和同 DPI 跨屏停稳也应用对应工作区。保留所选字号，列表超出可用高度时滚动。
- 隔离的 Tauri / WebView2 测试窗口读取本机 1920×1080、125% 的实际工作区为 1920×1020。大字号为 1.15 倍、特大字号为 1.3 倍，适配后视口高 816 逻辑像素。合成渠道覆盖列表溢出，三种布局的标题栏、底栏与设置按钮完整可见，列表可滚到末卡。左右贴边往返通过。测试数据和调试配置与已安装应用独立。
- 前端回归覆盖普通与贴边面板，以及同 DPI 跨屏后重新适配并保存落点。整屏替代工作区的坏样本触发相应断言。静态门、前端冒烟、重置顾问、core 单测、Tauri 全目标编译和凭据扫描均通过。
- 真机形态校验按当前布局和字号读回目标尺寸，并核对测试窗口原生样式。面板、紧凑条及胶囊尺寸往返和胶囊圆角透明通过，面板无原生缩放边框及最大化按钮。

## 2026-11-18 v0.3.61 发版验证

- 桌面版本号三处同步至 0.3.61，本版包含 Windows 大字号面板的工作区适配。
- 串行通过静态门及自检、前端语法、jsdom、密钥箱 / 顾问 / Google Cloud / Antigravity / 网关冒烟、core 单测、Tauri 全目标编译、锁文件检查和凭据扫描。jsdom、core 和 Tauri 已单独复跑。

## 2026-11-18 v0.3.61 发布结果

- `v0.3.61` 指向 `43e7f6b`，该提交先推送到 `main`。桌面发版 `44234837093` 的门禁、构建签名和发布产物核验均通过。
- Release 于北京时间 12:20:37 公开发布。Windows x64 安装包为 6,055,752 字节，签名和更新清单均已公开，固定更新端点指向 0.3.61。
- 本机 `verify-release.js` 通过。匿名下载三件套后用应用内公钥验签，公开 `.sig` 与更新清单逐字一致，篡改安装包的坏样本验签失败。安装包 SHA-256 与发布资产元数据一致，为 `4c01a0789c00fb3082f474e2b783e0990f5a09d22a50c0ebcd163fb740a91616`。
- main CI `44234827128` 首轮在 Antigravity 回调夹具的首个 PowerShell 子进程未取得退出码，用时与夹具的十秒超时吻合。同一提交的发版流程及本地回调夹具均通过。独立重跑的第二轮 main CI 全部通过。

## 2026-11-18 Qoder 账户到期与逐笔临期排查（工作区，待发版）

- 只读核对本机结构化快照：Qoder 国内版显示的 11 月 22 日 00:31 是账户额度到期，当前逐笔明细未知。原首页只筛逐笔积分，导致账户临期被漏掉；账户有效期不代表余额里的每笔积分都在同一时刻到期。
- 本机诊断中两区网页明细请求从 11 月 17 日晚已出现会话被平台拒绝，早于 11 月 18 日的 v0.3.60 与 v0.3.61 发布；现有证据不支持断言新版本主动清掉登录。逐笔列表的失败回退缓存在进程内，重启会失去缓存，可能使之前已有的会话问题变得明显。恢复真实逐笔明细仍需在对应区域用当前账号重新登录并观察平台响应，本轮没有操作用户的网页会话。
- 首页临期入口现在分别识别逐笔积分和账户额度到期，金额仅加逐笔积分；可仅凭账户日期筛选，混合事件按更早的到期时刻排序。三种布局给账户日期保留明确的账户范围。七天窗口以今天起七个本地自然日为准，结束边界、柱图和筛选统一，修正跨夏令时及边界零点的差异。
- 网页明细沿用同一账号的旧列表时写入结构化 `expiryStale`，保留详情但不再作为当前已确认积分、状态风险或三十天汇总；旧快照与当前明细可区分，旧快照字段缺失可兼容读取。同步 v1 没有逐笔数据新旧字段，故不把旧逐笔列表作为当前明细同步。平台拒绝、结构未识别与网络错误的提示保持区别，旧列表不再显示成当前已登录。
- 前端与 Rust 回归覆盖账户单独临期、混合金额与排序、旧数据、跨到期时刻、同步、失败提示及上海/纽约自然日与夏令时边界；临期门禁对漏掉账户、把旧明细算入当前金额及用固定小时替代自然日分别注入坏样本。后端故意移除旧明细标记时对应单测变红，恢复后复绿。真实渲染的合成夹具检查三种布局和三档字号，混合摘要缩短后无文字截断。
- 首轮浏览器排版验证在旧的「本周日」期望处失败，实际实现已于 v0.3.60 改为「今天」；已修正验证期望并独立复跑。首轮假 DOM 探针因桩缺少 `isVisible()` 报错，已补齐并单独复跑。原始结果保存在被忽略的 `build/qa/expiry-qoder/`。
- 最终改动后两轮串行通过 IPC、静默失败、文件规模及 CSS 门和自检、前端语法、临期正反样本、jsdom、顾问 / Google Cloud / 密钥箱 / 网关冒烟、前端探针、浏览器排版、core `cargo test --locked`、Tauri `cargo check --all-targets --locked` 与凭据扫描。验证进程设置回环 `NO_PROXY`；需真实账号或特定本机条件的忽略用例仍按测试定义未运行。Qoder 真机验证脚本已按新提示语更新，分类自检独立复跑通过；其会打开登录窗口的真实交互未在本轮执行。
- 改动保留在工作区，尚未发版或安装；原生安装版的筛选显示和重新登录后的逐笔明细仍需真实验收。

## 2026-11-18 v0.3.62 发版验证

- 桌面版本号三处同步至 0.3.62。本版修正 Qoder 账户额度临期筛选、逐笔明细失效提示及七个自然日的统计边界。
- 本地串行通过静态门及自检、前端语法、临期与 Qoder 回归、jsdom、密钥箱、顾问、Google Cloud、Antigravity、网关冒烟、core 单测、Tauri 全目标编译、锁文件检查和凭据扫描。

## 2026-11-18 v0.3.62 发布结果

- tag `v0.3.62` 指向 `b6227d5`，该提交先推送到 `main`。桌面发版 run `44254517449` 的 `build-and-release` 全部步骤成功；Release 于北京时间 15:11:27 公开发布，非 draft / prerelease。
- 三件套齐全：安装包 `Grandettoken_0.3.62_x64-setup.exe` 6,056,093 字节，GitHub 资产记录 SHA-256 `4b566e2a609cbae6270055d40a8df704cac962732aa571aebbb7b3928823aff8`；签名 `.sig` 444 字节；更新清单 `latest.json` 1,337 字节（notes 249 字符 / 163 中文字符、0 替换字符）。固定更新端点 `latest/download/latest.json` 返回 0.3.62。
- 本机 `node --use-env-proxy build/verify-release.js` PASS（13 项；本地 `.sig` 与安装包两项按既有口径 SKIP，签名逐字比对在 CI 端完成）。`build/verify-antigravity-capture.js` 本机独立复跑 PASS。
- main CI `44254504241` 首轮在「Antigravity callback whitelist」步骤以 exit 1 结束：该步用时 11 秒，与夹具 `spawnSync` 的 10 秒超时吻合；同一提交的发版流程与本机同一夹具均通过，形态与 v0.3.61 首轮相同。
- 该假红按 AGENTS.md §6.7 新增条目修复：`build/verify-antigravity-capture.js` 的超时改为 30 秒，失败信息补上 `status` / `signal` / `error` / 截断后的 `stderr`，提交 `08d1865`（不动产品代码）。推送后 main CI `44259165813` 全部步骤通过，该步骤用时 6 秒；`b6227d5` 那一轮仍保留为红，记录不改写。修复前后本机复跑 `verify-antigravity-capture.js`、静态门及自检、jsdom 275/275、顾问冒烟、core `580 passed / 10 ignored`、Tauri 全目标编译与凭据扫描均通过。

## 2026-11-18 v0.3.63 网关上游默认 User-Agent 随版发布

- 本地模型网关请求上游时默认附带 `User-Agent: OpenAI/JS 4.77.0`（`gateway_proto::sources::DEFAULT_UPSTREAM_USER_AGENT`），同时写入 `proxy::build_client` 与 `build_upstream_headers`，避免第三方中转站前置 Cloudflare / WAF 因空 `User-Agent` 返回 403 且不暴露特定桌面软件指纹；渠道在 `customHeaders` 中配置非空 `User-Agent`（不区分大小写）时优先使用自定义值。
- 双向验证：`sources::tests::upstream_headers_default_user_agent_and_allow_custom_override` 与 `proxy::tests::proxy_changes_refresh_the_client_and_direct_groups_bypass_it` 覆盖默认 UA、自定义覆盖与真实 HTTP 报文头；临时移除 `build_client` 的 `.user_agent(...)` 时 `proxy` 单测按预期变红，恢复后复绿。
- 桌面版本号三处同步至 `0.3.63`（`Cargo.toml` / `tauri.conf.json` / `Cargo.lock`）。本地串行通过静态四门及自检、临期与 Qoder 门禁、9 个前端/捕获 JS 语法、jsdom 275/275、密钥箱 / Codex 重置顾问 / Google Cloud / Antigravity / 网关冒烟、`app/core` `cargo test --locked -- --test-threads=1`（581 passed / 10 ignored + `sync_fixtures` 6 passed）、`app/src-tauri` `cargo fetch --locked` 与 `cargo check --all-targets --locked`、凭据扫描 CLEAN。

## 2026-11-18 v0.3.63 发布结果

- tag `v0.3.63` 指向 `fffb4b0`，该提交先推送到 `main`。main CI `44287851779` 与桌面发版 run `44287896071` 的 `build-and-release` 全部步骤一次通过；Release 于北京时间 19:00:55（`2026-11-18T11:00:55Z`）公开发布，非 draft / prerelease。
- 三件套齐全：安装包 `Grandettoken_0.3.63_x64-setup.exe` 6,057,308 字节，GitHub 资产记录与本机匿名下载回验 SHA-256 均为 `7d7d3f6a7ef4c61da05f4a0a53647fb5901489447902dcb591e38cd57a701f7e`；签名 `.sig` 444 字节；更新清单 `latest.json` 1,387 字节（notes 311 字符 / 157 中文字符、0 替换字符）。固定更新端点 `latest/download/latest.json` 返回 `0.3.63`。
- 本机 `node --use-env-proxy build/verify-release.js` 二次核验 PASS（本地 `.sig` 与安装包文件两项按既有口径 SKIP，签名逐字比对在 CI 端完成）。

## 2026-11-18 本地模型网关支持 Responses（工作区）

- 按用户确认的接口口径，保留客户端现有 `POST /v1/chat/completions`，仅 OpenAI 兼容渠道上游可切换为 Responses。旧渠道默认沿用 Chat Completions。
- 适配器把历史消息、工具及关联结果映射成 Responses input，再把上游 JSON 与 SSE 转回 Chat Completions，保留模型别名、推理摘要、函数调用、完成原因和用量。Responses 渠道填写手动模型后跳过 `/models`，联调复用同一转换路径。
- 隔离的回环 HTTP 夹具从同一个 Chat Completions 入口验证传统聊天上游与 Responses 上游，覆盖 JSON、SSE、工具结果、用量和鉴权。移除工具结果映射的坏样本在上游断言处失败。隔离程序复用网关源码，规避本机原生 Tauri 测试二进制的 WebView2 加载问题。安装版与真实 AgentRouter 接入待验收。

## 2026-11-18 底栏设置/退出常驻与网关忙碌态修复（工作区）

- 底栏去掉「•••」更多菜单，把「设置」改成加宽的主按钮，并把「退出」直接常驻在它右边；两者的高度和 `#dockClose` 预留空间改用统一的 `--foot-height`，详情弹层的底部边界同步到同一变量。底栏在缩略布局沿用同一高度，避免退出入口在窄窗或大字号下被挤压。
- 网关操作按钮统一加 `data-gw-exclusive`，忙碌时按当前 DOM 批量同步禁用态；异步操作跨过一次重绘后，结束只释放当前页面上的按钮，不再解禁已被替换掉的旧节点。联调「发送测试」在自己的 `finally` 里恢复，不占用全局操作锁。
- `jsdom-smoke.js` 断言退出按钮直接位于设置按钮之后、失败后提示并可重试；`verify-gateway.js` 覆盖下架重绘后按钮可用、失败保留草稿可直接上架、复制地址未结束时下架仍持有锁。`verify-settings-entry.js` 改为断言加宽设置按钮与直达退出，并检查三档字号下底栏零溢出；它需要真实窗口 CDP，安装版本轮未运行。
- `README.md` 补充本地模型网关的 Chat Completions / Responses 接入口径与客户端配置示例；`DESIGN.md`、`AGENTS.md` 同步底栏入口与网关协议约束。

## 2026-11-18 v0.3.64 发版验证

- 桌面版本号三处同步至 0.3.64。本版包含本地模型网关 Responses 上游、底栏设置/退出常驻和网关忙碌态修复。
- 本地串行通过静态四门及 `--self-test`、全部前端 JS 语法、jsdom 277/277、临期与 Qoder 门禁、密钥箱 / Codex 重置顾问 / Google Cloud / Antigravity / 网关冒烟、`app/core` `cargo test --locked -- --test-threads=1`（586 passed / 10 ignored + `sync_fixtures` 6 passed）、`app/src-tauri` `cargo fetch --locked` 与 `cargo check --all-targets --locked`、凭据扫描 CLEAN（483 文件）。验证进程设置回环 `NO_PROXY`。
- `build/verify-settings-entry.js` 依赖真实窗口 CDP，本轮未运行；待安装版原生窗口核验。

## 2026-11-18 v0.3.64 发布结果

- tag `v0.3.64` 指向 `991f0e4`，该提交先推送到 `main`。main CI `44330050381` 全部通过。桌面发版 run `44330074895` 的构建、签名、建 Release 与三件套补传均成功；Release 于北京时间 23:21:54（`2026-11-18T15:21:54Z`）公开发布，非 draft / prerelease。
- 安装包 `Grandettoken_0.3.64_x64-setup.exe` 6,080,665 字节，本机匿名下载回验 SHA-256 `b885a25a29f057b63afe1aecd3390120ce1c8c2605acff35500dde122806cc36`；签名 `.sig` 444 字符。
- 首次发版说明只写提交标题，`latest.json.notes` 为 68 字符 / 13 个中文字符，CI 最后一步 `verify-release.js` 的 `notes intact` 判 FAIL；该 run 保留为红，不改写记录。已补齐 GitHub Release 正文并重传 `latest.json`（notes 316 字符 / 174 中文字符 / 0 替换字符，签名与下载地址未改动，资产 1,453 字节），本机 `node --use-env-proxy build/verify-release.js` 全部 PASS。
- 根因门禁前移：`release.yml` 的 notes 长度检查改为与 `verify-release.js` 同一判据（正文至少 20 个中文字符），在打包前失败；`RELEASE.md` 的发版提交示例改为带说明正文，`AGENTS.md` 同步该结论。
- 待验收：安装版真实 AgentRouter / Responses 接入、原生窗口底栏设置与退出按钮、`verify-settings-entry.js` 的 CDP 检查；v0.3.63 及更早的遗留真机核验项仍待完成。

## 2026-11-18 底栏设置与退出按钮统一（工作区）

- 按实机反馈，设置与退出改为同尺寸、同样式的普通按钮：两者共用 `56u` 最小宽度、`32u` 高度和 `12u` 字号，设置去掉主按钮配色；按钮间距保持原样。
- 回归：`jsdom-smoke.js` 断言两者类名一致且设置不带 `p`；`verify-settings-entry.js` 的三档字号几何检查改为断言宽高一致、底色/边框/字号一致，并继续检查底栏零溢出。
- 静态四门、前端语法、jsdom 278/278 通过；`verify-settings-entry.js` 依赖真实窗口 CDP，本轮未运行。

## 2026-11-19 网关诊断、空流识别与联调取消（工作区）

- 网关诊断新增首字耗时、帧数、工具调用次数与参数字符数、token 用量、finish_reason 和零输出说明；诊断列表按状态区分错误与警告，错误和零输出同时写入脱敏诊断日志，便于定位上游空流、代理拦截和结构不兼容。
- 流式处理补齐聊天上游的终止判定，收到 `[DONE]` 后不再继续等待 EOF；只收到结束标记但没有任何有效帧时明确返回 `incomplete_stream`，非 JSON 数据会保留片段并在诊断中说明，同时兼容 OpenAI 多段 content 数组与 Gemini `candidates` 结构。
- 「模型与联调」的发送按钮显示已等待秒数，并新增取消按钮；取消会中止后端联调任务，前端按运行 ID 丢弃迟到结果，不会覆盖取消后的界面状态。
- 回归覆盖空流错误、工具调用统计、诊断字段展示、取消命令调用与按钮恢复；`verify-gateway.js` 全部通过，jsdom 278/278，core 587 passed / 10 ignored，src-tauri `cargo check --all-targets --locked` 通过。

## 2026-11-19 v0.3.65 发布结果

- 首次 v0.3.65 发版在用户要求下取消并删除 tag，未产生 Release。合并网关诊断与联调取消改动后重新打 tag，`v0.3.65` 指向 `bc74284`，该提交先推送到 `main`。
- main CI `44342986043` 与桌面发版 run `44342998823` 全部通过。Release 于北京时间 2026-11-19 00:29:05（`2026-11-18T16:29:05Z`）公开发布，非 draft / prerelease。
- 三件套齐全：安装包 `Grandettoken_0.3.65_x64-setup.exe` 6,087,045 字节，本机匿名下载回验 SHA-256 `829d0660e18085fc46ce7b8bbc38fc669f2e89562f199b1cc8f0074108c1d26b`；签名 `.sig` 444 字节；更新清单 `latest.json` 1,270 字节（notes 238 字符 / 132 中文字符、0 替换字符）。固定更新端点返回 `0.3.65`。
- 本机 `node --use-env-proxy build/verify-release.js` 二次核验 PASS；本地 `.sig` 与安装包两项按既有口径 SKIP，签名逐字比对在 CI 端完成。
- 待验收：安装版底栏设置/退出按钮的实际观感，以及网关诊断、空流提示和联调取消在真实 AgentRouter / Responses 上游下的表现。

## 2026-11-19 Windows 桌面窗口无响应

- 现场安装版 v0.3.64 的窗口被 Windows 判定无响应。两次附加 GDB 都看到主线程停在同一处 `WaitOnAddress`。用 PE 导入表和 COM 接口标识回查，堆栈下方是 Wry 同步读取 Cookie 时运行的 `DispatchMessageA` 嵌套循环，上方是重入的 WebView2 导航回调尝试取得 Tauri 的 WebView 注册表锁。后台调用的三秒超时无法解除界面线程的等待。采集线程栈后重启旧版，窗口恢复响应。
- WorkBuddy、Qoder 两个区域和豆包的网页登录会话共用 `webview_cookies.rs`。Windows 界面线程登记 `GetCookies` 回调后立即返回，工作线程负责等待和超时。清理保留原始域名及路径，任何入口读取失败都会先返回错误。原生回归用隔离 profile 验证回调时序及同名 Cookie 的域名和路径隔离。
- 原生示例首轮因缺少 Common Controls v6 manifest 在 `TaskDialogIndirect` 处无法启动。给示例链入 Tauri 同一份 Windows 资源后，旧同步路径命中时序断言。另一次原生复跑检出删除请求返回后立即读取仍可能看到旧条目。清理改为在同一截止时间内读回核对。最终正样本连续复跑通过，`--sync-sentinel` 在时序断言处退出 1。用例同时核对前导点域名、同名子路径和其它站点隔离。
- 本机通过替换可执行文件运行基于 v0.3.65 的修复构建，保留原 v0.3.64 可执行文件备份。实机触发刷新、进入设置并回到首页，后台轮询持续记录成功取数。清理回归使用合成会话。
- 串行通过 IPC、静默失败、文件规模、CSS 形态、前端语法、jsdom、Codex 重置顾问、网关冒烟、core 单测、Tauri 全目标编译及凭据扫描。新 Rust 源一并扫描。首轮文件规模门因用 PowerShell 统计时漏计空行而误设预算，按门禁的物理行数修正预算后复跑通过。jsdom、core 和 Tauri 已分别复跑确认。
- 原生回归 `verify_webview_cookies` 正常路径 PASS，`--sync-sentinel` 在时序断言处按预期 FAIL（退出码 1）。

## 2026-11-19 v0.3.66 发版验证

- 桌面版本号三处同步至 0.3.66。本版把 WorkBuddy、Qoder 和豆包的网页会话读写统一到 `webview_cookies.rs`，避免 Windows WebView2 的同步 Cookie API 在界面线程开启嵌套消息循环导致卡死；清理保留原始域名与路径、按名称域名路径去重，并读回核对后才报告成功。
- 本机串行通过静态四门及自检、全部前端 JS 语法、jsdom 278/278、临期与 Qoder 门禁、密钥箱 / Codex 重置顾问 / Google Cloud / Antigravity / 网关冒烟、core `cargo test --locked -- --test-threads=1`（587 passed / 10 ignored + `sync_fixtures` 6 passed）、`app/src-tauri` `cargo fetch --locked` 与 `cargo check --all-targets --locked`、凭据扫描 CLEAN。原生 `verify_webview_cookies` 正常路径 PASS，sentinel 路径按预期 FAIL。



## 2026-11-19 v0.3.66 发布结果

- tag `v0.3.66` 指向 `5ddccf2`，该提交先推送到 `main`。main CI `44442091013` 与桌面发版 run `44442099550` 全部通过。Release 于北京时间 2026-11-19 10:27:09（`2026-11-19T02:27:09Z`）公开发布，非 draft / prerelease。
- 三件套齐全：安装包 `Grandettoken_0.3.66_x64-setup.exe` 6,088,391 字节，本机匿名下载回验 SHA-256 `0486d1082325375ef8d6be46a4adee6976435ac2e09a5655ace191170aeb39f7`；签名 `.sig` 444 字节；更新清单 `latest.json` 1,270 字节（notes 291 字符 / 127 中文字符、0 替换字符）。固定更新端点返回 `0.3.66`。
- 本机 `node --use-env-proxy build/verify-release.js` 二次核验 PASS；本地 `.sig` 与安装包两项按既有口径 SKIP，签名逐字比对在 CI 端完成。
- 待验收：安装版在 Windows 上的无响应问题是否彻底改善，以及网页会话读取、清理和取数链路的真实表现。


## 2026-11-19 底栏设置与退出按钮统一尺寸（工作区）

- 底栏「设置」和「退出」改为与左侧布局切换按钮同尺寸、同样式：取消 56u 最小宽度与 32u 高度，改用 3u/8u 内边距、12u 字号、4px 圆角、透明背景，间距保持不变。
- 回归：`jsdom-smoke.js` 仍断言两者类名一致且设置不带主按钮类；`verify-settings-entry.js` 的几何断言同步为小按钮尺寸（高度至少 14u、宽度至少 24u）并继续检查底栏零溢出。
- 静态四门、前端语法、jsdom 278/278 通过；`verify-settings-entry.js` 依赖真实窗口 CDP，本轮未运行。

## 2026-11-19 v0.3.67 发版验证

- 桌面版本号三处同步至 0.3.67。本版调整底栏「设置」和「退出」按钮尺寸与样式，使其与左侧布局切换按钮保持统一。
- 本机串行通过静态四门及自检、全部前端 JS 语法、jsdom 278/278、临期与 Qoder 门禁、密钥箱 / Codex 重置顾问 / Google Cloud / Antigravity / 网关冒烟、core `cargo test --locked -- --test-threads=1`（587 passed / 10 ignored + `sync_fixtures` 6 passed）、`app/src-tauri` `cargo fetch --locked` 与 `cargo check --all-targets --locked`、凭据扫描 CLEAN。


## 2026-11-19 v0.3.67 发布结果

- tag `v0.3.67` 指向 `2a4416e`，该提交先推送到 `main`。main CI `44445170461` 与桌面发版 run `44445184222` 全部通过。Release 于北京时间 2026-11-19 10:55:51（`2026-11-19T02:55:51Z`）公开发布，非 draft / prerelease。
- 三件套齐全：安装包 `Grandettoken_0.3.67_x64-setup.exe` 6,086,675 字节，本机匿名下载回验 SHA-256 `c1578baa0634e37003b06452e41ce8b31f4d78ec92317f48bd94dcc27b227c14`；签名 `.sig` 444 字节；更新清单 `latest.json` 1,092 字节（notes 180 字符 / 78 中文字符、0 替换字符）。固定更新端点返回 `0.3.67`。
- 本机 `node --use-env-proxy build/verify-release.js` 二次核验 PASS；本地 `.sig` 与安装包两项按既有口径 SKIP，签名逐字比对在 CI 端完成。
- 待验收：底栏按钮与布局切换按钮在真实窗口下的观感统一性。

## 2026-11-20 v0.3.68 发布结果

- tag `v0.3.68` 指向 `aaab970`，该提交先推送到 `main`。main CI `44557993432` 与发版 run `44558459930` 全部通过。Release 于北京时间 2026-11-20 00:10:21（`2026-11-19T16:10:21Z`）公开发布，非 draft / prerelease。
- 三件套齐全：安装包 `Grandettoken_0.3.68_x64-setup.exe` 6,112,977 字节，本机下载回验 SHA-256 `7920222f96c66a0ea53720e2f849813c15762a188a151481176e77fe9784b526`；签名 `.sig` 444 字节；更新清单 `latest.json` 版本 `0.3.68`、notes 1,198 字节，正文与发版提交一致（notes intact）。固定更新端点返回 `0.3.68`。
- 发版前本机复测：全量静态门禁与冒烟、core 647 通过 / 0 失败（10 ignored 真实账户用例未跑）、两 crate check、凭据扫描 CLEAN；网关相关 87 用例按 CI 同款并行线程 0 失败；`src-tauri` release profile 预检通过。`cargo fetch --locked` 因锁文件需随本包版本号更新而报错，改用 `cargo fetch --offline`（仅 tokenscope 版本行与 core 新增 axum 依赖条目），与 RELEASE.md 第 1 步描述不符，后续发版按此口径执行。
- 待验收：原生 WebView 焦点/输入法、真实局域网另一台设备、真实上游与发布版负载；本版未消耗真实模型额度。


## 2026-11-20 Qoder 已用尽账户额度不再提醒到期（本机构建）

- 本机只读核对发现，套餐额度余额为零时，Qoder 仍返回账户到期日；总余额还含附加积分。取数解析按 `userQuota.remaining` 标记已用尽的有期限账户额度。首页、卡片和详情跳过这条提醒，逐笔按实际余额和日期展示。
- 静态门禁、前端冒烟、core 单测、两 crate 全目标检查和凭据扫描均通过。移除详情的已用尽判断后，冒烟命中对应的失败断言。
- 已备份本机原可执行文件并换成本地构建。真实账号刷新后，Qoder 国内版退出七日临期筛选，详情仍显示剩余积分和最近一笔到期明细。

## 2026-11-20 v0.3.69 发布结果

- tag `v0.3.69` 指向 `6bb684b`，发版提交先推到 `main`。main CI `44702533754` 和桌面发版流水线 `44702553936` 均成功。北京时间 16:42:27 公开发布。
- 本版修复 Qoder 账户额度用完仍提示到期，附加积分仍按各笔有效期显示。发版前的本机静态门禁、前端冒烟、Rust 单测和两 crate 全目标检查、锁文件校验及凭据扫描均通过。
- 安装包 `Grandettoken_0.3.69_x64-setup.exe`、签名和 `latest.json` 已公开可用。匿名下载核对了签名与清单、安装包 SHA-256 与 GitHub 资产摘要，以及更新说明与发版提交。固定更新端点返回 `0.3.69`。

## 2026-11-21 套餐临期用量计划（v0.3.70）

- 详细方案见 `docs/product/product-quota-usage-plan-2026-11-21.md`。OpenCode Go、Codex、Antigravity 各自在可重叠的本地在线时段内安排用量。每天最多十小时，每个适用配额池最多安排两个 5 小时窗。
- 首页「今天该用」给出目标和开工时刻，详情显示按天拆分的用量、最晚开工和需补数据。OpenCode Go 取本期月窗，Codex 复用按账号保存的订阅日期，Antigravity 按 Google 账号手填截止并计算 Gemini 配额。用量样本和手填设置按账号隔离。
- 复测补充：在 `quota-plan.css` 为紧凑条与胶囊形态隐藏 `.qp-nudges` 避免撑破横向条；收窄 `needsTick` 仅在存在排程或刚跨过截止时触发重绘；移除 `schedule()` 冗余 `exhausted` 字段。串行通过全部静态门禁、jsdom 与各专项界面冒烟、core 单测、两 crate 全目标检查及凭据扫描。未做原生窗口和真实账号用量核验。

## 2026-11-21 v0.3.70 发布结果

- tag `v0.3.70` 指向 `d6819ca`，发版提交先推到 `main`。main CI `44899888473` 和桌面发版流水线 `44899893638` 均成功。北京时间 20:01:28（`2026-11-21T12:01:28Z`）公开发布。
- 三件套齐全：安装包 `Grandettoken_0.3.70_x64-setup.exe`（6,051,534 字节，SHA-256 `ed1069bd07919543a7228185372c8748d0a251558d468040b50ddbc0e8563d97`）、签名 `.sig`（444 字符）和 `latest.json`（1,263 字节，notes 233 字符 / 133 中文字符 / 0 替换字符）。固定更新端点返回 `0.3.70`，`verify-release.js` 核验通过（本地无产物两项按既有口径 SKIP）。

## 2026-11-22 阶段 0 文档卫生与发布安全(工作区,未提交)

- **PROGRESS.md 编码损坏复原**:v0.3.66 发布结果、底栏按钮统一尺寸、v0.3.67 发版验证 / 发布结果四段共 46 处非法 UTF-8。形态是 GBK 往返:每个受损汉字只剩前两个字节,第三字节连同紧跟的一个 ASCII 字符(空格、换行或数字)被写成一个 `?`,所以几条列表被粘成了一行。直接读 `.git` 对象库核对过,**git 里首个版本就已损坏**,历史中没有干净原文。复原依据:残留的两个字节把每处限定在 64 个候选字之内,再对照同模板的 v0.3.65 条目;被吞掉的数字按源码与前文核对 —— `styles.css` 的 `.foot .btn` 为 `3px/8px` 内边距、`12px` 字号、`4px` 圆角,`verify-settings-entry.js` 断言高度 ≥ 14u、宽度 ≥ 24u,core 用例数与 v0.3.66 条目一致为 587。
- 同类问题另有两处 U+FFFD,同样首版即损坏:`app/core/src/fetch.rs` 一条测试断言消息(按上一行注释复原为「是服务端/限流问题」,只改字符串,不动逻辑与行数),以及 `docs/product/product-logging-observability-2026-10-21.md` 一处(按 `diaglog.rs` 同句复原为「最有用的信息」)。
- **新门禁 `build/check-encoding.js`**:扫描 `git ls-files` 里的文本文件,要求合法 UTF-8(允许 BOM)且不含 U+FFFD,`--self-test` 含与本次同形的损坏样本。因为 `ci.yml` 对 `**.md` / `docs/**` 设了 paths-ignore(纯文档提交不跑 CI,本次损坏正是从这里漏进来的),门禁放在新的 `.github/workflows/text-encoding.yml`(无路径过滤),并加进 `release.yml` 的 Pre-release gates 与 `AGENTS.md` §4.1。
- **归档**:v0.3.50 之前的条目及早期参考章节原样移至 `docs/report/progress-archive-2026-11-22.md`(正文逐行核对一致),本文件从约 460 KB 降到约 76 KB;头部同步更新状态并加归档指引。仓库里另有几处「见 PROGRESS.md 的 v0.1.24 / v0.2.0 / v0.3.0 / 追加调整 12」引用,在本次归档前就已经找不到对应章节,本轮未改。
- **RELEASE.md**:第 1 步改为 `cargo fetch --offline` → `git diff` 核对 lock → `cargo fetch --locked` 复核,并写明原因(v0.3.68 实测);新增「Android 发版 → 6. 签名材料备份与恢复演练」;桌面私钥一节注明 Actions secret 不算备份。
- **新增 `docs/report/acceptance-backlog.md`**:收拢 v0.3.50 以来散落的待验收项(发布安全 / 原生窗口 / 真实账号 / 网关 / Android),`AGENTS.md` §0 加指引。
- 验证:门禁逻辑在沙箱中对修复前的三个文件判红(46 处 / 1 处 / 1 处)、修复后判绿,8 个自检样本全部符合预期;另用 Python 在本机按同一规则全仓扫描,结果为零。**未跑**:`node build/check-encoding.js` 本机实跑、其余 §4.1 门禁、CI。本轮工具环境不能执行 `git` 和 `node`,需要在本机串行跑一遍门禁后再提交。
- `AGENTS.md` 现为 22,520 字节,离 22 KB(22,528)上限只剩 8 字节;本次「工具按 GBK 读写」的教训因此只写在门禁头注释与本条目里,下次往 AGENTS.md 加内容前需要先精简旧条目。
- 待用户执行:签名材料离线备份与恢复演练(待验收清单 A1)。

## 2026-11-22 精简 AGENTS.md(工作区,未提交)

- 起因:阶段 0 之后 `AGENTS.md` 为 22,520 字节,离 22 KB 上限只剩 8 字节,`app.js` 拆分(1a / 1b)要登记新门禁也没有空间。
- 精简后约 19.7 KB(余量约 2.8 KB)。**34 个章节标题与编号不变**(脚本注释和报告按 `2.3`、`2.1.6`、`6.6`、`8.8` 等编号引用);**§4.1 门禁清单与 §5 用户裁决逐字未动**,两者都用脚本与原文比对过。压缩的是 §1、§6、§8 的案例叙述:去掉过程和已失效的细节,保留规则和原因。初稿压缩过头的几条规则性内容(§8.2 顾问判定、§8.4 核验项、§8.6 网关回归清单、§6.7 两个已知不稳定用例名)已逐条补回。
- 精简前全文原样存于 `docs/report/agents-md-archive-2026-11-22.md`,只供查细节,不是现行规则;「写入纪律」一节已加这条指引,并写明「章节编号不要改」。
- 同时写入 §2.2:阶段 0 的编码教训(文本一律按 UTF-8 读写,PowerShell 5.1 须显式 `-Encoding utf8`,`check-encoding.js` 与独立的 `text-encoding.yml`);§0 门禁事实源加上 `text-encoding.yml`。
- 已知遗留(本轮未改):`PROGRESS.md` 与 `docs/report/test-run-2026-11-18.md` 里有三处「AGENTS.md §7.3」,指向的是更早版本的章节,当前文件没有 §7.3(对应内容现在在 §6.7、§8.6)。历史记录不改写。
- 随后按用户要求**删除 §5「用户裁决」整节**(11 条裁决表),§8 里指向 §5 的括注一并去掉;其余章节编号不变,§5 留空不复用。删除后约 18.1 KB。随后按用户要求把这张裁决表原样移到 `DESIGN.md` 新增的 §16(表内「详见 §8.x」改写为「详见 `AGENTS.md` §8.x」),`AGENTS.md` §0 事实源表指向它。
- 验证:本机 Python 检查新文件合法 UTF-8、无 U+FFFD,文中引用的 20 个仓库路径均存在。**未跑** `node build/check-encoding.js` 与其余门禁,提交前需在本机串行跑一遍。

## 2026-11-22 优化设计文档(执行版)

- 新增 `docs/design/optimization-design-2026-11-22.md`,供其它 agent 领取执行:19 项问题(结构 A1–A6、性能 B1–B6、稳定性 C1–C2、验收与流程 D1–D4)的现状证据、方案、技术路线、20 张任务卡、分层测试路径、并行波次与文件独占规则,以及 8 项待用户拍板事项。
- 写文档前重新核对了源码,更正会话开头排查的三处结论:推送合批与隐藏态降载已在 v0.3.59 落地;`fetch.rs` 的 `.lock().unwrap()` 是生产 43 处 + 测试 7 处;Rust 大文件里测试代码占比很高(`fetch.rs` ~1562 行、`inject.rs` ~2199 行、`store.rs` ~1047 行),所以把测试模块按 `codex_reset.rs` 的 `#[path]` 先例外移(A1)排在所有拆分之前。
- 本条只新增文档,未改产品代码;文档内行号为当天快照,执行时须重新定位。

## 2026-11-22 T-00 验证并提交 2026-11-22 既有改动

- 按 `docs/design/optimization-design-2026-11-22.md` §6 T-00 核对工作区改动（经用户确认将 `README.md` 的 1 行归档指引一并纳入第 2 个提交），在本机串行跑完 `AGENTS.md` §4.1 全部门禁：
  - 零依赖静态门：`check-encoding.js --self-test`（8 项自检通过）与全仓扫描 PASS、`check-ipc-contract.js`（133 处 invoke / 104 命令）、`check-silent-failures.js`（含 `--self-test`，12 脚本 80 处 catch）、`check-file-size.js`（含 `--self-test`）、`check-css-forms.js`（含 `--self-test`，8 样式文件 1397 条规则）、`verify-expiry.js`、`verify-qoder-expiry.js --self-test`、`verify-quota-plan.js`、13 个 JS 文件 `node --check` 全部 PASS；
  - 界面冒烟（`NO_PROXY=127.0.0.1,localhost,::1`）：`jsdom-smoke.js`（首轮冷启动下哨兵子进程触发 20s `spawnSync` 超时，单独复跑 294/294 PASS）、`verify-vault.js`、`verify-codex-reset.js`、`verify-google-cloud.js`、`verify-antigravity-capture.js`、`verify-gateway.js` 全部 PASS；
  - Rust 与安全扫描：`app/core` `cargo test --locked -- --test-threads=1`（648 passed / 10 ignored + `sync_fixtures` 6 passed）与 `cargo check --all-targets --locked`、`app/src-tauri` `cargo check --all-targets --locked`、`scan-secrets.ps1`（507 文件 CLEAN）全部通过。
- 分 3 个提交落库：①编码修复 + `check-encoding.js` + `text-encoding.yml`；②`RELEASE.md` + `acceptance-backlog.md` + `PROGRESS.md` 归档与 `README.md`；③`AGENTS.md` 精简 + `DESIGN.md` §16 + 两份方案文档。
- 未跑：依赖原生窗口 CDP 的检查（归入后续 T-D1）及需真实账号的 10 项 ignored 用例。

## 2026-11-22 T-A1 Rust 测试模块外移

- 按 `#[path = "..."]` 先例将 6 个 core 大文件的 `#[cfg(test)] mod tests` 原样外移，生产文件不再计入约 6,700 行测试代码：
  - `fetch.rs`（5,264 → 3,708 行）→ `fetch_tests.rs`（1,123 行）+ `fetch_tests_view.rs`（437 行）；
  - `inject.rs`（7,248 → 5,055 行）→ `inject_tests.rs`（950 行，4 个共用辅助函数标 `pub(super)`）+ `inject_tests_trae_dsh.rs`（1,248 行）；
  - `store.rs`（2,996 → 1,952 行，保留 CRLF）→ `store_tests.rs`（1,044 行）；
  - `checkin.rs`（2,220 → 1,518 行）→ `checkin_tests.rs`（702 行）；
  - `providers.rs`（2,094 → 1,505 行）→ `providers_tests.rs`（589 行）；
  - `workbuddy.rs`（2,392 → 1,776 行，保留末尾 `app_roots_for` 测试注入钩子）→ `workbuddy_tests.rs`（616 行）。
- 词法状态机去缩进：对处于多行字符串 / 原始字符串字面量（如 `DSH_PATCH` YAML 夹具）内部的续行保留原字节不剥离前导空白，其余代码行去掉一层 4 空格缩进；所有新文件均 ≤ 1,500 行软上限，同步下调 `build/check-file-size.js` 对应预算。
- 验证：`cargo test --locked -- --list` 与 `-- --ignored --list` 归一化模块前缀后与外移前基线 100% 一致；`app/core` `cargo test --locked -- --test-threads=1`（648 passed / 10 ignored + `sync_fixtures` 6 passed）、两 crate `cargo check --all-targets --locked`、`check-file-size.js`（含 `--self-test`）、`check-encoding.js`（含 `--self-test`）、`jsdom-smoke.js`（294/294，含读取 `checkin.rs` / `providers.rs` 源码文本的 `thumbnail-regression.js`）、各专项冒烟与 `scan-secrets.ps1`（514 文件 CLEAN）全部串行通过。

## 2026-11-22 T-D3 待验收清单纳入发版流程

- 在 `RELEASE.md`「每次发版」第 2 步（发版提交前）与新增第 6 步（同步待验收清单）明确写入：发版提交前将本版新增待验收项记入 `docs/report/acceptance-backlog.md`，发版安装后完成验收并更新状态与日期；同时注明 `build/verify-release.js` 仅检查线上产物，不检查该清单。
- 验证：静态门禁（`check-encoding.js` 含 `--self-test`、`check-ipc-contract.js`、`check-silent-failures.js`、`check-file-size.js`、`check-css-forms.js`）与 `scan-secrets.ps1` 串行通过（本卡仅改动 `RELEASE.md` 与 `PROGRESS.md`，未改动代码）。

## 2026-11-22 T-D2 本地文件验签入口

- `build/verify-signature.js` 新增 `--local <文件> --sig <签名文件> [--pubkey <公钥文件或base64>]` 模式与 `--self-test` 自检：跳过线上下载，直接复用 minisign 校验器与公钥解析逻辑验证本地文件；`RELEASE.md`「Android 发版 → 6. 签名材料备份与恢复演练」第 3 步同步改为调用该入口，`docs/report/acceptance-backlog.md` 的 A2 更新为通过。
- 双向验证：`node build/verify-signature.js --self-test` 使用 `cargo tauri signer generate --ci` 生成临时测试密钥对并签临时文件，验证：①临时公钥验签通过；②篡改一个字节后验签判红；③用 `tauri.conf.json` 正式公钥验证临时签名判红；④`finally` 彻底清理临时密钥与文件（全程不接触正式私钥）。静态门禁与 `scan-secrets.ps1` 串行通过。

## 2026-11-22 T-C2 Antigravity `--version` 探测超时复现与修复

- 先在 `app/core/src/antigravity.rs` 新增 Windows 复现单测 `timeout_with_grandchild_holding_stdout_returns_within_budget`（用 `cmd /C ping -n 6 127.0.0.1` 构造继承标准输出管道写端的孙进程）：旧实现在 400 ms 超时杀掉 `cmd.exe` 后被 `stdout_h.join()` 阻塞到孙进程自然退出，实测耗时 5.11 s 触发断言变红。
- 修复 `run_command_capped`：
  1) Windows 下将子进程加入 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 的 Job Object（`KillOnDropJob`），超时或等待出错时先释放 Job 句柄终止整棵进程树；
  2) `spawn_capped_reader` 改为通过 `mpsc::channel` 回传结果并配合共享缓冲，主线程按剩余预算 `recv_timeout` 等待，超时则 detach 读线程并返回已收集字节（标记 `truncated = true`）。
- 验证：修复后 `timeout_with_grandchild_holding_stdout_returns_within_budget` 耗时 0.51 s 通过（同时断言正常退出 `cmd /C echo 1.5.9` 完整读回且 `truncated == false`）；静态门禁、`app/core` 单线程全量测试（649 passed / 10 ignored + `sync_fixtures` 6 passed）、两 crate `cargo check --all-targets --locked`、`scan-secrets.ps1` 全部通过。

## 2026-11-22 T-B2 Codex 重置顾问：刷新保留旧结果、profile 只读一次与后端耗时采样

- **第一步（`app/src/codex-reset.js`）**：
  1) 刷新重算时保留已有的 `state.advice`，仅设 `state.refreshing = true`（首次尚无结果时仍设 `state.loading = true` 显示加载中），消除每分钟重算时的「正在计算建议…」界面闪烁；
  2) `state.profileLoaded` 为真时跳过 `codex_reset_profile_get`，每分钟定时器的 IPC 从 2 次降为 1 次；同时抽离 `refreshNeedFreshness()` 在跳过读取时仍按当前时钟校验 `soon`（今天要用）跨本地 0 点与 12 小时新鲜度；在保存或清空订阅日期（`saveDate`）、切换账号指纹（`quotaPlanScope` 变化或 `scope_mismatch`）、以及上次读取失败（`catch` 中保持 `state.profileLoaded = false`）三种场景下重新读取 profile。
- **双向验证（`build/verify-codex-reset.js`）**：改用固定本地时钟（`2026-11-14 15:30:00`），覆盖推进 1 分钟不重读 profile、建议只算 1 次、刷新期间 DOM 不出现 `.cr-loading` /「正在计算」、保存/清空订阅日期重读 profile、切换账号指纹重读 profile、读取失败后下次刷新重试，以及 `soon` 跨本地 0 点自动复位为 `patient` 重算；新增反向哨兵 `CODEX_ADVICE_FLICKER_SENTINEL=1`（恢复刷新时置空 `state.advice = null`）按预期触发闪烁断言变红。
- **第二步耗时采样**：在本机真实 `tokenscope.db` 副本上对 `Snapshot::stored_scope` + `local_today_end` + `codex_reset::advise`（含 3 张重置卡与事件模拟）连续采样 20 次，实测微秒耗时为 `[1914, 169, 107, 85, 70, 65, 63, 66, 82, 68, 64, 61, 61, 75, 67, 74, 99, 78, 65, 63]` µs（首次冷读 1.91 ms，稳态 61–169 µs，20 次均值 169.8 µs ≈ 0.17 ms），远低于设计文档 §3 B2 的 2 ms 阈值，按方案保留每分钟重算。

## 2026-11-22 T-B1 Google Cloud 后台任务精确睡眠

- 将 Google Cloud 后台 loop 的调度决策抽为 `app/core/src/google_cloud.rs` 的纯函数 `gc_tick(now, hidden, pending, window_open, interactive, started, updated, interval, has_account) -> GcTick { action, next_check_in }`：
  - 有 `pending` 会话时保持 5 秒轮询（监测用户关闭窗口与 90 秒超时）；
  - 隐藏或未选结算账号时睡 `CAP`（60 秒兜底）；
  - 已选结算账号且无 pending 时按 `clamp(updated.max(started) + interval.max(60) - now, 1s, 60s)` 精确睡眠；触发 `Open` 时立即切入 5 秒轮询。
- `app/src-tauri/src/google_cloud_web.rs` 按 `gc_tick` 执行动作后通过 `tokio::select!` 同时等待 `sleep(tick.next_check_in)` 与 `GC_WAKE.notified()`；在 `set_config`（`lib.rs`）、登录/手动刷新/断开连接/手填到期日、会话开始与完成/失败处调用 `wake()`（`notify_one()`）。
- 双向验证：`google_cloud::tests::gc_tick_table_driven_branches_and_boundaries` 覆盖 5 种动作与边界（恰好到期、`started > updated`、`interval < 60` 抬升、无账号、交互会话不超时）；`gc_tick_matches_legacy_5s_loop_on_600s_grid_and_rejects_bad_pending_cap` 在 0–600 s 网格上对比旧 5s 轮询与精确睡眠，断言动作序列一致且时刻差 ≤ 5s，同时注入把 `pending` 分支改为 `CAP`（60s）的反向样本并断言等价性失败。`docs/report/acceptance-backlog.md` 新增 C6 三项原生验收。

## 2026-11-22 T-A2a `app.js` 拆分阶段 1a：测试脚本读法改造与分组预算

- 按 `docs/research/appjs-split-plan-2026-11-22.md` §5 阶段 1a，在不改动 `app/src/app.js` 单文件的前提下统一测试与门禁对 `app*.js` 的读取方式：
  - 新增零依赖 `build/lib/app-sources.js`（`appFileNames`、`appFiles`、`appSource`、`replaceOnce`、`loadInto`）与 `build/lib/app_sources.py`（`app_file_names`、`app_files`、`app_source`、`strip_app_script_tags`），按 `index.html` 中 `<script>` 出现顺序读取；`replaceOnce` 要求标记恰好唯一命中 1 处，否则抛错；`loadInto` 逐文件执行 `new vm.Script(code, { filename }).runInContext(...)`。
  - 迁移 9 个直接读 `app.js` 源码文本的脚本：`jsdom-smoke.js`（哨兵替换全改 `replaceOnce`，各模块逐文件 `loadInto` 加载，并修复哨兵子进程串扰 `CHECKIN_SENTINEL` 的问题）、`probe-frontend.js`、`verify-codex-reset.js`、`verify-google-cloud.js`、`verify-tray.js`、`verify-refresh-stability.js`、`verify-vault.js`、`test-sync-persistence.py`、`verify-thumbnail-layout.py`。
  - `.github/workflows/ci.yml` 与 `release.yml` 的 `node --check app/src/app.js` 改为对 `app/src/app*.js` 的循环检查；`build/check-file-size.js` 新增 `GROUP_BUDGETS`（`app/src/app*.js` 合计 ≤ 9,014 行）及「分组预算合计超标被判红」自检样本。
- 验证：静态门禁、`replaceOnce` 缺失/重复抛错自检、`jsdom-smoke.js`（6 个哨兵单独变红 + 294/294 PASS）、`probe-frontend.js`、`verify-vault.js`、`verify-codex-reset.js`、`verify-google-cloud.js`、`verify-antigravity-capture.js`、`verify-gateway.js`、无头 Edge `verify-refresh-stability.js` 及 `scan-secrets.ps1` 全部通过。

## 2026-11-22 T-A2b `app.js` 拆分阶段 1b：按原顺序切分为 14 个 `app-*.js` 脚本与完整性门禁

- 按 `docs/research/appjs-split-plan-2026-11-22.md` §3.1 与 §5 阶段 1b，按 13 个唯一锚点将 `app/src/app.js`（9,014 行）原样切分为 14 个普通脚本并按原顺序写入 `app/src/index.html`：
  - `app-core.js`（658 行）、`app-rows.js`（964 行）、`app-view.js`（904 行）、`app-tray.js`（521 行）、`app-channel-cards.js`（579 行）、`app-window.js`（510 行）、`app-manage.js`（475 行）、`app-sync.js`（703 行）、`app-settings.js`（1,058 行）、`app-update.js`（596 行）、`app-vault.js`（141 行）、`app-checkin.js`（866 行）、`app-models.js`（717 行）、`app-boot.js`（322 行）；
  - 删除 `app/src/app.js` 及 `build/check-file-size.js` 中的单文件条目，由 `GROUP_BUDGETS`（`app/src/app*.js` 合计 ≤ 9,014 行）与单文件 1,500 行软上限共同约束；
  - 逐字节校验：14 个文件按序拼接后的 SHA-256 与原 `app.js` 完全一致（`b837feedc66ba1003e7ce6a690f7e7594963197ad0c0eba5c5dcbfa3f7a1fbb5`）。
- 新增门禁 `build/check-app-split.js`（带 `--self-test`）并接入 `ci.yml`、`release.yml` 与 `AGENTS.md` §4.1：校验 `index.html` 标签清单与磁盘 `app-*.js` 一致且首尾为 `app-core.js` / `app-boot.js`、单文件独立编译、436 个顶层声明跨文件无重名、逐文件加载期执行零异常；`--self-test` 覆盖少标签、多孤儿文件、`app-boot.js` 不在最后、函数截断、跨文件顶层重名、加载期前向调用 6 类坏样本全部判红。
- 同步更新 `app/README.md` 目录树并在 `docs/report/acceptance-backlog.md` 新增 B10 原生窗口验收项（后续由 T-D1 统一实跑）。
- 附加提交认领拆分后首次被 `check-silent-failures.js` 扫到的 4 处既有降级空 catch（`app-channel-cards.js:355,451`、`app-sync.js:543,549`；原单文件 `app.js` 中因 L80 `/[&<>"']/g` 正则字面量反转了 `maskSource` 的引号状态直至 L5210 `"https://..."` 而长期处于扫描盲区，纯搬家提交 `cb46e50` 先保持 14 文件拼接 SHA-256 与原 `app.js` 100% 一致，再单独提交补齐 4 处 `silent:` 认领注释）。
- 验证：静态门禁（IPC、`check-silent-failures.js --self-test` 及正式检查 25 个前端脚本 121 处 catch、`check-file-size.js`、`check-css-forms.js`、`check-encoding.js`、`check-app-split.js --self-test` 及正式检查、`verify-expiry.js`、`verify-qoder-expiry.js --self-test`、`verify-quota-plan.js`、26 个 JS `node --check`）、`jsdom-smoke.js`（294/294）、`verify-vault.js`、`verify-codex-reset.js`、`verify-google-cloud.js`、`verify-antigravity-capture.js`、`verify-gateway.js`、`probe-frontend.js`、无头 Edge `verify-refresh-stability.js`、两 crate `cargo check --all-targets --locked`、`scan-secrets.ps1` 全部通过。

## 2026-11-22 T-C1 `app/core` 互斥锁中毒恢复与 `check-lock-unwrap.js` 棘轮门禁

- 新增 `app/core/src/lockx.rs`（在 `app/core/src/lib.rs` 注册 `pub(crate) mod lockx;`）：提供 `lockx::lock(&Mutex<T>)`，遇到锁中毒时调用 `into_inner()` 恢复内部状态，并以 `("lock-poisoned", std::any::type_name::<T>())` 调用 `diaglog::write_keyed` 按锁类型节流写入一条诊断日志；对 `Store`（`rusqlite::Connection`），`rusqlite::Transaction` 在 drop 时默认回滚（`ROLLBACK`），持锁线程在事务中途 panic 后恢复出来的连接不会停在半截事务里。
- 将 `app/core/src/fetch.rs` 生产代码中的 43 处 `.lock().unwrap()`（含 `recovery` 15 处、`store` 7 处、`progress` 6 处、`cache` 5 处、`scopes` 4 处、`progress_sink` 2 处、`qoder_packs` 2 处、`last_run` 1 处、`qoder_memos` 1 处）及 `next_retry_at`/`set_web_credential_source`/`invalidate`/`cache_updates` 的锁调用逐处替换为 `crate::lockx::lock(...)`；复核其余 4 处 `unwrap`/`expect`（`Fetcher::new` 常量 HTTP 客户端构建、`result.valid` 分支下的 `chosen`/`shown_cred`、`matches!(def.id, "qoder" | "qoder-cn")` 下的 `qoder_site`）并补齐不变量注释。
- 新增棘轮门禁 `build/check-lock-unwrap.js`（带 `--self-test`）并接入 `ci.yml`、`release.yml` 与 `AGENTS.md` §4.1：剥离 Rust 注释/字符串及单个 `#[cfg(test)]` 修饰项后扫描 `app/core/src` 与 `app/src-tauri/src` 全部生产源码中的 `.lock()/.read()/.write()` 后接 `.unwrap()/.expect()`，要求为 0（79 个生产源码文件 0 处违规，5 个自检样本含 `#[cfg(test)]` 辅助函数之后的生产代码违规全部按预期判红/判绿）。
- 在 `app/core/src/fetch_tests.rs` 新增 `poisoned_recovery_progress_and_cache_locks_recover_and_log_throttled_diag`：在子线程持锁 panic 使 `recovery`、`progress`、`cache` 三把锁中毒后，验证 `failed`、`fetch_retries`、`publish`、`cached` 不 panic 且状态正常推进，同时断言诊断日志环形缓冲恰好按锁类型各记 1 条 `lock-poisoned` 节流记录。
- 验证：静态门禁、`app/core` 单线程全量测试（652 passed / 10 ignored + `sync_fixtures` 6 passed）、两 crate `cargo check --all-targets --locked`、`scan-secrets.ps1` 全部通过。

## 2026-11-22 T-A5 `app/core/src/inject.rs` 生产代码按客户端拆分子模块

- 按设计文档 §2 A5，将 `app/core/src/inject.rs`（5,055 行）中的 5 个独立区段通过 `#[path]` 子模块与 `pub use` 重新导出外移（对外 `crate::inject::*` 路径零变化，`src-tauri` 无需任何改动）：
  - `inject_io.rs`（626 行）：JSON 文件读写、Windows DPAPI 加密备份快照与回滚、`commit` / `commit_layered`；
  - `inject_workbuddy.rs`（300 行）：WorkBuddy / WorkBuddy AI 写入、校验与前缀学习；
  - `inject_zcode.rs`（247 行）：ZCode 写入与 UUID 生成；
  - `inject_trae.rs`（342 行）：Trae（SQLite `state.vscdb`）单事务写入与模型 ID 解析；
  - `inject_dsh.rs`（1,298 行）：DeepSeek Harness 目录路由、YAML/JSON 补丁写入与移除；
  - `inject.rs` 降至 2,276 行（同步将 `build/check-file-size.js` 中 `inject.rs` 的预算从 5,055 下调至 2,276；全部 5 个新子模块均 ≤ 1,500 行软上限）。
- 纯搬家等价性校验：剥离新增的 `use super::*;`、子模块声明及 `pub(super)` 可见性修饰后，6 个文件还原出的 5,055 行与拆分前 `inject.rs` 逐行 100% 一致；核对 `README.md`「已知边界」中改写其它客户端配置与 DPAPI 备份快照的披露完整准确，无需更新。
- 验证：静态门禁、`app/core` `inject::` 全部 60 个单测（58 passed / 2 ignored）、两 crate `cargo check --all-targets --locked`、`jsdom-smoke.js`（294/294 含模型页用例）、`scan-secrets.ps1` 全部通过。

## 2026-11-22 T-A4 `app/core/src/fetch.rs` 生产代码拆分子模块

- 按设计文档 §2 A4，将 `app/core/src/fetch.rs`（3,703 行）中的凭据分段、单次请求与视图组装拆入 3 个 `#[path]` 子模块并通过 `pub use` / `pub(crate) use` 重新导出（对外 `crate::fetch::*` 路径零变化，`src-tauri` 无需任何改动）：
  - `fetch_creds.rs`（373 行）：密钥与账号指纹（`current_key_fp`、`key_fp`、`credential_scope`）、Qoder 凭据换号记录、`demote_rejected` 降权、`read_desktop_tokens_region`、`credentials` 与认证指引文案；
  - `fetch_once.rs`（789 行）：`FailKind` 与重试判定、`fetch_once` / `fetch_once_inner`、L2 诊断日志与响应截断、`fetch_channel`、`fetch_codex_reset_credits`、自定义渠道并发抓取 `fetch_custom_all_observed`；
  - `fetch_view.rs`（1,138 行）：周期学习与套餐月费折算、`claim_state`、本地日历边界、批量窗口统计 `AmountAnchors` / `AmountStats`、`build_view`、缺口判定、`placeholder` 与自定义渠道视图组装；
  - `fetch.rs` 降至 1,438 行（4 个文件全部 ≤ 1,500 行软上限；按 §2 A6 规则将 `app/core/src/fetch.rs` 从 `build/check-file-size.js` 的超限预算表移除，改由 1,500 行软上限直接约束）。
- 纯搬家等价性校验：剥离新增的 `use super::*;`、子模块声明及 `pub(super)` 可见性修饰后，4 个文件按原区段顺序还原出的 3,704 行与拆分前 `fetch.rs` 逐行 100% 一致。
- 验证：静态门禁、`app/core` `fetch::` 全部 54 个单测（54 passed / 0 failed）、两 crate `cargo check --all-targets --locked`、`scan-secrets.ps1` 全部通过。

## 2026-11-22 T-A3 `app/src-tauri/src/lib.rs` 拆分为 13 个平铺模块

- 按设计文档 §2 A3，将 `app/src-tauri/src/lib.rs`（6,272 行）按职责切分为平铺于 `app/src-tauri/src/` 下的 13 个同层模块（保证 `build/check-ipc-contract.js` 单层目录扫描完整覆盖），并在 `tauri::generate_handler!` 中改为模块前缀引用（对外前端 IPC 命令名零变化）：
  - `config_cmds.rs`（330 行）：`get_channels`、`home_view`、`get_config`、`set_config`、`key_hint`、`set_key`、`delete_key` 及热键/凭据清理辅助；
  - `key_ref_cmds.rs`（136 行）：`key_source`、`set_key_ref`、`clear_key_ref`、`vault_ref_map`；
  - `weblogin_cmds.rs`（640 行）：WorkBuddy / Qoder / 豆包网页登录窗口与登录观察器；
  - `window_cmds.rs`（340 行）：`channel_changes`、`mod autostart`、托盘图标/提示、`window_cmd`、`set_pin`、`save_window_pos`、`clamp_to_monitors`；
  - `notify.rs`（217 行）：跌破红线系统通知 `maybe_notify_crit`；
  - `diag_cmds.rs`（357 行）：渠道体检 `probe_channel`、`run_diagnostic` 与系统代理探测；
  - `custom_cmds.rs`（145 行）：`custom_test`、`custom_add`、`custom_remove`；
  - `poll.rs`（161 行）：`poll_loop`、`refresh_if_stale`、`quit_when_idle`、`refresh_now`；
  - `update_cmds.rs`（241 行）：`check_update`、`install_update`、`skip_update_version`、`app_version`、`take_startup_error`；
  - `vault_cmds.rs`（332 行）：本地密钥箱 5 个命令与引用索引重建；
  - `checkin_cmds.rs`（835 行）：签到状态/历史/立即领取命令、`checkin_credentials`、`checkin_state_of`、`run_checkin`、`credit_grants`；
  - `checkin_activity.rs`（756 行）：Qoder 与标准渠道活动记录、`monitor_*`、`record_observed_already`、`settle_arrivals`、`record_checkin_skip`、`spawn_checkin_ticker` / `checkin_tick_once`；
  - `model_cmds.rs`（1,121 行）：4SAPI 模型目录、客户端一键写入/移除/回滚、推广源与诊断日志命令；
  - `lib.rs` 降至 727 行（保留 `AppState`、`home_view_of`、`emit_channels`、`save_config_off_main`、`lock_rw`、`note`、`run()` 及内联单测；全部 14 个文件均 ≤ 1,500 行软上限，按 §2 A6 规则将 `app/src-tauri/src/lib.rs` 从 `build/check-file-size.js` 的超限预算表移除）。
- 纯搬家等价性校验：剥离新增的 `mod`/`use`、`pub(crate)` 可见性修饰与 `generate_handler!` 模块前缀后，`lib.rs` + 13 个新模块的非空行多重集合与原 6,272 行 `lib.rs` 100% 一致（0 差异）。
- 验证：`check-ipc-contract.js`（前端 133 处 invoke / 104 个唯一命令；Rust 104 个定义 / 104 个注册项）、`check-file-size.js`、`check-lock-unwrap.js`（100 个生产源码文件 0 处违规）、`check-encoding.js`、`app/src-tauri` `cargo check --all-targets --locked`（0 warning）、`scan-secrets.ps1` 全部通过。

## 2026-11-22 T-B3 渠道行签名去除墙钟文字（原地更新时间文案）

- `app/src/app-core.js` 与 `app/src/app-rows.js`：
  - 将 `renderRow` 中的快照年龄（`snapAgeSpan` → `<span class="tdyn" data-tdyn="snap" data-at="..."></span>`）、峰谷倒计时（`<span class="tdyn" data-tdyn="peak" data-peakcd='...'></span>`）与到期相对时间（`expiryRelativeSpan` → `<span class="tdyn" data-tdyn="exp" data-exp-at="..."></span>`）统一改为仅含绝对时间戳/载荷的空占位节点；同步把 `.r2[role="note"]` 的 `aria-label` 与 `.rt` / `.end` 的 `title` 移出 HTML 模板，交由 `tickPeakCountdowns(root = document)` 原地同步（仅在值变化时写 DOM）；
  - `setTextIfChanged` 在目标已有单个 `Text` 子节点时直接改 `firstChild.nodeValue`，确保分钟级更新只触发 `characterData` 而非 `childList` 重建；`renderDetail` 末尾通过 `queueMicrotask(() => tickPeakCountdowns())` 保证弹层打开时相对到期文案即时填充；
  - `build/check-file-size.js` 将 `app/src/app*.js` 分组上限从 `9,014` 下调至 `9,011`。
- 双向验证：
  - `build/verify-refresh-stability.js` 新增 `clockTick` 场景与 `--assert` 判定：固定时钟 `2026-11-14 15:30` 下，数据不变仅推进 1 分钟，`#list` 上 `MutationObserver` 测得 `childListCount === 0` 且 `characterDataCount > 0`，快照年龄从「12 分钟前快照」更新为「13 分钟前快照」，峰谷与到期倒计时同步原地更新；
  - 反向哨兵 `ROW_SIG_SENTINEL=1`（将 `snapAge` 写回占位属性使 `QSIG` 失配）实测触发 `FAIL 推进 1 分钟 #list 无 childList 节点更换 (childListCount=2)` 并以退出码 1 拦截。
- 验证：静态门禁（IPC、`silent-failures`、`file-size`、`css-forms`、`encoding`、`app-split`、`node --check`）、`jsdom-smoke.js`（294/294）、`verify-vault.js`、`verify-codex-reset.js`、`verify-google-cloud.js`、`verify-gateway.js`、无头 Edge `verify-refresh-stability.js --assert`、`scan-secrets.ps1` 全部通过。

## 2026-11-22 T-B4 `fitPill` 按文本/字体/宽度缓存测量结果

- **改动**：`app/src/app-tray.js` 为 `fitPill()` 增加 `PILL_MEASURE_KEY` / `PILL_MEASURE_W` 缓存（键为 `文本|字号档|fontFamily|clientWidth`），命中时不创建离屏 `<span>`、不读 `offsetWidth`；将字体样式复制收敛为循环，保持 `app*.js` 总行数仍在 `9011/9011` 预算内。
- **验证补强**：`build/jsdom-smoke.js` 新增 3 项 `fitPill` 测量缓存断言及 `FIT_PILL_SENTINEL=1` 双向哨兵。
- **验证**：`check-file-size.js`、`check-silent-failures.js`、`check-app-split.js`、`check-encoding.js`（均含 `--self-test`）、`check-ipc-contract.js`、`check-css-forms.js`、`node --check app/src/app-tray.js`、`jsdom-smoke.js`（297/297）、`FIT_PILL_SENTINEL=1`（3 项 `fitPill` FAIL，退出码 1）、`verify-vault.js`、`verify-codex-reset.js`、`verify-google-cloud.js`、`verify-gateway.js`、`scan-secrets.ps1` 全部通过。


## 2026-11-22 T-B5 SQLite 单轮逐条提交 vs 单事务实测结论（放弃）

- **实测数据**（临时磁盘库 `Store::open`、WAL + `synchronous=NORMAL`，连续 20 轮 × 28 次写入/轮 ≈ 12 渠道全量 `record_fp` + `quota_snapshot_v1` `set_setting` + 2 渠道 `insert_grant`/`prune_grants`）：
  - 逐条自动提交：总计 `43.360 ms`，均值 **`2.168 ms/轮`**；
  - 单事务 `unchecked_transaction()`：总计 `10.319 ms`，均值 **`0.516 ms/轮`**；
  - 差值仅 **`1.652 ms/轮`**（实测耗时可忽略，远低于引入事务批处理的风险阈值；临时基准测试已删除）。
- **结论**：按既定决策**放弃**（不引入跨渠道写合并与借用生命周期改造，保留逐条立即落盘）。
- **验证**：`check-encoding.js`、`cargo test --locked -- --test-threads=1`、`scan-secrets.ps1` 通过。


## 2026-11-23 T-B6a WAL checkpoint 与完整性校验入口

- **改动**：
  - `app/core/src/store.rs`：抽取 `row_to_checkin`、`row_to_vault` 消除行映射重复（`store.rs` 降至 `1949/1952` 行）；新增 `Store::wal_checkpoint()`（`PRAGMA wal_checkpoint(TRUNCATE)`）与 `Store::integrity_check()`（`PRAGMA integrity_check`）。
  - `app/src-tauri/src/poll.rs`：每日定期 `s.prune(...)` 后及 `quit_when_idle` 退出前调用 `wal_checkpoint()` 将 `-wal` 刷回主库并截断。
  - `app/core/src/store_tests.rs`：新增 `wal_checkpoint_truncates_wal_and_passes_integrity_check` 验证磁盘库写入后 `-wal` 非空、checkpoint 后归零且 `integrity_check() == "ok"`。
- **验证**：`check-file-size.js`、`check-lock-unwrap.js`、`check-encoding.js`（均含 `--self-test`）、`app/core` `cargo test --locked -- --test-threads=1`（653 passed, 10 ignored; `sync_fixtures` 6 passed）+ `cargo check --all-targets --locked`、`app/src-tauri` `cargo check --all-targets --locked`（零警告）、`scan-secrets.ps1` 全部通过。


## 2026-11-23 T-B6b `%TEMP%` 副本数据库体积与碎片率只读实测

- **实测结果**（详见 [`docs/report/db-growth-measurement-2026-11-22.md`](docs/report/db-growth-measurement-2026-11-22.md)，全程仅读取 `%TEMP%` 副本，未停应用且未改写原库）：
  - 原文件体积：`tokenscope.db` `80.84 MB` + `-wal` `3.98 MB` + `-shm` `0.03 MB` = `84.85 MB`，副本 `PRAGMA integrity_check` 为 `ok`；
  - `page_size=4096`、`page_count=20,702`、`freelist_count=8`，**空闲页碎片率仅 `0.04%`（`32 KB`）**（库龄 68 天，尚未触发 90 天 `prune`）；副本 `VACUUM` 可收紧 `12.33 MB`（`15.25%`，属时序 B-tree 页内空洞）；
  - `snapshots` 表共 `475,144` 行（`25.11 MB`）加三张索引 `55.47 MB` 合占主库 **`99.6%`**，近 10 天均写入 `~15,476` 行/天（约 `+2.6 MB/天`）。
- **结论**：按 `< 10%` 阈值裁决**不做自动 `VACUUM`**，`T-B6a` 的 WAL checkpoint 已足够；临时测试与 `%TEMP%` 副本已删净。
- **验证**：`check-encoding.js`、`scan-secrets.ps1`、`git diff app/core/src/store_tests.rs` 为空，全部通过。


## 2026-11-23 T-D1 原生窗口 CDP 一键回归脚本

- **改动**：
  - 新增纯 ASCII 脚本 `build/run-native-checks.ps1`：启动前校验若 `Grandettoken` / `tokenscope` 已在运行则直接拒绝（不杀用户实例）；将 `%APPDATA%\com.wacil.tokenscope` 下 `*.db*` / `*.json` 按 SHA-256 备份到临时目录并在 `finally` 等待进程退出后清删多余文件并按 SHA-256 逐字节还原；拉起 `launch-debug.ps1` 并等待 CDP 端口就绪后串行执行 7 项 CDP 验证（`verify-settings-entry.js`、`verify-checkin-center.js`、`verify-resize.js`、`verify-qoder-expiry.js`、`verify-ui-0923.js`、`verify-focus-ring.js`、`verify-form.js`），日志与汇总表写入 `build/qa/native-<YYYY-MM-DD>/`，任一失败返回退出码 1；内置 `-SelfTest` 离线自检。
  - 更新 `RELEASE.md`（桌面发版第 2 步提醒与第 6 步一键原生窗口回归说明）及 `docs/report/acceptance-backlog.md`（标注 `B1`、`B9`、`B10` 已纳入一键脚本，仍待用户关闭运行实例后实跑）。
- **验证**：纯 ASCII 检查（16,593 字节中非 ASCII 为 0）、`build/run-native-checks.ps1 -SelfTest`（4 项自检全绿）、`check-encoding.js`（含 `--self-test`，469 文件 PASS）、`check-silent-failures.js`（25 文件/121 catch PASS）、`scan-secrets.ps1`（562 文件 CLEAN）。
- **未跑项**：本机有运行中的 `Grandettoken` 实例且按规则不杀用户进程，真实原生窗口 CDP 全套回归（`B1`–`B10`）待用户在关闭应用后运行 `powershell -NoProfile -ExecutionPolicy Bypass -File build/run-native-checks.ps1`。


## 2026-11-23 v0.3.71 发版前代码审查与实际程序测试

- **范围**:`v0.3.70..HEAD` 共 22 个提交(T-00、T-A1、T-A2a/b、T-A3、T-A4、T-A5、T-B1–B4、T-B5 测量、T-B6a/b、T-C1、T-C2、T-D1–D3)。
- **搬家等价性**(一次性脚本,放系统临时目录,未入库):
  - T-A2b:14 个 `app-*.js` 按 `index.html` 顺序拼接后与原 `app.js` 逐字节相同;
  - T-A1 / T-A5:改动文件的行多重集合(去缩进、忽略 `use`/`mod`/`#[path]`/可见性)零差异;
  - T-A4:只多出 `pub use` 重导出清单;T-A3:只差 `generate_handler!` 模块前缀与两个元组字段的 `pub(crate)`;
  - `#[test]` 计数 core 从 658 到 663(新增 5 个均为本轮新测试),src-tauri 的测试数加命令数不变。
- **行为改动审查**(无阻断问题):
  - T-C1 `lockx::lock`:diaglog 内部全用 `if let Ok` / `unwrap_or_else`,记日志路径不会二次 panic;
  - T-C2:Job Object 杀进程树,读线程改为带期限的 channel;正常退出时释放 Job 也会杀掉残留孙进程,对 `--version` 探测可接受;
  - T-B1:pending 时保持 5 s,空闲按到期时刻睡眠、上限 60 s;`notify_one` 留许可,不丢唤醒;两处 `continue` 都在状态已变之后,不会空转;
  - T-B2:刷新保留旧结果;profile 在保存、切账号、读失败、`scope_mismatch` 后重读;
  - T-B3:隐藏时由 `window-shown` 补 render + tick;`renderDetail` 末尾的 microtask tick 覆盖 `app-manage.js` 直接写 innerHTML 的路径;新建占位节点 `before` 为空,不会触发 `expCrossed → render` 重入;
  - T-B4:缓存键含字号与 body 类名,溢出兜底保留;T-B6a:checkpoint 每日一次加退出前一次;`integrity_check` 暂无生产调用方(只在单测里用,按 T-B6b 结论保留为手动排查入口)。
- **门禁**(按 AGENTS.md §4.1 串行跑,两轮):全部静态门含 `--self-test`、`node --check`、jsdom 冒烟 297/297、vault / codex-reset / google-cloud / antigravity-capture / gateway 冒烟、core `cargo test --locked`(653 passed / 10 ignored;fixtures 6 passed)+ `cargo check --all-targets --locked`、src-tauri `cargo check --all-targets --locked`(零警告)、`scan-secrets.ps1` CLEAN、`verify-signature.js --self-test` PASS、`run-native-checks.ps1 -SelfTest` PASS。
- **原生 WebView2 回归**(经用户同意关闭运行中实例;`build/build.ps1 build` 重新编出 release,`run-native-checks.ps1 -Port 9241 -Exe <新 exe>`;用户数据 3 个文件每轮都按 SHA-256 备份、恢复并校验):通过 `verify-settings-entry`、`verify-checkin-center`、`verify-resize`、`verify-qoder-expiry`、`verify-focus-ring`,日志零加载期 JS 报错。`verify-ui-0923`、`verify-form` 失败;用本机旧二进制(约 v0.3.36)单独复跑,U2 文案、U6 紧凑条字母、compact 尺寸三项同样失败,判定为脚本断言过时、不是本版回归,已记 `acceptance-backlog.md` B11。
- **一键脚本问题**(记 B12):默认端口 9222 被本机 Chrome 占用;`launch-debug.ps1` 不构建、只挑最新的 exe(首次跑到 11-20 的旧二进制);`-Filter` 不支持 `|`,全部 SKIP 仍退出 0;子脚本日志 GBK 往返后乱码。
- **发版**:三处版本号同步到 0.3.71,`Cargo.lock` 只变本包版本行,`cargo fetch --locked` 复核通过。只发桌面,Android 本轮零改动不打 tag。
- **未跑**:安装版自动更新(B7)、真实账号相关项(C/D 区)、紧凑条与模型页 / 同步页 / 密钥箱的手动打开(B10 剩余部分)。


## 2026-11-23 v0.3.72 AI 优惠改读官网公开列表

- **背景**:上游 `https://www.aipromonow.com/rss.xml` 已返回 HTTP 410,优惠页取不到数据;旧版把请求失败与解析失败一律显示「更新失败」,看不出原因。
- **改动**:
  - `app/src-tauri/src/model_cmds.rs`:`fetch_promo_feed` → `fetch_promo_page`,改取官网 `/deals`;不跟随重定向,非 2xx 报具体 HTTP 状态,分块读取按 2 MB 上限判定过大,UTF-8 非法单独报错;`PROMO_HTTP` / `promo_client` 收窄为模块私有(全仓确认零外部引用)。
  - `app/src/promo-page.js`(新增):用惰性 `template` 解析官网 JSON-LD `ItemList`,校验来源域名、`/promos/` 路径、条数与 `numberOfItems` 一致、条目去重后输出 `{id,link,title,description}`;摘要取卡片 `#promo-<slug>` 首个 `<p>` 的纯文本。
  - `app/src/app-boot.js`:已读状态与历史发布日期按详情链接迁移(换 id 口径不整页变「新」);失败提示改为具体原因(HTTP 状态 / 列表格式异常),失败时保留上次列表与更新时间;新列表无发布日期,不再显示「时间未知」占位。
  - `build/check-file-size.js`:`app/src/app*.js` 分组预算 9011 → 9007(棘轮只下调)。
  - 文档:`AGENTS.md` §8.10、`docs/report/promo-rss-retired-2026-11-23.md`、`acceptance-backlog.md` 新增 B13/B14。
- **验证**(按 AGENTS.md §4.1 串行):静态门 16 项(带 `--self-test` 的先证红)全 PASS;`node --check` 覆盖 `app/src/*.js` 与 `google_cloud_capture.js`;jsdom 冒烟 **303/303**(新增 6 项优惠用例),vault / codex-reset / google-cloud / antigravity-capture / gateway 冒烟 PASS;`app/core` `cargo test --locked -- --test-threads=1` **653 passed / 0 failed / 10 ignored**、`sync_fixtures` 6 passed;core 与 src-tauri `cargo check --all-targets --locked` 零警告(src-tauri 实际重编,非缓存);`scan-secrets.ps1` CLEAN。
- **双向验证**:临时删掉解析器的「条数 ≠ `numberOfItems`」校验后,jsdom 冒烟退出 1 并精确命中 `FAIL 优惠刷新保留缓存及失败提示：列表被截断`(外部链接项为级联失败);按 SHA-256 还原原文件后复跑 303/303。
- **实网复核**:`/deals` 直连与经本地代理均直接返回 200(无跳转),`rss.xml` 仍 410;用应用同款解析器读实际页面得 24 条、全部带摘要、链接全部落在 `open_promo_url` 的官网 `/promos/` 白名单内,首条详情 200。官网列表分页(共 411 条 18 页),`numberOfItems` 等于当页条数,首页按发布时间倒序 —— 只取首页等于只展示最近几天新增,取舍已记 B14。
- **未跑**:安装版真机优惠页(B13/B14)。本轮无窗口几何 / WebView2 改动,未跑 `run-native-checks.ps1`(本机有运行中的实例,按规则不杀用户进程)。


## 2026-11-23 对标 Magpie 的本地网关升级与现代编码 CLI 接入

- **设计稿**: [`docs/design/design-gateway-magpie-upgrade-2026-11-23.md`](docs/design/design-gateway-magpie-upgrade-2026-11-23.md)。
- **改动**:
  - **F1 虚拟路由组与预首字切备**（`app/core/src/gateway/routing.rs`、`dispatch.rs`）：新增 `group/<id>` 命名空间与 `order` / `rotate` / `expiring`（7 天临期积分 + `peak::info_at` DeepSeek 谷时优先）/ `pace` 策略；主窗口 `emit_channels` 同步推送只读 `ChannelSignal`；首字节前遇 429/5xx/超时按 `Retry-After` `[5, 600]` 秒自动冷却并切换下一候选（4xx 请求错误不切备）；保留 `alias == "group"` 关键字校验。
  - **F2 Sticky 会话与 Prompt Cache 解析**（`app/core/src/gateway/routing.rs`、`app/core/src/gateway_proto/stream.rs`）：支持显式会话头与系统提示+首轮消息 SHA-256 摘要映射（纯内存，不落明文），修复 Sticky 命中与 `Rotate` 策略混用时消费游标的缺陷；`UsageDetails` 与 `StreamStats` 提取 `cached_prompt_tokens`、`cache_creation_tokens`、`reasoning_tokens`。
  - **F3 SQLite 持久化网关台账**（`app/core/src/store.rs` `WANT=7`、`store_gateway.rs`）：新增只记元数据的 `gateway_usage_log` 表（90 天自动清理），支持 1/7/30 天统计（命中率、TTFT P50/P95、4SAPI 目录单价费用估算、按渠道/模型分组、一键清空）。
  - **F4 Anthropic `/v1/messages` + `/v1/messages/count_tokens` 入口与可选 Sticky Prompt 脱敏**（`app/core/src/gateway_proto/anthropic.rs`、`gateway/http/anthropic.rs`、`gateway_proto/masking.rs`）：支持 Anthropic ↔ OpenAI 双向 JSON / SSE 与工具调用转换及 `x-api-key` 校验；脱敏默认关闭（`enabled = false`），开启时驻留内存并配 `SseFrameUnmasker` 处理跨分片 `[MASKED_` / `KEY_01]` 还原。
  - **F5 「通过本地网关接入」与现代编码 CLI 配置**（`app/core/src/inject_cli.rs`、`inject_gateway.rs`、`inject.rs`、`inject_trae.rs`、`inject_io.rs`、`inject_workbuddy.rs`、`inject_dsh.rs`）：扩展 `Client` 至 8 家（新增 `Claude Code`、`Codex`、`OpenCode`），遵循进程未运行检查、`backup_layered` DPAPI 加密备份（含新建文件标记，回滚时自动删掉）、保注释局部修改与读回校验；外部文件仅写入回环网关地址与网关访问令牌，上游密钥留在 Windows 凭据管理器；同步将 `CATALOG_DISPLAY` 与 Trae 删除/校验辅助函数迁出 `inject.rs` 守住文件规模棘轮（`2066/2276`）。
  - **壳层与前端界面**（`app/src-tauri/src/gateway_ext.rs`、`gateway.rs`、`lib.rs`、`app/src/gateway-advanced.js`、`gateway-view.js`、`gateway-view.css`、`app-models.js`、`index.html`、`README.md`）：新增「路由组」「用量账本」「接入与安全」三个面板及 9 个 Tauri 命令，`app-models.js` 更新 8 客户端显示名并压紧行数保 `app*.js <= 9007`。
- **验证**:
  - `app/core`: 回环集成测试 `magpie_tests` 5/5、`inject_cli` 3/3、`inject_gateway` 1/1、全套 `cargo test --locked -- --test-threads=1` 与 `cargo check --all-targets --locked` 零警告通过；`app/src-tauri` `cargo check --all-targets --locked` 零警告通过。
  - 前端与双向门禁：新增 `build/verify-gateway-magpie.js`（含 4 项坏样本哨兵自检，验证脱敏默认关、失败回滚 DOM+Toast、未预览禁止接入写入与非法 IPC 命令必变红），协同全量 §4.1 静态门、jsdom 冒烟与凭据扫描串行验证通过。
- **未跑项**: 本机无真实 Claude Code / Codex / OpenCode 在途会话与真实安装包真机 UI 巡检，待手工验收。


## 2026-11-23 v0.3.73 对标 Magpie 网关升级的实机验收与三处实机修复

- **实机方法**:`build.ps1 build` 出新 exe → `launch-debug.ps1 -Port 9241` → CDP 驱动**真实应用进程**;上游用回环本地假上游(按模型名返回 429+`Retry-After`/503/400/正常 JSON/SSE/reasoning),不耗真实额度、不动真实密钥;用户数据先 SHA-256 备份、测完还原并校验。
- **实机断言合计 134 项全过**:网关功能 68 + UI 走查 39 + UI 深查 17 + rotate/pace/删组 10。
  - 429 首字节前切备:上游先收 429 再收正常请求,下游 200,响应头 `group/<id>` 与上游别名;冷却剩余秒数等于 `Retry-After`;Sticky 同会话第二轮只打一次上游;400 原样返回且不偷切。
  - Anthropic `/v1/messages` 非流式与 SSE 状态闭环;`thinking` 预算 4096→`medium`、1024→`low`;上游吐 `reasoning_content` 时块序列 `start:thinking,stop,start:text,stop`;`count_tokens` 不打上游。
  - 脱敏默认关;开启后出站无密钥/邮箱原文、同会话占位符一致、跨 SSE 分片完整还原;分类开关单独生效;关回后原文直传。
  - 台账:本轮 11 笔逐笔入账,命中率 0.6、TTFT P50/P95、容灾与脱敏笔数齐全;未知单价 `pricedRequests=0` 且费用 `null`;表 21 列全元数据、无正文/密钥;真实 85MB 库 v6→v7 迁移后 **481,689 快照无损**、`integrity_check=ok`。
  - CLI:8 客户端;Codex/OpenCode 在跑被拒,`~/.codex/config.toml` 与 `~/.config/opencode/opencode.jsonc` **逐字节未变**;预览只含回环地址与网关令牌、`writesSecret=false`;未预览时确认写入按钮不存在。
- **实机抓出并修复 3 个缺陷**(均双向证红):
  1. OpenCode 桌面版进程名是 `opencode-cli.exe`,守卫只查 `opencode.exe` 会漏判「在跑」而边跑边写。改 `Client::processes()` 全名任一命中、徽章显示实际命中名;新增单测,撤掉修复即红(exit 101)。
  2. 用量账本面板横向越界:`.gw-tab-pane` 的隐式 auto grid 轨道被表格 nowrap 最小内容撑到 713px(容器 513px)。钉 `minmax(0,1fr)`;新增 `build/verify-gateway-layout.js`(单列/双列/缩略 × 三面板,含超宽卡片坏样本自检)并纳入 `run-native-checks.ps1`(7→8 项);未修二进制上 exit 1、修后 9/9 PASS。
  3. 优惠官网中文标题 slug 是百分号编码、卡片 id 是解码字面,ASCII 正则拒绝导致**整页刷新永久失败**(真机第 2 页第 7 条命中)。校验与查摘要改按 `decodeURIComponent(pathname)`,id/链接保留编码原样,解码后仍拒路径穿越;jsdom 309→312,撤掉修复精确红 2 项;真机刷新取前 3 页 72 条、编码条目带摘要。
- **原生 8 项回归**:6 PASS / 2 FAIL;失败项与 `acceptance-backlog.md` B11 记录的过时断言逐字相同(U2 文案、U6 官方标 4 vs 2、U6 紧凑条字母、compact 尺寸、`win-style.ps1` 按标题找窗),**非本版回归**;用户数据每轮 SHA-256 备份还原校验。
- **门禁(串行,修后复跑)**:静态门含 `--self-test` 全 PASS;jsdom **312/312**;vault / codex-reset / google-cloud / antigravity-capture / gateway / gateway-magpie 全 PASS;core `cargo test --locked -- --test-threads=1` **673 passed / 0 failed / 10 ignored** + `sync_fixtures` 6 passed;core 与 src-tauri `cargo check --all-targets --locked` 零警告(实际重编);`scan-secrets.ps1` CLEAN。
- **收尾**:测试用的渠道/路由组/台账/脱敏开关随用户库还原清除(还原后 port 18771、lan true、原 3 渠道、0 路由组);用户安装版(v0.3.71)已重新拉起;三个 CLI 配置哈希与测前一致。
- **待验收**:B15 三个 CLI 真实写入/回滚(需用户关闭对应 CLI)、B16 真实渠道鉴权链路、B17 冷却徽章自动刷新与预览加载态(待产品裁决)、B11 过时断言改写。
- **发版**:三处版本号同步 0.3.73,`Cargo.lock` 仅本包版本行,`cargo fetch --locked` 复核通过;只发桌面,Android 本轮零改动不打 tag。
