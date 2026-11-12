# PC 端代码质量清理与经验沉淀(2026-11-12 · 第二轮)

> 触发:用户要求「清理和优化屎山代码和低质量代码防止项目变得难以维护以至于变废,
> 并总结经验教训 —— 哪些原因导致、哪些验证不缜密,写进经验里防止以后的 agent 重犯」。
> 与上午那一轮的分工:上午(`code-governance-2026-11-12.md`)治理**版本库 / 门禁 / 文档 / 产物边界**,
> 本轮治理**代码本身的失效方式**,并把两条最容易回潮的纪律做成机器判定。
> 基线:`2e04eca` / v0.3.36。本轮结束时核心门禁:core **431 passed / 8 ignored**。

---

## 一、结论先行

1. **「屎山」的主因不是某段代码写错,而是"失败路径从来没人验"**。本轮修掉的 4 处
   静默失败,每一处都能让界面显示 A、磁盘里是 B:置顶、窗口位置、跳过版本、凭据指纹。
   用户看到的现象分别是"记不住位置""跳过没用""又弹同一个版本"—— 全都查不到日志。
2. **纪律写了三版都没生效,因为它是"文档",不是"门禁"**。`AGENTS.md` 早已写明
   「不许吞错」,`app.js` 里也有同样的注释,而实际仍有 15 处空 catch。
   本轮把这条规则变成 `check-silent-failures.js`:**空 catch 必须写 `silent: <理由>`**,
   写不出理由就说明它不该被吞。
3. **同一个原因也解释了文件膨胀**:「新增功能优先拆模块」写了三版,
   `app.js` 依然从 9141 长到 9176。本轮加 `check-file-size.js`,把上限变成**只许下调**的棘轮。
4. **门禁自己会假红,而且假红比不设门禁更危险**。本轮新门禁第一版把 115 处
   **有处理逻辑**的 catch 全判成空块(原因:JS 的 `RegExp` 匹配结果没有 `.end` 属性,
   照 .NET 的写法取偏移得到 `NaN`)。若照报错去改,会给一百多处 catch 加噪音提示。
   所以两个新门禁都自带 `--self-test`,并且自检本身也覆盖了"注释里的示例代码不算代码"。

---

## 二、本轮改了什么(逐项有证据)

### 2.1 前端静默失败 → 回滚 + 用户可见提示

| 位置 | 原行为 | 现在的行为 | 不改会怎样 |
|---|---|---|---|
| `app.js` `dockExitNow()` 末尾 | `invoke("set_pin", …).catch(() => {})` | `try/await` 失败 `showToast("恢复置顶失败:…")` | 按钮显示"已置顶",窗口实际没置顶,重启才对上 |
| `app.js` `scheduleSettleCheck()` | `invoke("save_window_pos", …).catch(() => {})`,内存坐标已改 | 失败**回滚** `CFG.winX/winY` + 一次会话只提示一次 | 界面记住了、磁盘没有,下次启动跳回旧位置 |
| `app.js` `#btnUpdSkip` | 先清 UI 再发请求,`.catch(() => {})` | 先 `await` 落盘,失败保留 UI + `showToast` | 界面说"已跳过",重启又弹同一版本,按钮显得没用 |
| `app.js` `dockExitNow()` 的 `console.error` | 只打控制台 | 改 `showToast` | GUI 从托盘启动时根本没有控制台(AGENTS.md 2.1.6) |
| `fetch.rs` `note_qoder_scope()` | 两处 `let _ = store.set_setting(…)` | 失败进 `diaglog::write_keyed` | "是否换了账号"的判据写不进去,下一轮仍按旧账处理,且无迹可查 |

**为什么不用「统一加 toast」了事**:拖拽保存会反复触发,逐次弹提示反而盖住真问题,
所以窗口位置那处是"回滚 + 一次提示";`loadCheckin` / `previewTick` / `localStorage`
那类属于"读失败就降级、没有用户需要知道的状态",保留吞掉但必须写明 `silent: 理由`。

### 2.2 空 catch 从 15 处降到 0 处"未认领"

`check-silent-failures.js` 扫到 71 处 catch,其中 15 处块内无语句。处置:

| 类型 | 处数 | 处置 |
|---|---|---|
| IPC 写操作 | 3 | 改成回滚 + `showToast`(见 2.1) |
| 视觉/降级路径 | 12 | 补 `/* silent: <理由> */`,理由必须 ≥6 字 |

