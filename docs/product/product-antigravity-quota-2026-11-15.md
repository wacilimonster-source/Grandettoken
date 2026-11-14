# 产品设计：Antigravity 配额监测渠道

- **日期**：2026-11-15（同日终稿定版，并完成 **M0 实机输出样本核验通过**）
- **状态**：M0 验证通过，待启动 P0/Android 代码实现（尚未改动应用代码）
- **版本基线**：`v0.3.50`（仓库当前仅含 GCP 账单赠金渠道 `google-cloud`，尚无 Antigravity 渠道）
- **关联文档与基线模块**：
  - [DESIGN.md](../../DESIGN.md)（全产品交互规范、四形态尺寸与红线约束）
  - [AGENTS.md](../../AGENTS.md)（自动化改代码、规模棘轮、验证纪律与安全边界）
  - [app/README.md](../../app/README.md)（渠道架构、轮询策略与门禁说明）
  - [docs/product/product-codex-weekly-reset-2026-11-03.md](./product-codex-weekly-reset-2026-11-03.md)（固定主窗 + 副窗风控的多窗口百分比渠道参照）
  - [docs/product/product-quota-recovery-2026-11-06.md](./product-quota-recovery-2026-11-06.md)（旧快照跨恢复点状态机参照）
  - 外部开源架构调研参照：`steipete/CodexBar`（`docs/antigravity.md`、`AntigravityStatusProbe.swift`、`AntigravityCLISession.swift`）
  - 核心代码参照：`app/core/src/providers.rs`、`app/core/src/view.rs`、`app/core/src/fetch.rs`、`app/core/src/google_cloud.rs`、`app/core/src/sync/model.rs`、`app/src/app.js`、`app/src/logos/README.md`、`android/app/src/main/java/com/wacil/grandettoken/sync/ChannelPresentation.kt`、`android/app/src/main/java/com/wacil/grandettoken/sync/WidgetConfigActivity.kt`

---

## 0. 一句话定位与已拍板决策总览

为看盘工具新增 **`antigravity`** 渠道（展示名 **`Antigravity`**，短代码 **`AG`**，类型 **`Kind::Percent`**），通过调用用户本机已登录的 **Google 官方 `agy` CLI 非交互结构化命令（`agy -p /usage --output-format json`）**，安全只读地监测 Antigravity 账号下各模型分组（`Gemini`、`Claude / GPT` 等）的 **5 小时 / 周上限剩余百分比与精确恢复时间**。

### 五项决策定稿清单

| 决策项 | 最终拍板结论 |
|---|---|
| **1. 鉴权与取数路线（安全优先）** | **仅走 Google 官方 `agy` CLI（`agy -p /usage --output-format json`）**。不开发独立 OAuth、不抽取/冒充第一方 `client_id`、不直连私有 `cloudcode-pa` 接口、不读外部客户端 `state.vscdb` 数据库、不探测 `language_server` 随机端口 |
| **2. 主数字计算口径** | **B：固定核心主池 + 副行与红线通知盯紧最紧限流池**（参照 `Codex` 在 `view.rs` 的双口径分治机制） |
| **3. AI Credits 超额积分** | **一期不显示**（仅展示模型配额分组百分比与恢复时间） |
| **4. Android 端适配排期** | **P0 同步交付**（同步详情展示 + `WidgetConfigActivity.kt` 分池小组件多选） |
| **5. 渠道图标** | **采用官网真实透明图标**：`https://antigravity.google/assets/image/antigravity-logo.png`（实测 `276×276` RGBA 透明底，双端缩至 `256×256` 同源入库） |

### 与既有 `google-cloud` 渠道的严格隔离

| 维度 | 既有 `google-cloud`（Gemini · Google Cloud） | 新增 `antigravity`（Antigravity） |
|---|---|---|
| 监测对象 | GCP 结算账户下的赠金 / 试用额度账本 | Google Antigravity 订阅各模型组配额池（5h / 周上限） |
| 数据类型 | `Kind::Amount`（美元 / 人民币金额） | `Kind::Percent`（`0%–100%` 剩余百分比 + 重置时间） |
| 首页汇总参与 | 计入「总可用金额」与「7 天内临期金额」 | **不计入**金额总池与临期金额；仅参与配额红线告警 |
| 取数与凭据方式 | WebView2 登录 GCP Console 抓取页面 | 调用本机官方 `agy` CLI 只读命令行输出（本应用不保存 Google 凭据） |
| 渠道 ID | `google-cloud` | `antigravity` |

