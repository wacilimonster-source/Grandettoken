# `app/src/app.js` 拆分方案(2026-11-22,待拍板)

> **性质**:实施前方案。按 `AGENTS.md` §2.3「单次改动超过 300 行先说明拆分方案」写成,**未改任何产品代码**。
> **对应路线图**:S1-1。
> **数字口径**:行号、计数都是 2026-11-22 当天对 `app/src/app.js`(9014 行)做的静态扫描快照,会随代码变化而失效。
> 实施时以拆分脚本和门禁的输出为准。扫描基于正则,足够定方案,但**不能替代**下文 §6 的运行期门禁。

---

## 0. 结论

- **拆法**:按原文顺序切成 **14 个连续片段**,每段一个普通 `<script>`(不是 ES module),按原顺序写进 `index.html`。
  **不改一个字符**:14 个文件依次拼接后与原 `app.js` 逐字节相同。
- **为什么能这样拆**:多个普通脚本共享同一个全局词法作用域,顶层 `let` / `const` / `function` 彼此可见。
  项目里已经这么用了 —— `panel-layout.js` 比 `app.js` 先加载,运行时照样读 `CFG`、`render`、`saveCfg`(见 §1.2)。
- **唯一的语义差异**:函数声明只在**自己那个文件**里提升。加载时立即执行的代码如果调用了后面文件里的函数,会报 `ReferenceError`。
  静态扫描发现 228 处加载期代码里只有 3 处引用了后面才声明的函数,切分点都避开了(§4)。
- **最大工作量不在搬代码,而在测试**:9 个测试 / 验证脚本直接读 `app.js` 的**源码文本**(拼接、正则截取、哨兵替换)。
  所以分两步:**1a 先改测试读法,app.js 不动;1b 再搬家**。两步各自过一遍全量门禁。
- **棘轮不放松**:`app.js` 的 9014 行预算改成「`app-*.js` 合计 ≤ 9014」,单文件仍受 1500 行软上限。
  否则拆完等于把总预算从 9014 放宽到 14 × 1500。

**需要你拍板的 5 件事**见 §9。

---

## 1. 现状与约束

### 1.1 文件内部

| 项 | 快照值 |
|---|---|
| 物理行数 | 9014(预算 9014,余量 0) |
| 分段横幅(`// ─── xxx ───`) | 25 段;最大一段「同步到手机」1626 行,但后半实际是置顶 / 吸附条 / 设置页事件 |
| 顶层声明 | 436 个(函数 + `const` / `let`) |
| 顶层可变状态 `let` | 72 个,散落在各段(`CFG`、`CHANNELS`、`VAULT`、`CK`、`trayKey`…) |
| 加载期立即执行的代码 | 228 处(含 90 条顶层语句:事件绑定、`setInterval`、`refreshTraySize()`、`bindSyncUI()`、`init()`) |

### 1.2 现有模块已经依赖「共享全局作用域」

扫描其它前端文件引用了哪些 `app.js` 顶层名字(只在运行期使用,加载期不用):

| 文件 | 加载顺序 | 用到的 `app.js` 名字(节选) |
|---|---|---|
| `vault-view.js` | app.js **之前** | `$`、`esc`、`invoke`、`render`、`showToast` |
| `quota-plan-ui.js` | app.js **之前** | `CFG`、`CHANNELS`、`openDetail`、`render`… |
| `panel-layout.js` | app.js **之前** | `CFG`、`CHANNELS`、`applyForm`、`saveCfg`、`hvRow`… 共 18 个 |
| `gateway-view.js` | app.js 之后 | `$`、`esc`、`invoke`、`listen`、`showToast` |
| `codex-reset.js` | app.js 之后 | `CHANNELS`、`DETAIL_ID`、`renderDetail`… |
| `antigravity.js` | app.js 之后 | `CFG`、`rowMore`、`saveCfg`… 共 15 个 |

所以「多个普通脚本 + 共享全局」不是新机制,只是把 `app.js` 自己也按这个方式切开。

### 1.3 直接读 `app.js` 源码文本的脚本(拆分后会断)

