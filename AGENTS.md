# AGENTS.md — 自动化改代码的硬约束

> **写给所有会改这个仓库的 agent / 工具。** 本文件受版本控制,是跨 agent 的公共记忆。
> 仓库里同时存在 `.workbuddy/`、`.workbuddy-ai/`、`.zcode/`、`.trae-html-share-packages/`、
> `.dsh-acl-report/` 等多套 agent 工作目录,它们**都被 gitignore 且互不共享**。
> 把结论只写进自己那一份 memory,下一个 agent 一定看不到 —— 这是本项目反复踩坑的结构性原因。
> 任何「下次别再这样」的结论,必须落到本文件,再顺手记进自己的日志。

---

## 0. 动手前:先找单一事实源

| 要确认什么 | 看哪里 |
|---|---|
| 当前版本、最近改了什么 | `PROGRESS.md` **文末最新条目** + `git log`(头部状态是快照,会过时) |
| 产品口径 / 设计裁决 | `DESIGN.md` |
| 发版流程、签名、回滚 | `RELEASE.md` |
| 模块划分、加渠道/客户端的步骤 | `app/README.md` |
| 历史体检、冗余审计 | `docs/report/` |

**改之前先 grep 现有实现。** 本项目多次出现「同一件事写了第二份」:旧摘要 CSS 段被新段完全覆盖、
`SyncStatus` 与 `GithubStatus` 两套等价结构、`savePosSoon` 与 `scheduleSettleCheck` 并存。
找不到调用方 ≠ 可以新增一份,先用 `build/check-ipc-contract.js` 和全仓 grep 确认。

---

## 1. 本次治理查到的真实问题(别重犯)

2026-11-12 全量审计的结论与证据见 `docs/report/code-governance-2026-11-12.md`。
下面每条都是**实测**出来的,不是通用建议。

| # | 现象 | 证据 |
|---|---|---|
| 1 | 机器产物被提交进版本库 | 2 个 APK(21.5 MB)、6 份 `refresh-*.json`(每份 1.4 MB / 5.8 万行)、1 个 Trae 分享 zip 被 git 跟踪,合计约 30 MB;`--stat` 里一次提交出现 11.6 万行「新增」 |
| 2 | 验证桩让错误永远测不出来 | `jsdom-smoke.js` 的 invoke 桩对未登记命令一律 `return null`;窗口对象用 Proxy 把未知方法变成恒成功的空函数。命令名拼错、漏注册都能 182/182 全绿 |
| 3 | 门禁只在某台机器上存在 | 同一脚本曾写死 `G:/game/nw/Grandettoken/app/src`;jsdom 未随仓库固化,换个目录直接报「需要 jsdom」,而 `RELEASE.md` 仍宣称全绿。CI 当时只跑 5 个文件的 `node --check` |
| 4 | 验证脚本自己会腐烂且没人复核 | `verify-codex-layout.js` 在 v0.3.35 基线就必然抛 TypeError(引用已删除的 `.quota-number`),直到 11-12 清理时才被发现;`verify-refresh-stability.js --compare` 只打印表格就 `exit(0)`,没有阈值也没有断言 |
| 5 | 构建链路曾「失败却报成功」 | `build.ps1` 注释记录:捕获失败的构建打印了 `ok import table clean` 并退出 0(PROGRESS 报告 P0-3) |
| 6 | 文档数字与实测长期不一致 | README 写「227 个单元测试」、CI 注释写「422 tests, 9 ignored」,实测 `425 passed / 8 ignored`;PROGRESS 头部停在 v0.3.22,实际已 v0.3.36 |
| 7 | 纪律写了却没有门禁 | `app.js` 注释明确写「**不许吞错**」,全文件仍有 14 处空 catch,其中 5 处是 IPC 写操作(`set_pin` / `save_window_pos` / `skip_update_version` 等) |
| 8 | 一次性脚本像正式代码一样躺着 | `.dsh-acl-report/` 64 个探针:`decrypt_desktop*.js` 解桌面端凭据、`dump_ck.py` 导 Cookie、`fix_n05.py` 对源码做全局字符串替换(只靠一个 `assert` 校验),路径写死到本机 |
| 9 | 门禁自身也会假阳性 | 新写的 IPC 门禁第一版把带 `//` 注释的注册行整项丢掉,误报 `key_source` 未注册。**门禁报错不等于代码有错,先证伪自己** |

---

## 2. 硬规则

### 2.1 验证(最重要)

1. **双向验证**:门禁必须既能「当前代码全绿」,也能「注入一个已知坏样本时变红」。
   单向的「变绿了」说明不了任何事 —— 这条纪律项目里早有(`PROGRESS.md` 里 `scan-secrets.ps1` 的夹具仓库实测),但没推广开。
   `build/check-ipc-contract.js` 内置了哨兵命令自检,照这个模式写新门禁。