---

## 1. 用户场景、核心目标与明确不做

### 1.1 目标用户与典型痛点
1. 同时使用多款 AI 编程订阅（Codex、Claude、OpenCode Go、Antigravity 等）的开发者，希望在一个悬浮看盘里并排盯住 Antigravity 主力组配额余量，并在其它组限流时收到直观提醒。
2. 相比第三方插件抽取 IDE token 或在后台跑非官方 OAuth 触发 Google ToS 403/封号，用户明确要求**只用安全合规路径**：凭据与认证续期完全交给 Google 官方 `agy` CLI 自身管理，看盘只读取其官方暴露的 JSON 用量报告。
3. 支持在 PC 桌面（主面板、详情弹层、紧凑条、悬浮胶囊、托盘）与 Android 同步端/小组件查看分池进度和精准恢复时间。

### 1.2 明确不做（Non-goals）
1. **不做对话推理入口、不代理模型调用**（守住 `DESIGN.md` 第 1 节）。
2. **不自动切号、不启动交互式登录弹窗、不解析 CLI TUI 屏幕字符**：只认结构化 `--output-format json` 输出；未登录则如实报错提示用户在终端运行 `agy` 登录。
3. **不进入「模型可用性/配置源」（`MODEL_SOURCES`）**：`antigravity` 是订阅配额看盘，不是 API 中转密钥源。
4. **不写回、不读取、不解密其它客户端文件（`state.vscdb` / `oauth_creds.json`），也不探测本地 `language_server` 端口**。
5. **一期不读取也不显示 AI Credits，也不虚构非官方美元估值**。

---

## 2. 技术路线与安全防护设计（基于 `agy` CLI 官方命令）

### 2.1 可执行文件发现与用户自定义路径
1. **自动发现顺序**（实测官方 Windows 安装器 `install.ps1` 将 CLI 落盘到 `%LOCALAPPDATA%\agy\bin\agy.exe`，且不会自动注入已打开进程的旧 `PATH`）：
   - 用户在「设置 → 渠道管理 → Antigravity」显式填写的 `agy` 可执行文件路径（为空时走自动探测，对应配置字段 `antigravityCliPath`）；
   - Windows 默认安装位置 `%LOCALAPPDATA%\agy\bin\agy.exe`（优先覆盖未重启继承新 `PATH` 的 GUI 场景）；
   - 系统 `PATH` 中的 `agy` / `agy.exe` / `agy.cmd`，及兼容兜底 `%USERPROFILE%\.local\bin\agy.exe`。
2. **版本门槛校验**：
   - 官方从 `agy 1.1.11` 起支持非交互式用量输出（本机实测当前版本 `1.5.9`）；首次发现或可执行文件变更（路径 / 文件大小 / 修改时间变化）时执行 `agy --version` 缓存校验，低于要求版本时提示升级 `agy`，不盲目运行。

### 2.2 子进程安全隔离规范（借鉴 CodexBar 实战经验 + Windows M0 实测）

每次轮询或手动刷新调用：`agy -p /usage --output-format json --print-timeout 25s`（注意 **不可传** `--disable-slash-commands`，否则 `/usage` 斜杠命令失效），必须全部满足以下保护约束：

1. **禁止 Windows 控制台黑框闪烁**：创建子进程必须设置 Windows `CREATE_NO_WINDOW`（`0x08000000`），且独立无控制台窗口，不打扰用户工作。
2. **禁止后台自动更新（关键）**：子进程环境显式注入 **`AGY_CLI_DISABLE_AUTO_UPDATE=true`**（小写 `true`；`CodexBar` 实测大写/数字 `1` 无效），防止看盘静默触发 CLI 自动更新或占用文件锁。
3. **禁止未登录时悄悄拉起浏览器**：在私有隔离临时空目录中运行子进程，屏蔽/阻断 `BROWSER` 环境变量与非交互弹窗行为；若 `agy` 返回未登录或需要认证，立刻终止并返回明确引导「`本机 agy CLI 尚未登录，请在终端运行 agy 完成一次登录`」。
4. **单飞锁 + 超时 + 进程树清理 + 输出上限**：
   - 全渠道共享 Antigravity 单飞执行锁（防止手动点击与后台定时器并发拉起多个 `agy` 进程）；
   - 超时时间建议 **30–45 秒**（CodexBar 上限 90s，Windows 日常冷启动数秒内应结束；超时立即终止子进程树，避免挂死 MCP/子进程残留）；
   - Stdout 读取上限 `1 MiB`，超限直接中止并报错；
   - **日志脱敏**：`diag.log` 仅记录命令退出码、耗时、解析出的窗口数量与分类错误原因，**不落盘完整原始 stderr/敏感用户路径或邮箱**。
