# AGENTS.md — 自动化改代码的硬约束

> **写给所有会改这个仓库的 agent / 工具。** 本文件受版本控制,是跨 agent 的公共记忆。
> `.workbuddy/`、`.workbuddy-ai/`、`.zcode/`、`.trae-html-share-packages/`、`.dsh-acl-report/` 等目录
> **都被 gitignore 且互不共享**。任何「下次别再这样」的结论必须落到本文件,再记进自己的日志。

### 写入纪律

- **按主题归位,不追加到文末或当前小节**:通用验证教训进 §6,模块陷阱进 §8(无合适小节则新建)。**章节编号不要改**(§5 已于 2026-11-22 移出,编号留空不复用),脚本注释和报告按编号引用。
- 以日期开头,只写「规则 + 一句原因」;过程与证据放 `docs/report/`,这里留结论和路径。
- 写入前先搜同主题条目,**合并改写**而非新增「补充」;不写会腐烂的数字(测试数/行数/文件数/文档行号),改用「以某命令输出为准」。
- **控长**:本文件会注入每个 agent 上下文,超过 ~24 KB 会截断末尾(曾导致 §8 被切掉)。总长保持在 **22 KB 以内**;快满时先精简旧条目再写新的。2026-11-22 精简前的全文存于 `docs/report/agents-md-archive-2026-11-22.md`,只作查细节用。

---

## 0. 动手前:先找单一事实源

| 要确认什么 | 看哪里 |
|---|---|
| 当前版本、最近改了什么 | `PROGRESS.md` **文末最新条目** + `git log`(头部状态是快照,会过时) |
| 产品口径 / 设计裁决 | `DESIGN.md`;§16 是用户裁决,不许推翻 |
| 发版流程、签名、回滚 | `RELEASE.md` |
| 模块划分、加渠道/客户端 | `app/README.md` |
| 门禁实际跑了什么 | `.github/workflows/ci.yml`、`release.yml`、`text-encoding.yml` |
| 历史体检、冗余审计、跨版本待验收 | `docs/report/`(待验收:`acceptance-backlog.md`) |

**改之前先 grep 现有实现。** 本项目多次出现「同一件事写了第二份」(旧摘要 CSS 被覆盖、`SyncStatus`/`GithubStatus` 重复、`savePosSoon`/`scheduleSettleCheck` 并存)。找不到调用方 ≠ 可新增,先用 `build/check-ipc-contract.js` 和全仓 grep 确认。

---

## 1. 2026-11-12 治理基线

全量审计见 `docs/report/code-governance-2026-11-12.md`、`docs/report/redundancy-audit-2026-11-11.md`。当时的结构性问题(产物入库、宽松桩、门禁硬编码本机路径、脚本腐烂、构建失败仍退出 0、空 catch、门禁假阳性等)已全部转化为 §2、§4、§6、§7 的硬约束。

---

## 2. 硬规则

### 2.1 验证(最重要)

1. **双向验证**:门禁既要「当前代码全绿」,也要「注入坏样本时变红」(哨兵、`--self-test`、夹具)。单向变绿说明不了任何事。
2. **期望值从源码实时提取,不写死**:命令表、测试数、版本号、行数以命令输出为准。
3. **CI 门禁优先零外部依赖**;有依赖(如 jsdom)须写明安装方式与失效条件,未跑不许写「已通过」。
4. **验证脚本随代码复核**:改了渲染/CSS/命令后确认 `build/verify-*.js` 引用的选择器和命令还在;破损脚本宁删勿留。
5. **只打印不判定的工具须显式标注**:如 `verify-refresh-stability.js --compare`,`exit 0` 不代表通过。
6. **不许静默吞错**:写操作失败按 `saveCfg()` 范式(回滚内存状态 + `showToast(...)` 用户可见);GUI 里 `console.warn`/`eprintln!` 不算已处理。
7. 提交前按 §4.1 跑完门禁;**没跑完不许写「已验证」**。

### 2.2 仓库与产物