2. **期望值从源码实时提取,不写死**。命令表、测试用例数、版本号一旦写死就会腐烂。
   同理,文档里不要写具体测试数量,以命令输出为准。
3. **能进 CI 的门禁必须零外部依赖**;有依赖的必须写明安装方式、失效条件,并且不许在文档里把它说成「已通过」。
4. **验证脚本本身也要被验证**:改了渲染 / CSS / 命令后,先确认 `build/verify-*.js` 引用的选择器、命令、路径还存在。
   破损的脚本宁可删掉,也不要留着冒充门禁。
5. **「只打印、不判定」必须显式标注**。`--compare` 之类报告工具的 `exit 0` 不代表通过。
6. **不许静默吞错**。写操作失败的范式是 `saveCfg()`:回滚状态 + `showToast(...)` 用户可见。
   GUI 程序里 `console.warn` / `eprintln!` 没人看得见,不能拿它当「已处理」。
7. 提交前门禁清单见第 4 节;**没跑完不许写「已验证」**。

### 2.2 仓库与产物

- 抓包 JSON、APK、安装包、截图归档、分享包 zip **一律不入库**,加进 `.gitignore`,本地留档即可。
- 一次性探针 / 补丁脚本放 `build/tmp/` 并在同一轮改动里删掉;**禁止**把本机绝对路径、真实凭据、解密脚本写进仓库。
- 脚本里禁止硬编码 `G:/...`、`C:/Users/...`;一律 `path.resolve(__dirname, ...)` / `os.tmpdir()` / 环境变量。
- 纯追加的日志型文档要定期修头部状态;头部与 `git log` 冲突时以 `git log` 为准。

### 2.3 改代码

- **批量字符串替换是最后手段**。必须逐处列出命中、说明为什么每处都该改,改完跑全量门禁;
  `.dsh-acl-report/fix_n05.py` 那种 `s.replace("secrets::get(", "secrets::channel_key(")` 不许再用。
- 大文件已经超标:`app.js` 9.1k 行、`inject.rs` 7.1k 行、`src-tauri/lib.rs` 6.4k 行、`fetch.rs` 5.2k 行。
  **新增功能优先拆出新模块,不要继续往这几个文件里堆**;单个改动超过 300 行时先说明拆分方案。
- 注释要写「为什么」,不要写「以前这里有个 bug」的流水账;修复完成后删掉临时性注释。
- 删东西前先确认零引用(含 `build/`、跨端协议 `contract/`、Android、文档),并注意 `pub` 符号与 Tauri 命令注册是编译器看不见的引用。
  参考 `docs/report/redundancy-audit-2026-11-11.md` 的二次确认方法与 11 类误报清单。

### 2.4 高权限 / 敏感行为

本项目会做这些事,**每一项都必须在 README 披露、可关闭、有明确触发条件**:

- 用 `ELECTRON_RUN_AS_NODE=1` 启动其它桌面应用并读其内部存储以派生凭据密钥,超时用 `taskkill /T /F` 杀进程树;
- 改写其它客户端(Trae / WorkBuddy / ZCode / DSH)的配置文件与 SQLite,留下备份快照
  (**2026-11-12 起每份都用 Windows DPAPI 加密**,见 `backup_crypto.rs`;换 Windows 账户后旧快照无法解密);
- 读取并解密本机 WebView2 Cookie / 桌面端加密凭据;
- 默认开启的每日自动签到、写注册表自启动、监听局域网的本地网关。

**新增同类能力前先问用户**。不允许用「方便」「自动化」为由默认打开,也不允许把凭据、令牌、Cookie
写进日志、配置、导出文件或 QA 数据 —— `diaglog.rs` 的脱敏白名单和 `publish.rs` 的 `crypto::seal`
是既有防线,改动时先跑相应单测。

---

## 3. 「恶意代码」的判定口径

2026-11-12 全量扫描**未发现传统后门**:出网域名全部是渠道自身域名 / GitHub / 汇率 API / 官方 CDN;
无 `eval` / `new Function` / 远程脚本加载;CSP 只放行自身;日志有脱敏;同步载荷先加密再上传;
无凭据外发。但上面 2.4 的高权限链确实存在,且是多次自动化迭代逐渐加进来的 ——
**它们不是后门,却同样需要逐项知情同意**。发现新的出网目标、新的凭据读取路径、新的持久化写入时,
先当作可疑项报告,不要静默合入。

---

## 4. 提交前门禁(可直接复制)