5. **适配 CLI 冷启动的合理轮询节奏**：
   - 因每次拉取需启动一次本地 `agy` 子进程（耗时数秒），在既有轮询调度之上增加 Antigravity 最小刷新冷却期（**主面板展开时建议 ≥ 2 分钟、折叠/托盘 ≥ 5–10 分钟、3 连败退避 15 分钟**；点击「立即刷新」可绕过自动冷却）。
   - **隐藏即零拉起**：渠道隐藏后绝不探测也不拉起 `agy` 进程。

---

## 3. 配额领域模型与「主池 / 限流副池」口径（已按决策 2B 定稿）

### 3.1 真实数据结构与映射规则（已按 M0 实机 `agy 1.5.9` JSON 契约对齐 ✅）

实测 `agy -p /usage --output-format json` 返回顶层 `status: "SUCCESS"` 及 `command.name: "usage"`，核心数据在 **`command.data.groups[]`**（每组含 `name`、`description`、`buckets[]`），每个桶字段为 snake_case：**`id`、`name`、`window`（如 `"weekly"` / `"5h"`）、`remaining_fraction`（`0.0`–`1.0`）、`reset_time`（RFC3339 UTC）、`description`**：
1. **实测分组与多窗口覆盖**：
   - **`Gemini Models`** 组：`gemini-weekly`（`window: "weekly"`）、`gemini-5h`（`window: "5h"`）；
   - **`Claude and GPT models`** 组：`3p-weekly`（`window: "weekly"`）、`3p-5h`（`window: "5h"`）；
   - **独立特殊模型组**（如实测到的 **`Gemini 4 Argon`** 组）：`gemini-argon-wtu-weekly` 等动态出现的独立组，必须**原样保留为独立 `Window`**（例如显示 `Gemini 4 Argon 周`），**绝不并入或丢弃**；
   - 且 `groups[].description` 自带成员模型列表（如 `"Models within this group: Gemini 4 Hydrogen, Gemini 3.8 Flash"`），可直接用于详情页副文案展示！
   - **Starter / 无 5h 档兼容性**：若某组仅返回周限桶而无 5 小时桶，**按实渲染周限行，绝不凭空捏造 5 小时条**。
2. **防失真铁律**：
   - 正常百分比：`remain_percent = (remaining_fraction * 100.0).clamp(0.0, 100.0)`，`percent = 100.0 - remain_percent`。
   - **未启用 / 未知桶不得伪造成 `0%`**：当某个桶被禁用或缺少 `remaining_fraction`（`null`/缺失）时，不能渲染成 `0% 已耗尽`；仅在明确 `remaining_fraction == 0.0` 且可用时标为 `status = "rate-limited"`。
   - **稳定 `Window.key` 命名**：直接采用稳定的后端 `bucket.id`（`gemini-weekly`、`gemini-5h`、`3p-weekly`、`3p-5h`、`gemini-argon-wtu-weekly` 等），便于 Android 小组件长期固定绑定不漂移。
   - **身份/套餐诚实呈现**：实测 JSON 不返回邮箱与套餐名，**不编造也不显示假邮箱或假 `Free/Pro` 徽章**；仅使用 `agy CLI` 来源标识及各组/桶实时描述。

### 3.2 主窗口与副池告警口径（决策 2B，严格对齐 `Codex` 在 `view.rs` 的规则）

1. **固定主窗标记（`w.main = true`）**：
   - 主力核心窗固定优先级：**`Gemini 周限窗（gemini-weekly）`** → 若无周限（或账号无该组）则回退 **`Gemini 5 小时窗（gemini-5h）`** → 再回退首个有效已知窗口（与 `Codex` 以「本周」为主窗、没有周窗时落到主可用窗的逻辑完全一致）。