| 脚本 | 怎么用 | 拆分后的风险 |
|---|---|---|
| `build/jsdom-smoke.js` | 读全文 → 4 个哨兵环境变量(`ANTIGRAVITY_ACCOUNT` / `PERFORMANCE` / `WORK_AREA` / `CHECKIN`)共 5 处 `js.replace(...)`,外加 `initMarker` → 与其它模块拼成一串 `w.eval` → 另有两处正则断言 | 找不到文件直接报错;**部分 `replace` 前没有命中检查,未命中时静默不变**(哨兵失效却不报) |
| `build/probe-frontend.js` | 读全文 `vm.runInContext` | 找不到文件报错 |
| `build/verify-codex-reset.js` | 对全文做 4 条 `includes` 断言 | 找不到文件报错 |
| `build/verify-google-cloud.js` | 对全文做 2 条正则断言 | 同上 |
| `build/verify-tray.js` | 按两个注释锚点截取托盘实现 | 同上 |
| `build/verify-refresh-stability.js` | 在 `index.html` 的 `<script src="app.js">` 前后注入桩和驱动 | **`.replace` 未命中时静默不注入**,测试可能空跑 |
| `build/verify-vault.js` | 断言 `vault-view.js` 在 `app.js` 之前加载 | `indexOf` 返回 -1,断言语义被破坏 |
| `build/test-sync-persistence.py` | 从 HTML 删掉 `app.js` 标签,再按 `function syncSummary()` 到 `bindSyncUI();` 截取 | 截取范围恰好跨两个新文件 |
| `build/verify-thumbnail-layout.py` | 正则取 `fmtDay` / `fmtClock` / `const esc` | 找不到文件报错 |

另外:`ci.yml` 与 `release.yml` 的 `node --check` 是**逐个文件写死**的清单。

### 1.4 门禁的扫描范围

- `check-ipc-contract.js`、`check-silent-failures.js` 用 `readdirSync("app/src")` **只扫一层** → 新文件必须**平铺在 `app/src/`**,不能放子目录(否则静默漏扫,AGENTS.md §8.8 踩过)。
- `check-file-size.js`:预算表外的文件超过 1500 行即失败;预算表里的文件不存在也失败(删掉 `app.js` 时必须同步删预算项)。
- CSP 只放行自身,`tauri.conf.json` 的 `frontendDist` 是 `../src`:新增同目录脚本无需改配置。

---

## 2. 方案选型

| 方案 | 做法 | 结论 |
|---|---|---|
| **A. 多个普通脚本,按原顺序连续切片** | 14 个 `<script src>`,顶层名字不变 | **推荐**。零重命名、零行为变化;与现有 6 个模块同一机制;拼接后可逐字节校验 |
| B. ES modules(`type="module"` + `import`/`export`) | 每个文件显式导出导入 | 不采用。72 个顶层 `let` 被跨段**赋值**,模块导出的绑定是只读的,得全部改成 setter 或状态对象 —— 这已经是重写;且 6 个现有模块和所有测试桩都按全局名访问 |
| C. 源码分文件,构建期拼回 `app.js` | 加一个拼接脚本 | 不采用。项目明确「无打包器」;生成物入库会和「产物不入库」冲突,不入库又要改 Tauri 构建链;9014 行文件依旧存在 |
| D. 每段包成 IIFE,挂到 `window.AppXxx` 命名空间 | 改所有跨段调用 | 不采用。跨段引用上千处(§3.2 依赖表),改动面巨大,属于第二阶段以后的事 |

---

## 3. 拆分清单

### 3.1 文件表

切分点一律取**分段横幅**或紧贴函数的注释首行,前面的空行留给上一个文件。实施脚本按**锚点文本**定位,不按行号(行号会漂移)。