12 处分别属于:焦点恢复、金库列表读失败、OAuth 取消、复制兜底、预览线、
localStorage 三处、首页视图降级、签到读失败、版本号/启动错误读取、真实置顶态读取。
每一条都在代码里写清了"为什么可以不知道"。

### 2.3 旧版摘要 CSS:删掉 8 条**必然不生效**的声明

`styles.css` 132-170 是旧版三格摘要,下方「P1/P2 · 折叠形态」段有同名选择器。
按属性集逐条比对后:

- **删除**(全部属性都被同名选择器覆盖,零视觉变化):`.risk-cell`、`.risk-cell.on`、
  `.hero-unit`、`.due-name`、`.sb-row`、`.sb-tx`、`.expbars`、`.expbars i`;
- **保留**(有下方未声明的独有属性):`.vv` 的 `overflow/text-overflow`、
  `.hero-v` 的 `font-variant-numeric/white-space`、`.due-date` 的 `margin-top`、
  `:hover` / `:focus-visible` 两个交互态、`.expbars.is-empty`、`.vv b`。

CSS 规则数 1052 → **1044**,形态门仍 PASS。
**未做真机视觉核对**,依据是"同选择器同特异性、后者胜出"的属性级静态证明 ——
这一点如实记在这里,不冒充"已肉眼确认"。

### 2.4 两条新门槛(带自检,已进 CI)

| 门禁 | 判定 | 自检覆盖 |
|---|---|---|
| `build/check-silent-failures.js` | 空 catch 必须带 `silent: <理由>`(≥6 字);注释与字符串用状态机屏蔽,不误判 | 未标记判红 / 理由过短判红 / 已认领判绿 / 有处理逻辑判绿 / 嵌套 try 不误判 |
| `build/check-file-size.js` | 10 个超标文件的行数预算,超出即失败;**上限只许下调** | 超限判红 / 等于上限判绿 / 低于上限判绿 |

规模预算现状(`上限 = 当前 + 约 1%`):

```
app/src/app.js            9176 / 9260
app/core/src/inject.rs    7248 / 7320
app/src-tauri/src/lib.rs  6432 / 6500
app/core/src/fetch.rs     5198 / 5250
app/src/styles.css        1844 / 1860
```

---

## 三、根因:为什么"自动化改代码"会把项目推向难维护

> 下面每条都对应本仓库的具体证据,不是通用劝告。可执行版本在 `AGENTS.md` 第 6、7 节。

### 3.1 改动是增量的,验证是抽样的

每个 agent 只验证自己改动的那一小块:跑一次 `node --check`、看一眼自己的截图。
没有人验证"整体还是不是同一个系统"。于是同一件事出现第二份实现
(`SyncStatus` / `GithubStatus`)、同名 CSS 段互相覆盖、配置项只剩读没有写
(`compact_max_w` 半死状态持续了十几个版本)。

**症状**:`git log` 里每个提交都"验证通过",但系统的可理解性在稳定下降。

### 3.2 没有共享记忆,每个 agent 从零开始

仓库里并存 `.workbuddy/`、`.workbuddy-ai/`、`.zcode/`、`.trae-*`、`.dsh-*`
五套工作目录,全部 gitignore、互不读取。A agent 得出的"这个不能删"B agent 看不到,
于是重复审计、重复实现、甚至把同一份冗余清单又生成一遍。
上午那一轮的产出就是把结论从私有 memory 搬到受版本控制的 `AGENTS.md`。

### 3.3 "全绿"可以完全由宽松的桩制造

`jsdom-smoke.js` 对未登记命令返回 `null`、窗口对象 Proxy 恒成功。
命令名拼错、Tauri 命令漏注册,182 项冒烟照绿。
**mock 的默认行为是"成功"时,测试通过只证明"代码没崩",不证明"代码对"。**

### 3.4 验证脚本会腐烂,而腐烂是静默的

`verify-codex-layout.js` 引用的 `.quota-number` 类在改版后已不存在,
脚本一跑就 TypeError —— 但它不在 CI 里,没人知道,文档还写着它通过过。
一次性的人工核对结论会**随代码变更过期**,而过期的结论比没有结论更误导。

### 3.5 门禁本身没有验证机制

本轮两次假红(详见 `AGENTS.md` 6.5),上午还有一次(IPC 门禁把已注册的
`key_source` 报成未注册)。三次的共同点:**报错的是门禁,不是代码**。
没有 `--self-test` 的门禁,只能用"当前代码恰好绿"证明自己,
而"当前代码恰好绿"同样可以由"门禁什么都没检查"造成。

### 3.6 删除从来不被当作风险操作