- 抓包 JSON、APK、安装包、截图归档、分享 zip **一律不入库**。
- 一次性探针/补丁脚本放 `build/tmp/` 并在同一轮删掉;禁止提交本机绝对路径、真实凭据、解密脚本。
- 脚本禁止硬编码 `G:/...`、`C:/Users/...`,统一用 `path.resolve(__dirname, ...)` / `os.tmpdir()` / 环境变量。
- 追加型日志文档定期同步头部;与 `git log` 冲突时以 `git log` 为准。
- 2026-11-22 **文本一律按 UTF-8 读写**:有工具按 GBK 读、UTF-8 写回,PROGRESS.md 46 处汉字丢字节,首次提交即已损坏。PowerShell 5.1 读写文件须显式 `-Encoding utf8`;`build/check-encoding.js` 拦非法 UTF-8 与 U+FFFD。`ci.yml` 对纯文档提交不跑,编码门在独立的 `text-encoding.yml`。

### 2.3 改代码

- **批量字符串替换是最后手段**:须逐处列出命中和理由,改完跑全量门禁;禁止 `s.replace(...)` 盲替。
- 大文件已超标(实测看 `node build/check-file-size.js`)。**新功能优先拆新模块**;单次改动超 300 行先写拆分方案(见 §7.2)。
- 注释写「为什么」,不写修 bug 流水账;修完删临时注释。
- 删代码前确认全仓零引用(含 `build/`、`contract/`、`android/`、文档);`pub` 符号和 Tauri 命令注册是编译器看不到的引用。
- **能交给编译器强制的就不留给约定**:状态码→文案、枚举→分支用穷举 `match`(见 §8.2 `Reason`)。

### 2.4 高权限 / 敏感行为

以下能力**每一项都必须在 README 披露、可关闭、有明确触发条件**:

- 用 `ELECTRON_RUN_AS_NODE=1` 启动其它桌面应用并读其内部存储派生凭据密钥,超时用 `taskkill /T /F` 杀进程树;
- 改写其它客户端(Trae / WorkBuddy / ZCode / DSH)的配置与 SQLite,留备份快照(**2026-11-12 起 Windows DPAPI 加密**,见 `app/core/src/backup_crypto.rs`;换 Windows 账户后旧快照不可解);
- 读取并解密本机 WebView2 Cookie / 桌面端加密凭据;
- 默认开启的每日自动签到、写注册表自启动、监听局域网的本地网关。

**新增同类能力前先问用户**,禁止以「方便」为由默认打开。禁止把凭据、令牌、Cookie 写入日志/配置/导出/QA 数据 —— `app/core/src/diaglog.rs` 脱敏白名单和 `app/core/src/sync/publish.rs` 的 `crypto::seal` 是既有防线,改动先跑单测。

---

## 3. 「恶意代码」判定口径

2026-11-12 全量扫描**未发现传统后门**(出网只到渠道自身/GitHub/汇率 API/官方 CDN;无 `eval`/`new Function`/远程脚本;CSP 只放行自身;日志脱敏;同步先加密)。§2.4 的高权限链仍需逐项知情同意:发现新的出网目标、凭据读取路径或持久化写入时先当作可疑项报告,不静默合入。

---

## 4. 提交前门禁与发版

### 4.1 门禁清单(仓库根目录,Git Bash 可直接复制)

与 `ci.yml` / `release.yml` 对齐;**不一致时以 workflow 为准并回头修本节**,新增门禁时三处一起改。

```bash
# 零依赖静态门:带 --self-test 的先证红,再正式检查
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
for f in app/src/*.js app/src-tauri/src/google_cloud_capture.js; do node --check "$f" || echo "FAIL $f"; done

# 界面冒烟:需 jsdom(npm install jsdom --no-save --no-audit --no-fund)
node build/jsdom-smoke.js
node build/verify-vault.js
node build/verify-codex-reset.js
node build/verify-google-cloud.js
node build/verify-antigravity-capture.js
node build/verify-gateway.js
node build/verify-gateway-magpie.js

# Rust(用例数看输出,不写死)
(cd app/core && cargo test --locked && cargo check --all-targets --locked)
(cd app/src-tauri && cargo check --all-targets --locked)

# 凭据扫描(非零退出必须停)
powershell -NoProfile -ExecutionPolicy Bypass -File build/scan-secrets.ps1
```

- 本机 GNU 工具链:`PATH` 含 `C:/Users/wacil/mingw64/bin` 与 `~/.cargo/bin`,工具链 `stable-x86_64-pc-windows-gnu`。缺依赖如实写「未跑」,不拿历史结果冒充。
- **串行执行,结论单独复跑确认**;并发假红、代理阻塞、`src-tauri` 本机无法跑 `cargo test` 见 §6.7。