| # | 文件 | 原行号 | 行数 | 内容 | 切分锚点(文件首行) |
|---|---|---|---|---|---|
| 1 | `app-core.js` | 1–658 | 658 | Tauri 句柄、全局状态、主题与字号、格式化、统一视图 `HV` | 文件开头 |
| 2 | `app-rows.js` | 659–1622 | 964 | 行渲染、行展开五段骨架、逐笔到期 | `// ───── 渲染 ─────` |
| 3 | `app-view.js` | 1623–2526 | 904 | 详情弹层、`qReconcile` 最小 DOM 更新、主渲染链 `renderInner`、紧凑条、胶囊渲染 | `// ───── 渠道详情弹层(单例) ─────` |
| 4 | `app-tray.js` | 2527–3047 | 521 | 胶囊速览、胶囊外观、托盘数字与图标 | `// ─── 胶囊单击速览(方案 C:抬升卡片)───` |
| 5 | `app-channel-cards.js` | 3048–3626 | 579 | 设置页渠道卡、按需说明、渠道密钥来源 | `/** 设置页「渠道」页签:` |
| 6 | `app-window.js` | 3627–4136 | 510 | 形态切换、贴边吸附 | `// ───── 形态切换 ─────` |
| 7 | `app-manage.js` | 4137–4611 | 475 | 设置页开合、全局事件绑定、`showToast`、设置索引与页签 | `// ───── 设置与管理(四页签…` |
| 8 | `app-sync.js` | 4612–5314 | 703 | 同步到手机(到 `bindSyncUI();` 为止) | `// ───── 同步到手机 ─────` |
| 9 | `app-settings.js` | 5315–6372 | 1058 | 置顶、标题栏 / 吸附条 / 胶囊轮播事件、`saveCfg`、设置页事件、开关绑定、自定义渠道表单 | `// 置顶:窗口状态与配置一起改` |
| 10 | `app-update.js` | 6373–6968 | 596 | 快捷键录制、检查更新、更新提示、排查日志 | `// ───── 快捷键录制 ─────` |
| 11 | `app-vault.js` | 6969–7109 | 141 | 本地密钥箱 | `// ───── 本地密钥箱 ─────` |
| 12 | `app-checkin.js` | 7110–7975 | 866 | 每日签到、签到中心 | `// ───── 每日签到(Trae / WorkBuddy)───` |
| 13 | `app-models.js` | 7976–8692 | 717 | 模型页 | `// ───── 模型页:多来源 + 弹层确认 ─────` |
| 14 | `app-boot.js` | 8693–9014 | 322 | `refresh`、`applyChannelsPush`、`init()` 启动、AI 优惠 | `// ───── 启动 ─────` |

- 合计 9014 行。最大的是 `app-settings.js`(1058 行),都低于 1500 行软上限。
- 命名统一用 `app-` 前缀。**不保留叫 `app.js` 的文件**:任何漏改的旧引用会直接 ENOENT 响亮失败,而不是读到一个片段后静默通过。
- `app-boot.js` 必须最后加载:`init()` 是 async IIFE,它的同步部分在加载时就调用了大量前面文件的函数。

### 3.2 跨文件依赖(引用次数,只统计「用到别的文件里声明的顶层名字」)

| 文件 | 主要依赖 |
|---|---|
| `app-core.js` | 几乎不依赖别人(`app-view` 2、`app-manage` 1、`app-update` 1,均在函数体内,运行期才用) |
| `app-sync.js` | 只依赖 `app-core`(263)和 `app-manage`(45)—— 边界最干净 |
| `app-vault.js` / `app-models.js` | 基本只依赖 `app-core` + `app-manage` |
| `app-settings.js` | 依赖其余 11 个文件 —— 它是事件接线层,符合预期 |
| `app-boot.js` | 依赖 8 个文件 —— 启动编排,符合预期 |

读法:`app-core` 是公共底座。第二阶段要做的主要是把散落在 `app-manage` / `app-settings` 里、却被处处使用的工具函数(`showToast`、`saveCfg`、`bindSwitch` 等)挪回底座(§8)。

---

## 4. 跨文件语义差异与对策

拼成一个脚本和分成 14 个脚本,行为上只有三处不同:

| # | 差异 | 本次情况 | 对策 |
|---|---|---|---|
| 1 | **函数提升只在本文件内**:加载期代码调用后面文件里声明的函数 → `ReferenceError` | 静态扫描 228 处加载期代码,只有 3 处引用了**后面**才声明的函数:L2974 `refreshTraySize()` 与 L2976 `setInterval(refreshTraySize, …)` 用到 L2977 `drawTrayIcon`(同属 `app-tray.js`);L6545 `addEventListener("click", startInstall)` 用到 L6832 `startInstall`(「快捷键 → 更新提示 → 排查日志」因此必须同在 `app-update.js`) | 切分点已避开。扫描基于正则,所以另加**逐文件分别加载**的运行期门禁(§6) |
| 2 | **失败范围变小**:某个文件加载时抛错,只有它的后半段没执行,其它文件照常加载 | 原来是整个 app.js 停在出错处 | 视为改进(故障更局部、报错带文件名),但测试必须逐文件加载,才能看出是哪个文件坏了 |
| 3 | **跨文件重复声明**:后面文件里再写一次 `let CFG` → 那**整个文件**都不执行(SyntaxError) | 切片来自同一个文件,初始不会重复 | 门禁检查跨文件顶层名字唯一(§6),防止以后新加代码时撞名 |