2. **`app/core/src/view.rs` 接线逻辑**：
   - **`headline_of(v, state)`**：把 `antigravity` 并入与 `codex` 同类的主窗选择分支（优先读 `w.main`，找不到再退到 `binding_window(v)`）。
     - 结果：PC 主面板大字、详情 Hero 大字、紧凑条主数字、系统托盘叠加数字**全部稳定显示主力 `Gemini` 核心窗余量**。
   - **`notification_window(v)`**：与 `codex` 同类，跟踪 **`binding_window(v)`（实际最紧缺/已限流窗口）**。
     - 结果：哪怕 `Gemini 周限` 还有 `82%`，只要 `Claude / GPT 5 小时` 或 `Gemini 5 小时` 跌破红线或限流归零，红线通知照常以紧缺窗为主体发出。
   - **`Tone`、`ratio(v)`、`is_limited(v)`、`secondary_of(v)`**：全部沿用 `binding_window(v)`！
     - 结果：当副窗先限流时，主数字保持主力窗余量不乱跳，同时**状态轨变红告警 + 副行直接报出限流窗口名称与恢复倒计时**（如 `Claude/GPT 5h 窗 · 38m 后恢复 · Gemini 周 82%`），完全不漏告警。

---

## 4. 全形态交互与信息展示设计

### 4.1 真实官方图标（已定位核验 ✅）
- 官网透明 PNG 源：`https://antigravity.google/assets/image/antigravity-logo.png`（实测 `276×276` 32bpp RGBA 透明底彩色 “A” 标，角像素 `A=0`）。
- 缩至标准 `256×256` RGBA 入库（遵循 [app/src/logos/README.md](../../app/src/logos/README.md) 本地内嵌 CSP 要求，且避免 `/favicon.svg` 内嵌 10 组 `<feGaussianBlur>` 在 Android 端不兼容）：
  - 桌面端：`app/src/logos/antigravity.png`（`app.js::LOGOS` 注册，`shortLabel` 为 `"AG"`）；
  - Android 端：`android/app/src/main/res/drawable-nodpi/channel_antigravity.png`。

### 4.2 主面板折叠行（Panel Row）

| 状态场景 | 左侧主标题 / 副标题 | 右侧主大字（固定主窗） | 右侧副行（优先告警限流/紧缺窗） | 状态轨 |
|---|---|---|---|---|
| **各窗正常（Gemini 周 82%、5h 100%，Claude 5h 45%）** | **Antigravity**<br>`agy CLI · 正常` | **`82%`** | `Gemini 5h 100% · 最紧 Claude 5h 45%`（窄屏简化为 `最紧 Claude 5h 45%`） | 正常色（若最紧窗 `<=20%` 转琥珀） |
| **副窗限流（Gemini 周 82%，Claude 5h 0%）** | **Antigravity**<br>`agy CLI · Claude/GPT 已限流` | **`82%`** | `Claude/GPT 5h · 38m 后恢复 · Gemini 周 82%` | 红色限流高亮态 |
| **主窗限流（Gemini 周 0%，Claude 90%）** | **Antigravity**<br>`agy CLI · Gemini 周已限流` | **`0%`** | `Gemini 周 · 2d 4h 后恢复 · Claude 90%` | 红色限流高亮态 |
| **全窗耗尽** | **Antigravity**<br>`agy CLI · 配额已达上限` | **`0%`** | `最早 Gemini 5h · 1h 10m 后恢复` | 红色限流高亮态 |
| **未检测到 `agy` CLI（`noKey`）** | **Antigravity**<br>`未检测到 agy CLI` | **`——`** | `点此在设置中安装或指定路径` | 灰色置灰态 |
| **`agy` 未登录 / 登录失效（`auth`）** | **Antigravity**<br>`需在终端登录 agy` | **`——`**（无缓存时）或 **旧值（标过期）** | `运行 agy 登录后点击刷新` | 琥珀/红提示态 |
| **跨过重置点旧快照（`stale`）** | **Antigravity**<br>`agy CLI · 等待刷新` | 上次主窗百分比 | `上一周期数据 · 等待更新` | 琥珀过期态 |
| **CLI 调用失败（有未过期缓存）** | **Antigravity**<br>`agy CLI · 拉取失败，显示缓存` | 上次缓存百分比（**严禁降成 `0%`**） | `12m 前更新 · 重试失败` | 琥珀提示态 |