### 4.2 发版

- **发版走 CI**(2026-11-12 起):三处版本号同步 → **先推 main** → 再推 `v*` tag。本地 `build/release-local.sh` 仅 Actions 不可用时使用;详见 `RELEASE.md`。tag 指向的 commit 不在 main 上会导致源码归档不一致。
- **发版说明**取自发版提交正文,提交必须带说明正文;`release.yml` 打包前按 `verify-release.js` 同一判据拦截。`notes intact` 读线上 `latest.json.notes`,**只改 Release 正文不改核验结果**;修正时同时更新 Release 正文和 `latest.json` 资产,并保留原始失败记录。

---

## 6. 验证不缜密的七种真实形态

> 均为本仓库真实踩过的「绿得没意义」。

### 6.1 「只测成功路径」
`saveCfg()` 的回滚与 toast 是后补的,此前写盘失败不回滚不提示。**任何写操作验完成功,须再验失败时用户看到什么。**

### 6.2 「桩的默认行为是成功」
jsdom 桩曾对未登记 invoke `return null`,命令漏注册也全绿。**mock 默认须失败/抛出**,宽松桩必须配源码级静态门(`check-ipc-contract.js`)。mock 与 IPC 门都测不出后端新增 reason code 前端漏文案(§8.2)或服务端风控变脸(§8.3)。

### 6.3 「只测存在,不测顺序 / 状态 / 时序」
加入口须问:**顺序变了 / 并发到达会不会坏?**(顶栏按钮曾只断言存在;`loadCheckin` 曾被并发旧快照覆盖。)
- 声明时立即启动的 async 块,外层依次 await 仍会并发;冒烟须等初始化后按组调用。
- 涉及时效的前端冒烟,固定日期数据必须配同一固定时钟(v0.3.45 曾因真实时钟跨 12h 边界连红)。
- 渲染收尾会刷新倒计时,在里面触发套餐重绘会重入;重排只从外层分钟定时器触发,冒烟覆盖同分钟与换日。

### 6.4 「验证脚本腐烂与前置口径不一致」
- 改 DOM/CSS 后复核所有 `build/verify-*.js`(`verify-codex-layout.js` 曾长期引用已删的类名而必抛错)。
- **同一脚本里同类前置缺失用同一口径**(`verify-release.js` 曾缺安装包 SKIP、缺 `.sig` 却 FAIL,发版后稳定假红)。

### 6.5 「门禁报错就照着改代码」
**门禁红了,先拿已知正确样本证伪门禁自己。** 2026-11-12 两次假红:IPC 门把注释与下一行粘在一起误报未注册;静默失败门算出 NaN 误报 115 处空 catch。2026-11-22 拆 `app.js` 时暴露 `check-silent-failures.js` 的 `maskSource` 未解析正则字面量(原单文件 L80 `/[&<>"']/g` 反转引号奇偶直至 L5210 `"https://..."`,漏扫中间 4 处空 catch)。

### 6.6 「门禁没有自检与哨兵串扰」
- 新门禁必须能证明自己会变红(`--self-test` / 哨兵),坏样本贴近真实失败。
- 哨兵环境变量会被已有自检子进程继承而串扰;新增哨兵须核对**具体功能的 FAIL 行**,前置控件缺失时保留失败断言、跳过依赖交互,避免 TypeError 盖住定位。

### 6.7 「在错误的机器 / 并行度 / 环境下取结论」
- `src-tauri` 的 `cargo test` 本机启动即 `STATUS_ENTRYPOINT_NOT_FOUND`(WebView2 静态加载器老问题,与业务无关);本机/CI 均用 `cargo check --all-targets`。
- 并发跑 core + src-tauri + jsdom 会资源竞争假红:**门禁须串行,结论须单独复跑仍成立**。`proxy_changes_refresh_the_client_and_direct_groups_bypass_it` 并发时可能 10035/10054,保留原始记录,`--test-threads=1` 复核后再全量确认。
- `proxy` 夹具的 accept 连接在 Windows 可能继承非阻塞:夹具须改回阻塞并设读超时。
- 存在 `HTTP_PROXY` 时回环夹具(如 `custom_channels_fetch_in_parallel_keep_order_and_skip_hidden`)会被代理阻塞:验证进程设 `NO_PROXY=127.0.0.1,localhost,::1`。
- 模块级静态缓存 / 全局计数 / 进程单例在并行测试下会被同模块用例推进:做成可注入口(如 `LedgerCache::get_at`),测试只驱动注入值。
- `spawnSync` 超时是防挂死护栏不是断言;CI 冷启动 `powershell.exe` 可超 10s,超时留足余量,失败输出带 `status`/`signal`/`error`/截断 `stderr`。