加载顺序**不需要**改动 `index.html` 里其它模块的位置:14 个 `app-*.js` 整体占据原来 `app.js` 那一行的位置。

---

## 5. 实施步骤

### 阶段 1a:先改测试读法(`app.js` 仍是单文件)

目标:让所有测试不再写死 `app.js`,改完后在单文件状态下全绿。**证明测试改造本身是中性的**,再去搬代码。

1. **新增 `build/lib/app-sources.js`**(零依赖):
   - `appFiles()`:解析 `index.html`,按出现顺序返回所有 `src="app.js"` 或 `src="app-*.js"` 的文件名(1a 阶段只有 `app.js`);
   - `appSource()`:按顺序直接拼接全文(每个片段都以换行结尾,直接拼接就还原原文);
   - `replaceOnce(files, marker, replacement)`:**恰好命中一个文件的一处**才替换,否则抛错 —— 修掉现在 `replace` 未命中时静默放过的问题;
   - `loadInto(context, files)`:每个文件单独 `new vm.Script(code, { filename }).runInContext(context)`,与浏览器逐个执行 `<script>` 的语义一致。
2. **新增 `build/lib/app_sources.py`**:给两个 Python 脚本用的同等实现(`app_files()`、`app_source()`)。
3. **逐个迁移 §1.3 的 9 个脚本**:

   | 脚本 | 改法 |
   |---|---|
   | `jsdom-smoke.js` | 5 处哨兵替换与 `initMarker` 全改 `replaceOnce`(作用于其它模块的 `EXPIRY_UI` / `AG_LAYOUT` 哨兵顺手一并改);正则断言改对 `appSource()`;**加载改成逐文件 `loadInto(dom.getInternalVMContext(), …)`**,vault / expiry / layout / quota 等模块也一并逐文件加载 |
   | `probe-frontend.js` | `vm.runInContext(src)` 改为 `loadInto(ctx, appFiles())` |
   | `verify-codex-reset.js`、`verify-google-cloud.js`、`verify-tray.js` | 读 `appSource()` |
   | `verify-refresh-stability.js` | 桩注入到**第一个** app 脚本之前、驱动注入到**最后一个**之后;两处都断言注入成功 |
   | `verify-vault.js` | 断言 `vault-view.js` 在**第一个** app 脚本之前 |
   | `test-sync-persistence.py` | 删除 HTML 里**全部** app 脚本标签;截取改在 `app_source()` 上做 |
   | `verify-thumbnail-layout.py` | 读 `app_source()` |
4. **`ci.yml` / `release.yml`**:`node --check` 里的 `app/src/app.js` 一行换成对 `app/src/app*.js` 的循环(pwsh 的 `Get-ChildItem` 循环,每个文件判断退出码)。
5. **`check-file-size.js`** 新增「分组预算」:`app/src/app*.js` 合计 ≤ 9014,沿用「上限只许下调」规则;单文件继续受 1500 行软上限约束;`--self-test` 增加「合计超标判红」样本。
6. 跑 `AGENTS.md` §4.1 全量门禁,串行执行;jsdom 用例数不得少于改造前(以输出为准)。

**1a 的退出条件**:单文件状态下全绿;额外验证每个 `replaceOnce` 哨兵仍能让对应断言变红(`PERFORMANCE_SENTINEL=1` 等环境变量逐个跑一遍)。

### 阶段 1b:搬家(一次提交)

1. 一次性脚本 `build/tmp/split-app.js`(同轮删除):
   - 用 §3.1 的 14 个**锚点文本**定位切分点,每个锚点必须在原文中**唯一出现**且位于第 0 列,否则中止;
   - 写出 14 个文件、删除 `app.js`;
   - 在 `index.html` 里把 `<script src="app.js"></script>` 替换为 14 行,顺序同表。
2. **逐字节校验**:`cat app-core.js … app-boot.js` 的 SHA-256 等于 `git show HEAD:app/src/app.js` 的 SHA-256。不一致就整次回退。
3. `check-file-size.js`:删除 `app/src/app.js` 预算项,由 1a 的分组预算接管(合计上限先保持 9014,搬家后立即按实际合计下调)。
4. 文档:`app/README.md` 目录树、`AGENTS.md` §2.3 / §7.2 里的 `app.js` 改为 `app-*.js`(2026-11-22 精简后 `AGENTS.md` 约有 2.8 KB 余量,写入后复查字节数)。
5. 全量门禁串行跑一遍,再加 §6 的新门禁。
6. `PROGRESS.md` 文末追加记录,`docs/report/acceptance-backlog.md` 追加原生窗口验收项。