```bash
node build/check-ipc-contract.js          # 调用/定义/注册三方比对,零依赖
node build/check-silent-failures.js       # 空 catch 必须认领(silent: 理由)
node build/check-file-size.js             # 文件规模棘轮,上限只许下调
node --check app/src/app.js
node build/check-css-forms.js             # CSS 形态门
node build/jsdom-smoke.js                 # 需 jsdom: npm install jsdom --no-save
node build/verify-codex-reset.js          # Codex 重置顾问独立模块冒烟（同样需 jsdom）
cd app/core && cargo test --locked        # 用例数看输出,别写死
cd ../src-tauri && cargo check --all-targets --locked
cd ../.. && powershell -File build/scan-secrets.ps1   # 凭据扫描,非零退出必须停
```

本机 GNU 工具链前置:`PATH` 含 `C:/Users/wacil/mingw64/bin`(gcc)与 `~/.cargo/bin`,
工具链 `stable-x86_64-pc-windows-gnu`。**门禁缺依赖就如实写「未跑」,不要用别处的历史结果冒充本次验证。**

**发版走 CI,不要本地打包**(2026-11-12 起):同步三处版本号 → 推 main → 推 `v*` tag,
`release.yml` 自动跑「门禁 → 构建 → 签名 → 建 release → `verify-release.js` 核验」。
本地 `build/release-local.sh` 只在 Actions 不可用时用。完整步骤见 `RELEASE.md`。
tag 之前**必须**先推 main —— tag 指向的 commit 不在 main 上,GitHub 生成的源码归档会与代码不一致。

**门禁要串行跑,并单独复跑确认**:2026-11-12 把 core 编译、src-tauri 链接、jsdom 冒烟
并行跑时,`proxy` 一个用例与 jsdom 两项因资源竞争假红,单独复跑立刻全绿。
假红不能当回归,真红也不能当噪音 —— 结论必须是「单独跑一次仍成立」。

`src-tauri` 的 `cargo test` 二进制在本机启动即 `STATUS_ENTRYPOINT_NOT_FOUND`
(WebView2 静态加载器链栈老问题,`PROGRESS.md` 1464 / 4340 行早有记录),
**与业务逻辑改动无关**;CI 用 `cargo check --all-targets` 覆盖编译期,别把历史文档里
「19 passed」当成本机可复现的结论。

## 5. 2026-11-12 用户裁决(不许被后续 agent 推翻)

| 项 | 裁决 |
|---|---|
| 每日签到默认开启 | **保持默认开**,不要改成默认关 |
| 可监听局域网的本地网关 | **保留**,不要改成只绑 127.0.0.1 |
| 含明文密钥的备份快照 | **必须加密**,已用 Windows DPAPI 实现,见 `app/core/src/backup_crypto.rs` 与 `docs/report/code-governance-2026-11-12.md` 第七节 |

## 6. 验证不缜密的七种真实形态(2026-11-12 第二轮,逐条有案发现场)

> 这一节不是通用建议,是本仓库**实际发生过的**失效方式。写在这里是因为
> 它们全都有一个共同特征:**验证做过了,结论是绿的,但绿得没有意义。**

### 6.1 「只测成功路径」

`saveCfg()` / `saveConfigAndRefresh()` 的回滚逻辑是后补的:在此之前所有保存路径
都只验证过"能存进去",没人验证"存不进去会怎样"。后果是用户实测「隐藏渠道点 – 恢复不动」——
界面先把状态改了,写盘失败后既不回滚也不提示,看起来就是"点了没反应"。
**任何写操作,验完成功必须再问一句:失败时用户看到什么?**

### 6.2 「桩的默认行为是成功」

`jsdom-smoke.js` 的 invoke 桩对未登记命令 `return null`,窗口对象 Proxy 把未知方法
变成恒成功空函数。于是命令名拼错、Tauri 命令漏注册,182 项冒烟照样全绿。
**mock 的默认值必须是"失败"或"抛出",不能是 null / 无声成功**;要用宽松桩,
就必须另配一道从源码提取的静态门(见 `check-ipc-contract.js`)。

### 6.3 「只测存在,不测顺序 / 状态 / 时序」

顶栏四枚按钮的冒烟原来只断言"四枚存在且可见",顺序怎么挪都绿;后来用户要求把
隐藏挪到最右,才补上 `compareDocumentPosition`。`loadCheckin` 的并发乱序
(两次读晚到的旧快照盖掉新结论)是线上撞出来的,测试里从来没并发过。
**加了新入口就问:顺序变了会不会坏?两个请求同时到会不会坏?**

### 6.4 「验证脚本自己会腐烂,而且没人复核它」

`verify-codex-layout.js` 在 v0.3.35 基线就必然抛 TypeError —— 它引用的 `.quota-number`
类早已不存在。脚本躺在 `build/` 里,文档还宣称它通过过。
**改了渲染 / CSS / 命令之后,先确认 `build/verify-*.js` 引用的选择器和命令还在。**
破损的脚本宁可删掉,也不要留着冒充门禁。