---

## 7. 两条棘轮(不要绕过)

### 7.1 静默失败:空 catch 必须认领
`build/check-silent-failures.js` 扫描前端空 `catch {}`/`.catch(() => {})`,须标 `/* silent: <理由> */`。落盘写操作(`set_config`/`set_pin`/`save_window_pos` 等)**不该用 silent**,按 `saveCfg()` 回滚 + `showToast`;`silent` 只用于读失败降级。

### 7.2 文件规模:上限只许下调
`build/check-file-size.js` 为超标文件钉了物理行数预算(名单与上限以脚本为准,勿用 PowerShell `Measure-Object -Line` 核对)。**清理后同步下调上限**;新功能被拦时拆新模块,确需上调须在脚本注释写理由。

---

## 8. 按模块的已知陷阱

> 改到对应模块前先读本节,回归须覆盖所列边界。

### 8.1 日期、时钟与缓存时效
- 时间敏感缓存须含**时钟失效**;已过期项不能降级成「到期日未知」参与计算。
- 日期显示:积分卡与周重置共用格式,优先「今天/明天/后天」,其余「本周X/下周X/周X」,完整提示保留星期和时刻。
- Codex/Antigravity 周重置取各自周窗(Antigravity 取 Gemini 周池),缺失不借短窗;跨日须重算可见日期与提示。
- 回归配固定时钟,覆盖跨午夜、周日到周一、跨月跨年、闰日、夏令时及三种布局 + 首页摘要;**勿把一周近似为固定毫秒**。

### 8.2 Codex 重置顾问
规则以 `docs/report/codex-reset-policy-review-2026-11-14.md` 为准(旧 v3 不得覆盖)。
- 账号隔离同时检查存储键、前端缓存与迟到写请求的账号指纹。
- reason code→文案用穷举 `Reason` enum 编译期防漏;前端命令失败显示「建议暂时不可用」,与后端「无法判断」区分。
- 「今天要用」截止于本地当天 23:59:59(`AdviceInput.today_end_at`),不按 24h 近似,临近重置的省卡规则也查此截止;先判已耗尽窗口能否在期限内恢复,卡临期只提示风险。明确「现在就要」优先于 1 小时省卡偏好。
- 周额度与 5h 分别处理,完整重置范围按套餐核对,不符手动兑换门槛不输出 NOW。卡查询失败的旧库存只展示,不据此建议用卡;跨日与读失败分别验判定。
- 订阅到期日可留空/清空:空值按最后有效卡到期规划并逐张查卡有效期,界面明示「假设订阅持续有效」;禁止把卡到期写回订阅字段,缺日期或查询失败不构造截止。清空走 `clearSubEnd`,验持久 / 失败回滚 / 有无日期 / 跨到期重算。
- 日期编辑期间的分钟重算与详情刷新须保留输入节点、焦点和草稿,回归用真实可聚焦容器。

### 8.3 签到
- 前端签到名单必须从 `checkin_state` 动态生成(含已关闭渠道),签到中心/渠道卡/首页统计覆盖后端 `CHECKINS` 全部定义。
- 直连伪接口要有「平台收紧即下线」预案(豆包 claim 全报 710010702 后已移除签到,复盘见 `docs/report/doubao-checkin-710010702-2026-11-14.md`);与客户端版本联动的参数禁止抓包写死(`app/core/src/doubao_client.rs` 运行时探测 + Golden 双向验证)。

### 8.4 面板布局与窗口
- Tauri `destroy()` 异步派发:同名窗口重建须等标签释放,并使旧 session 回调失效。
- 缩略专用名称/状态只在缩略布局生成;卡片容量按真实列表高度扣除横幅、签到条、间距计算,坏样本覆盖漏行/单列/异常卡高。
- 尺寸与落点以 Tauri 显示器**工作区**为准;普通打开/贴边展开/解除贴边共用计算,同 DPI 跨屏停稳后复算。核验记录工作区与整屏物理矩形、窗口边界、实际 `scaleFactor`、字号倍率及底栏/列表几何位置,勿假定缩放;坏样本用整屏替代工作区。
- 2026-11-23 网关 `.gw-tab-pane` 是 grid:隐式 auto 轨道会被账本表格的 nowrap 最小内容撑宽整个 pane(513px 容器出现 713px 卡片),单列必须钉 `minmax(0,1fr)`;横向越界用 `build/verify-gateway-layout.js` 在真机三种布局量,含超宽卡片坏样本自检。