**提交前不夹带任何其它改动**:不修 bug、不改命名、不顺手整理注释。

---

## 6. 新门禁 `build/check-app-split.js`(带 `--self-test`)

| 检查 | 防的是什么 | 自检坏样本(必须判红) |
|---|---|---|
| `index.html` 里的 app 脚本清单 ≡ 磁盘上的 `app-*.js`;`app-core.js` 第一、`app-boot.js` 最后 | 新文件忘了加标签(整段不加载)、删了文件留下死标签 | 少一个标签;多一个孤儿文件;boot 不在最后 |
| 每个文件单独编译通过(`new vm.Script` 只编译不执行) | 切在函数中间 | 从函数中间截断的片段 |
| 跨文件顶层名字唯一(函数、`const` / `let` / `class`,含解构) | 后面文件整文件 SyntaxError | 两个文件各有一个 `let CFG` |
| 逐文件加载不抛错(jsdom,桩与 `jsdom-smoke.js` 共用) | 加载期调用了后面文件里的函数 | 在前面的文件顶层调用后面文件里的函数 |

接入 `ci.yml`、`release.yml` 的发版前门禁,以及 `AGENTS.md` §4.1(同样受 AGENTS.md 体积余量限制)。

---

## 7. 验证清单

**双向**(遵守 `AGENTS.md` §2.1):

- [ ] 1a:单文件状态全量门禁绿;每个哨兵环境变量单独跑仍能变红
- [ ] 1b:14 个文件拼接后的 SHA-256 与原 `app.js` 一致
- [ ] `check-app-split.js --self-test` 全部坏样本判红,正式检查判绿
- [ ] jsdom 冒烟逐文件加载,用例数不少于拆分前
- [ ] `verify-refresh-stability.js`:无头 Edge 用**真实 `<script>` 标签**加载,是最接近 WebView2 的加载顺序验证
- [ ] 原生调试实例(`build/launch-debug.ps1`):控制台无加载期报错;四种形态切换、设置各页签、签到中心、模型页、同步页、密钥箱逐个打开一次
- [ ] 首屏时间与 `build/qa/perf-baseline-2026-11-11.json` 同口径对比(14 个本地文件请求,预期差异可忽略,但要实测)
- [ ] 能跑的 CDP 脚本跑一遍(`verify-settings-entry` 等,对应待验收清单 B1 / B9)

---

## 8. 第二阶段(不在本次范围,各自单独提交)

搬家完成后,下面这些才是真正提升可维护性的工作。每项都是**行为不变的局部重构**,单独过门禁:

1. **公共工具归位**:`showToast`(现在在 `app-manage`)、`saveCfg` / `saveConfigAndRefresh` / `bindSelect` / `bindSwitch`(现在在 `app-settings`)被十几个文件使用,挪到 `app-core.js`。
2. **拆开超大事件处理器**:`app-settings.js` 里 `$("manage").addEventListener("click", …)` 一个回调 569 行(原 L5582–6150),按页签拆成独立处理函数。
3. **加载期接线显式化**:把散落在各文件顶层的 90 条事件绑定收进各文件的 `bindXxx()` 函数,由 `app-boot.js` 按顺序调用。到那时 §4 的第 1 类风险就从结构上消失了。
4. **`startInstall` 挪到更新提示旁边**,解除「快捷键 / 更新 / 排查日志必须同文件」的约束。
5. 每做完一项,把分组预算按实际合计下调。

---

## 9. 需要拍板

1. **方案 A**(多个普通脚本、按原顺序连续切片),不做 ES modules / 打包 / 命名空间改造。
2. **14 个文件的切分与命名**(§3.1);不保留 `app.js` 这个文件名。
3. **分组预算**:`app-*.js` 合计 ≤ 9014,只许下调。
4. **分两次提交**:1a 测试改造(产品代码不动)→ 1b 搬家(只搬不改)。
5. **冻结窗口**:1b 当天其它对话不要改 `app/src/app.js`,否则会产生大面积合并冲突。

`AGENTS.md` 已于 2026-11-22 精简(约 2.8 KB 余量),1a / 1b 登记新门禁与改措辞的空间够用。