### 4.3 详情弹层（Detail Sheet）
1. **顶部 Hero 区**：官方图标 + `Antigravity` + 来源标签（`agy CLI`；仅当 JSON 明确带出档位/账号信息时才显示徽章）+ 主窗剩余大数字 + 任意限流窗警示徽章 + 刷新按钮与最后更新时间（支持分钟级本地 `resets_at` 倒计时重算）。
2. **模型组配额列表（唯一内容区，无 AI Credits 区块）**：
   - 按 `Gemini`（周 / 5h）、`Claude / GPT`（周 / 5h）及任何新增独立组展示紧凑条卡；
   - 每卡显示：组别与周期名 + 剩余百分比（`82% 剩余` / `0% 已耗尽` / `暂不可用`）+ 剩余比例进度条 + 精确本地重置时间（`今日 16:40 恢复（2h 15m 后）` 或 `MM/DD HH:mm`）。
3. **底部状态说明**：标明数据来自 `本机 agy CLI`；跨 `resets_at` 尚未取得新周期成功回包时明确显示「`当前为上一周期快照（原重置时间 HH:mm 已过），等待服务端更新`」。

### 4.4 紧凑条、胶囊、托盘与红线通知
- **紧凑条（Compact Bar）**：L0 `[图标] 82%` / L1 `[图标] AG 82%`（有副窗限流：`AG 82% · 1限`）/ L2 `[图标] AG 82% (Claude 限)`，限流时文字/指示点同步变红。
- **悬浮胶囊（Pill）**：参与全局评分排序时采用 `binding_window` 分数，不被高余量主窗遮盖限流危机。
- **托盘图标 & Tooltip**：托盘数字绘制主窗百分比；Tooltip 单行摘要：`Antigravity · Gemini 周 82% · Gemini 5h 100% · Claude/GPT 5h 0% (38m 后恢复)`。
- **红线通知**：基于 `notification_window`（绑定 `binding_window`），任一组/周期首次跌破红线即提醒，按 `(window_key, resets_at)` 周期去重。

### 4.5 设置 → 渠道管理
- **隐藏停更**：隐藏后立即停止 `agy` 探测与后台轮询。
- **配置与操作区**（遵循 `DESIGN.md` `WHY` 字数预算）：
  - `L1 <= 30` 字简介：`调用本机官方 agy CLI 读取 Antigravity 配额，不保存账号密钥`。
  - 可选输入框：**`agy` 可执行文件路径（留空则自动探测 PATH）** + 「立即探测 / 测试连接」按钮 + 一句 CLI 安装与登录指引（提示运行官方安装脚本及 `agy` 登录即可，无多余弹窗）。
  - `khint <= 120` 字安全提示：`全程复用本机官方 agy CLI 的登录态输出用量 JSON，看盘本身不读取第三方数据库，也不存储 Google 令牌。`
  - 若 `agy` 路径保存或写配置失败，严格按 `saveCfg()` 范式**回滚输入框状态 + `showToast` 提示**，绝不静默吞错。

### 4.6 Android 端同步与分池小组件（P0 同步交付）
1. 同步协议复用 `ChannelKind::Quota` + `WindowSnapshot`（不含本地路径等无关系统信息，`is_builtin` 兼容 `antigravity`）。
2. `ChannelPresentation.kt` 注册 `antigravity` 品牌名、主题色与 `drawable-nodpi/channel_antigravity.png`，详情弹窗完整渲染所有 `WindowSnapshot` 组/窗。
3. `WidgetConfigActivity.kt` 为 `antigravity` 同步开放**与 `codex` 同级的多窗口选择器**：
   - 默认项：`主力主窗`（跟随 `w.main`）；
   - 分池项：动态读取最新同步快照中的各个稳定 `windowKey`（如 `Gemini 周`、`Gemini 5h`、`Claude/GPT 周`、`Claude/GPT 5h`），用户可在桌面上单独添加多个小组件分别盯不同模型组；旧快照缺少对应 key 时给出明确缺失状态而不是伪造 `0%`。

---

## 5. 异常态、失败降级与恢复策略