同类但更隐蔽的一种:**脚本对"前置条件不满足"的处理前后不一致**。
`verify-release.js` 在本地没有安装包时 SKIP(合理),没有 `.sig` 时却判 FAIL ——
而 CI 发版后本地恰好两者都没有,于是"二次确认"稳定报一个假失败(2026-11-12 v0.3.38 实测)。
假失败比不设门禁更糟:它会训练人忽略这道门。**同一个脚本里,同一类前置缺失必须用同一种口径。**

### 6.5 「门禁报错就照着改代码」

2026-11-12 出现过两次,方向相反但根因相同:

| 次数 | 现象 | 真实原因 | 如果照改会怎样 |
|---|---|---|---|
| 第一次 | IPC 门禁报 `key_source` 未注册 | 注册表里那行上一行是 `//` 注释,按逗号切分把注释和命令粘成一项丢了 | 会去给一个**已经注册**的命令补注册,制造重复注册 |
| 第二次 | 静默失败门禁报 115 处空 catch | JS 的正则匹配结果**没有** `.end`(那是 .NET 的 API),`m.end - 1` 得 NaN,块体全取成空串 | 会去给 100 多处**本来有处理逻辑**的 catch 加提示,把界面刷满 toast |

**门禁红了先证伪门禁自己。** 具体做法:把已知正确的样本丢进去,它必须是绿的;
做不到就说明门禁坏了,不是代码坏了。

### 6.6 「门禁没有自检」

上面两次假红之所以能被快速识别,是因为新门禁都带 `--self-test`。
不带自检的门禁只能用"当前代码恰好绿"来证明自己,而"当前代码恰好绿"同样可以由
"门禁什么都没检查"造成。
**新门禁必须能证明自己会变红**(`check-ipc-contract.js` 的哨兵命令、
`check-silent-failures.js` / `check-file-size.js` 的 `--self-test` 都是这个模式)。

### 6.7 「在错误的机器 / 错误的并行度上取结论」

- `src-tauri` 的 `cargo test` 二进制在本机启动即 `STATUS_ENTRYPOINT_NOT_FOUND`,
  历史文档却长期写着「19 passed」。**换机器跑不通的结论不能当基线。**
- 并行跑 core 编译 + src-tauri 链接 + jsdom 冒烟时,`proxy` 一个用例与 jsdom 两项
  因资源竞争假红;单独复跑全绿。**门禁要串行,结论要单独复跑一次。**

## 7. 两条棘轮(2026-11-12 新增,不要绕过)

2026-11-13 Google Cloud 赠金接入补充：赠金表中的已过期记录仍可能显示正的剩余金额，
必须先按状态排除再汇总。网页读取需检查表格加载、过滤与分页，未知结构保留旧快照并报错。
手填日期按结算账号与赠金 ID 隔离，日期到期只改变到期提示，余额仍保留上游最近上报值待刷新核对。
独立前端模块的 IPC 同样纳入全目录扫描，避免门禁只检查 app.js 而遗漏新模块。

2026-11-13 重置顾问评审补充：时间敏感的建议缓存必须包含时钟失效，不能只看快照更新时间；已过期项目不能降级成“到期日未知”继续参与供给模拟。账号隔离除了存储键，还必须检查前端缓存与迟到写请求的账号指纹。证据见 `docs/report/codex-reset-review-2026-11-13.md`。

v0.3.42 发布核验补充：`verify-release.js` 的 `notes intact` 检查读取线上 `latest.json.notes`，单独编辑 GitHub Release 正文不会改变检查结果。修正发布说明时需同步核对更新清单，并记录原始失败与修正后的实测结果，不能把原 run 写成全绿。

### 7.1 静默失败:空 catch 必须认领

`build/check-silent-failures.js` 扫前端所有 `catch {}` / `.catch(() => {})`,
块内没有任何语句的必须带 `silent: <理由>`:

```js
try { await invoke("save_window_pos", { x, y }); }
catch (e) { /* silent: 位置存不下只影响下次启动的落点 */ }
```

写操作(尤其是 `set_config` / `set_pin` / `save_window_pos` / `skip_update_version`
这类落盘命令)**不该用 silent** —— 按 `saveCfg()` 范式:回滚内存状态 + `showToast`。
`silent` 只用于"读失败就降级、没有用户需要知道的状态"。

### 7.2 文件规模:上限只许下调

`build/check-file-size.js` 给 10 个已超标文件钉了行数预算(约当前值 +1%)。
**清理掉代码后要把上限一起调下来**,否则清理出来的空间会被下一轮改动吃回去 ——
这正是 2026-11-11 清理完、11-12 又长回原样(425 → 431 行 diff)的原因。

新增功能写在 `app.js` / `inject.rs` / `lib.rs` / `fetch.rs` 里会被这道门拦下。
拦下时的正确做法是**拆新模块**,不是上调预算;确需上调,理由写在
`check-file-size.js` 的注释里(会被 review 看到)。