### 8.5 Antigravity
- 缩略卡只用 Gemini 周窗与 5h 窗,不混入 Claude/GPT 限流。
- 账号身份与配额走官方 `statusLine` 同一回调,禁止按 CLI 路径或额度推断;邮箱只在回调内生成指纹。
- `-p` 不触发回调:Windows 用临时 ConPTY + 单次 `--settings`,回调用系统 PowerShell `EncodedCommand`;进程入 kill-on-close Job Object;`cmd.exe` 路径逐段 join,回归覆盖空格/`&`/`!`。
- 成功与失败共用冷却,时钟用注入值测试;账号独立快照/卡片/隐藏,越过重置点留原余额并提示待刷新;断言覆盖合并、错归属、敏感字段泄露、保存失败。

### 8.6 本地网关
- 操作状态:忙碌仅持有者释放;筛选只隐藏完整节点、诊断局部更新、重绘保留脏字段/焦点,状态与诊断分别验代际;后端刷新仅合并探测字段并核对配置/凭据版本;凭据失败要补偿,端点与配置同事务,多目标同步须汇总未完成与失败。
- 协议:真实实现与回环测试在 `app/core/src/gateway/`,禁止另造成功宿主桩。错误先脱敏再截断;逐候选验完成,末帧先处理正文再结束,拒绝是有效输出;缓冲超限明确失败;重启等监听释放而非固定等待,失败恢复旧服务。
- 回归核对裸模型、call ID、流式索引/用量、断流/取消、共用鉴权和失败落盘。
- 2026-11-23 OpenCode 桌面版进程名是 `opencode-cli.exe`(辅助进程 `opencode-pty.exe`),只查 `opencode.exe` 会漏判「在跑」而边跑边写;判在跑按 `Client::processes()` 全名任一命中,界面徽章显示实际命中的名字。

### 8.7 Windows WebView2 网页会话
- Wry 同步 Cookie 读取会在 UI 线程开嵌套消息循环,重入导航回调导致卡死(2026-11-19)。读写清理统一走 `app/src-tauri/src/webview_cookies.rs`:UI 线程注册回调即返回,工作线程等结果;**读回确认删除后才算成功**。
- 原生回归用隔离 profile + 合成 Cookie,`--sync-sentinel` 须在时序断言失败;原生 example 须链入 Common Controls v6 manifest。

### 8.8 Google Cloud 赠金
- 已过期记录仍可能带正剩余金额,须先按状态排除再汇总;网页读取检查表格加载/过滤/分页,未知结构留旧快照并报错。
- 手填日期按结算账号 + 赠金 ID 隔离,到期只改提示。
- 独立前端模块的 IPC 须纳入全目录扫描(门禁只扫 `app/src` 一层,子目录会漏扫)。

### 8.9 Qoder 临期
- 额度用完平台仍返回 `expiresAt`,总额含附加积分 → 按 `userQuota.remaining` 停提醒;仅逐笔计金额。
- 网页明细失败沿用的列表仅供查看,须用结构化 `expiryStale` 标记,禁止凭旧文案或 `expiryComplete` 推断会话有效。
- 自然日窗口/柱图/筛选共用边界(第八日零点、夏令时、过期点);登录态区分平台拒绝/结构未知/网络失败/旧快照。

### 8.10 AI 优惠
- 2026-11-23 校验官网公开列表的结构化条目后再替换缓存,并按详情链接迁移已读状态。旧 RSS 已返回 HTTP 410,上游变动时须保留上次快照。见 `docs/report/promo-rss-retired-2026-11-23.md`。
- 2026-11-23 官网中文标题的 slug 是百分号编码、卡片 id 是解码字面:校验与查摘要按 `decodeURIComponent(pathname)`(解码失败按坏条目拒),id/链接保留编码原样;解码后仍须拒路径穿越/查询/外链。多页取数任一页坏即整体保留旧快照。