| 异常情形 | 识别条件 | 展示口径 | 后台行为 |
|---|---|---|---|
| **未安装 `agy` CLI / 路径无效** | `PATH` 与自定义路径均找不到可执行文件 | `ViewState::NoKey`（未检测到 `agy` CLI），展示设置与安装指引 | 不高频反复扫盘，仅在打开设置/手动触发/配置变化时重试 |
| **`agy` 版本过旧（`< 1.1.11`）** | `agy --version` 不支持非交互 JSON | `ViewState::Error`：「`agy 版本过低，请升级至 1.1.11+`」 | 保留旧缓存（如有），待下次升级或手动重试 |
| **`agy` 未登录 / 认证超时** | 进程输出登录提示或报错未认证 | `ViewState::Auth`（需在终端运行 `agy` 完成一次登录） | 立即终止并销毁子进程，避免拉起浏览器；停止无意义重试 |
| **进程超时 / 非 0 退出 / JSON 结构变更** | 超 30–45s、解析失败或无有效已知窗口 | 保留未过期缓存并标「缓存 · 拉取失败」；无缓存则 `Error` | 清理子进程树，进入 3 连败 15 分钟退避，脱敏记录退出码 |
| **旧快照跨过 `resets_at`** | `now > w.resets_at` 且尚无新周期成功输出 | 显示「上一周期数据，等待更新」 | 跨过重置点后在面板展开或下轮心跳时刷新一次 |

---

## 6. 模块拆分与门禁计划（严格受 `check-file-size.js` 棘轮约束）

现有大文件预算：`app.js` `9220`、`fetch.rs` `5280`、`src-tauri/lib.rs` `6484`、`providers.rs` `2095`（严禁把新业务逻辑堆进大文件）：

1. **Rust Core 新增独立模块 `app/core/src/antigravity.rs`**：
   - `agy` 路径解析与 `--version` 缓存；
   - `agy -p /usage --output-format json` 安全执行器（`CREATE_NO_WINDOW`、`AGY_CLI_DISABLE_AUTO_UPDATE=true`、隔离工作目录、单飞锁、超时杀树、1 MiB 限流）；
   - JSON 解析、`groups/buckets` 到 `Window[]` 映射（含 `w.main` 规则、缺字段不降 0、Starter 无 5h 兼容）与完整单测（注入坏 JSON / 超限 / 未登录 / 禁用桶测试夹具）。
2. **Core 最小接线**：
   - `providers.rs`：注册 `antigravity` `ProviderDef`（`Kind::Percent`、`unstable: true`，不加入 `auto_hide_candidates`）；
   - `config.rs`：新增可选 `antigravity_cli_path: Option<String>`（空串归一化为 `None`）；
   - `fetch.rs`：轻量分派 `antigravity` 拉取、冷却期与占位状态；
   - `view.rs`：在 `headline_of` 与 `notification_window` 并入 `antigravity`（与 `codex` 同类行为）并补单测验证四形态一致。
3. **前端与资源**：
   - `app/src/logos/antigravity.png` + [app/src/logos/README.md](../../app/src/logos/README.md) 更新 + `app/src/app.js` 最小 `LOGOS` / 设置路径字段（若设置项复杂则拆至 `app/src/antigravity.js`）；
   - 验证 `build/check-ipc-contract.js`、`build/check-silent-failures.js`、`build/check-file-size.js`、`build/check-css-forms.js`、`build/jsdom-smoke.js`。
4. **Android 端 P0 同步**：
   - `ChannelPresentation.kt`、`WidgetConfigActivity.kt`、`drawable-nodpi/channel_antigravity.png` 及对应 Kotlin/UI 单测。

---

## 7. M0 前置实测结果（已实机通过 ✅）

已在本机安装并实测 Google 官方 `agy` CLI，结果全部归档如下（可直接作为 P0 单测夹具契约）：
1. **安装路径与版本**：官方 `install.ps1` 将可执行文件安装于 `%LOCALAPPDATA%\agy\bin\agy.exe`；`agy --version` 返回 **`1.5.9`**。
2. **结构化输出与耗时实测**：
   - 执行 `agy.exe -p /usage --output-format json --print-timeout 15s`（独立临时空目录 + `AGY_CLI_DISABLE_AUTO_UPDATE=true` + `BROWSER=NUL`）；
   - **结果**：退出码 `0`、stderr 为空、stdout 输出 `2754` 字节完整 JSON，两次实测耗时分别为 `8465 ms` 与 `6080 ms`，未触发浏览器弹窗；
   - **实测返回覆盖 3 个独立分组 / 5 个窗口**：`Gemini Models`（`gemini-weekly` `80.4%`、`gemini-5h` `32.1%`）、`Claude and GPT models`（`3p-weekly` `47.0%`、`3p-5h` `0%` 已限流）、`Gemini 4 Argon`（`gemini-argon-wtu-weekly` `30.7%`）。
3. **下一步**：可随时按第 6 节模块拆分进入 P0（Rust Core + Tauri + 前端 + 官方图标 + Android 分池小组件）开发与全量门禁验收。