加代码有测试、有截图、有验证;删代码只有 `grep` 零引用。
但 `pub` 符号、Tauri 命令注册、`#[cfg(test)]`、CSS 级联、跨端协议码表
**都是 grep 看不见的引用**。上午删 `DESKTOP_READER` 是对的(删掉注入函数后它恒为 `None`,
属恒假死分支);但同样的手法用在活代码上就是事故。

### 3.7 高权限能力逐次叠加,没有一次性知情

读别家桌面端凭据、写别家客户端配置、默认开启每日签到、监听局域网的本地网关 ——
每一步单独看都"为了方便",累积起来是一份需要逐项同意的清单。
现在有 `AGENTS.md` 2.4 与第 5 节的裁决记录(签到保持默认开 / 网关保留 / 备份必须加密),
后续 agent 不得反转。

### 3.8 文档纯追加、数字写死

`PROGRESS.md` 335 KB 只往末尾加,头部状态停在 14 个版本之前;README 写"227 个测试"、
CI 注释写"422 tests",实测 431。**在文档里写死数字,每加一条测试就制造一次谎言。**
本轮所有文档一律改为"以命令输出为准"。

---

## 四、验证不缜密的七种具体形态

压缩版见 `AGENTS.md` 6.1–6.7,这里给判定口径:

| 形态 | 判定问句 | 本仓库案例 |
|---|---|---|
| 只测成功路径 | 失败时用户看到什么? | `saveCfg` 回滚是后补的;用户实测"恢复不动" |
| 桩默认成功 | mock 对未知输入返回什么? | jsdom 桩 `return null` |
| 只测存在不测时序 | 顺序变了 / 两个请求同时到会怎样? | 顶栏顺序断言;`loadCheckin` 乱序 |
| 验证脚本腐烂 | 它引用的选择器/命令还在吗? | `.quota-number` 已不存在 |
| 门禁报错就改代码 | 先证伪门禁自己了吗? | 三次假红 |
| 门禁没有自检 | 它能证明自己会红吗? | 新门禁全部带 `--self-test` |
| 在错误的机器/并行度取结论 | 换个环境还成立吗? | `STATUS_ENTRYPOINT_NOT_FOUND`;并行假红 |

---

## 五、回归门禁结果(本轮实测,串行)

| 门禁 | 结果 |
|---|---|
| `core cargo test --locked`(GNU 工具链) | **431 passed / 0 failed / 8 ignored**;fixtures 6 passed |
| `core cargo check --all-targets --locked` | EXIT=0,零警告 |
| `src-tauri cargo check --all-targets --locked` | EXIT=0 |
| `node build/check-ipc-contract.js` | PASS,112 处 invoke / 90 命令 / 90 定义 / 90 注册 |
| `node build/check-silent-failures.js --self-test` | 5/5(含"未标记必须判红") |
| `node build/check-silent-failures.js` | PASS,71 处 catch 全部已处理或已认领 |
| `node build/check-file-size.js --self-test` | 3/3 |
| `node build/check-file-size.js` | PASS,10 个预算文件全部在上限内 |
| `node build/check-css-forms.js` | PASS,7 容器 / **1044** 条规则 |
| `node build/jsdom-smoke.js` | **182/182** |
| `node --check`(app.js + 两个新门禁) | OK |

> `src-tauri cargo test` 在本机仍 `STATUS_ENTRYPOINT_NOT_FOUND`(WebView2 静态加载器链栈,
> `PROGRESS.md` 1464 / 4340 行有记录),**未跑通,不冒充通过**;CI 用 `cargo check` 覆盖编译期。

---

## 六、遗留(下一轮该做的,不许假装不存在)

1. **`app.js` 9176 行、`inject.rs` 7248 行仍未拆分**。本轮只做到"不许再长",
   真正的拆分(把渲染 / 事件绑定 / 网络层切开)需要配套测试,不能在发版周做。
2. **CSS 旧摘要段的剩余规则**只做了静态证明,没有真机视觉核对。
3. **`fetch.rs` 生产代码仍有 42 处 `.unwrap()`**,绝大多数是 `.lock().unwrap()`
   (Mutex 中毒即 panic)。对常驻 GUI 不是理想形态,但改动面覆盖整个取数层,
   需要单独一轮 + 故障注入测试。
4. **CDP 真机脚本**(`verify-checkin-center` / `verify-settings-entry` / `verify-resize` /
   `verify-qoder-expiry` / `verify-ui-0923` / `verify-focus-ring`)本环境无运行中会话,未跑;
   它们覆盖的入口不在本轮改动面内。
