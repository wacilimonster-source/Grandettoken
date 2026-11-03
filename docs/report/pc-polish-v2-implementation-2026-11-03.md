# PC 视觉精修 V2 · 实施报告

日期：2026-11-03。依据 [设计交接规格](../design/design-pc-polish-v2-2026-11-03-dev-spec.md) 与
[可交互原型](../design/design-pc-polish-v2-2026-11-03.html) 实施。本文记录**已落地的正式应用改动**、
验证结果与遗留限制；截图使用合成数据，不包含真实余额或凭据。

## 1. 已实现

### 1.1 主题与视觉基础

- 深石墨 / 暖雾白两套语义令牌，统一映射到现有 `--bg*` / `--tx*` / `--ac` / 状态色；
  旧蓝主色替换为鼠尾草绿（主按钮、选中芯片、开关、焦点环）。
- 新增 `Config.theme: "dark" | "light"`，默认 `dark`，非法值在 `sanitize()` 回退；
  老配置缺字段可直接读取。
- 设置 → 显示新增主题分段控件：即时预览，走真实 `set_config`；失败由 `saveCfg()` 回滚
  配置并重新应用旧主题。浅色主题覆盖标题栏、卡片、摘要、详情、设置索引、胶囊、紧凑条。
- Codex 纯白官方标在浅色主题下反相；置灰态保留去色语义。

### 1.2 首页

- 顶部只保留标题栏与三格摘要，不再放「额度概览」页面头、筛选芯片、搜索框和同步标记 ——
  这些信息与标题栏重复，去掉后把高度让给渠道卡。临期筛选仍通过摘要第一格点击切换。
- 三格摘要：未来 **7 天** 已确认临期积分合计、最先到期（日期 + 相对时间 + 渠道/数量）、
  每日签到。7 日窗口抽成 `SUMMARY_EXPIRY_DAYS`，详情仍列全部记录。
- 渠道卡改为描边卡片：品牌行、主数字、底部状态轨（最近到期 / 重置 / 今日消耗 / 快照）。
- 列表平铺渲染（2026-11-03 用户裁决）：取消「需要处理 / 其余」分组与分组标题；
  排序完全由设置 → 显示的「渠道排序」决定，限流 / 失效不再被强制排到前排，
  胶囊自动轮播与吸附竖条的顺序也改用同一口径（后端 `HomeView.order` 不再驱动展示顺序）。
- 窗口只保留两个固定尺寸：**单列 480×820**（一列，4 个渠道一屏完整可见）与
  **双列 860×820**（两列，4 个渠道一屏 2×2）；底栏「单列 / 双列」与
  设置 → 显示可切换。自由拖动缩放、右下角手柄、透明边距缩放任层全部移除。
- 积分型卡片的最近到期常驻，不依赖告警；Qoder 无逐笔数据时展示账户级有效期。
- 空结果、全部隐藏、未配置、冷启动骨架、失败横幅等既有状态保留。
- 修复了 Chromium 网格项 `overflow:hidden` 导致卡片轨道塌陷的问题（`grid-auto-rows:max-content`）。

### 1.3 渠道详情弹层

- 单例底部弹层：标题栏与页脚之间遮罩，上缘圆角 16，头部固定、主体独立滚动。
- 点卡片或摘要「最先到期」打开同一个 dialog；`Esc` 先关弹层、不收起窗口；
  遮罩空白与关闭按钮均可关闭；Tab / Shift+Tab 限定在弹层内；关闭后焦点回到触发元素。
- 刷新推送只更新当前 ID 的内容并保留滚动位置；渠道被隐藏/移除时关闭并提示。
- 「刷新数据 / 重试连接」复用真实 `refresh()`；「设置与管理」关闭弹层并定位到该渠道设置卡。
- 金额型「最近变动」仍按需取台账；其余既有详情（7 日消耗、明细、签到、重置卡、峰谷、
  账户切换、缺口说明）全部保留可达。

### 1.4 积分到期组件（WorkBuddy / Trae / WorkBuddy AI 共用）

- 唯一组件 `pointsExpiryHtml()`：最近一笔到期卡（完整日期 + 本地时间 + 相对时间 + 该笔剩余）
  → 逐笔明细（默认 3 笔，可展开全部，已过期记录排在未到期之后并标注待刷新）
  → 完整性 / 口径脚注。
- 不使用 `id === "trae"` 分支；WorkBuddy AI 不误加国内版签到。
- 相对时间由 60 秒 ticker 原地更新；跨过到期时刻显示「已到期,待刷新」，不出现负数倒计时。
- 空态区分：`expiryComplete=false` → 未知；完整且仍有余额 → 没有已知到期日；
  余额为 0 → 当前没有剩余积分。

### 1.5 Qoder 账户有效期

- 后端新增结构化可选字段：

  ```ts
  accountExpiry?: { state: 'dated' | 'never' | 'unknown'; at: number | null }
  ```

  由 `extract_qoder` 从原始 `expiresAt` 归一化：毫秒/秒、RFC3339 字符串都认；
  9999 年哨兵 → `never`；非法值 → `unknown`；字段缺失保持 `None`。
- 详情顶部显示「账户额度到期」（精确时间或长期有效），逐笔另列未知；
  账户有效期与逐笔积分到期严格分开，不用账户时间推算每一笔。
- 老快照回退只对 Qoder 的 `extra[账户额度到期]` 生效，且只接受 `YYYY-MM-DD` / `不过期`，
  不补 `23:59`、不画小时倒计时。

### 1.6 设置

- 索引顺序调整为：渠道、模型、签到、显示、刷新与启动、密钥箱、同步、关于；
  外观为分组描边卡片，图标统一鼠尾草绿。
- 子页字段、表单、校验、密钥、同步、模型、签到、更新、退出入口全部保留。

## 2. 修改文件

| 文件 | 改动 |
|---|---|
| `app/core/src/providers.rs` | `AccountExpiry` / `AccountExpiryState`；`FetchResult.account_expiry`；Qoder 归一化 |
| `app/core/src/fetch.rs` | `ChannelView.account_expiry` 及全部构造点 |
| `app/core/src/view.rs` | 测试 dummy 补字段 |
| `app/core/src/config.rs` | `theme` 字段、默认值、sanitize、兼容性与回环测试 |
| `app/src/index.html` | 页面头、筛选工具条、主题控件、单例详情弹层、图标 |
| `app/src/styles.css` | V2 令牌、卡片、摘要、弹层、到期组件、设置索引、浅色覆盖 |
| `app/src/app.js` | 主题、筛选、摘要、卡片状态轨、详情弹层、到期组件、Qoder 回退、ticker |
| `app/README.md` | 积分到期契约与 PC V2 界面说明 |
| `build/jsdom-smoke.js` | 新增 Qoder 账户有效期 / 旧快照 / 主题用例 |
| `docs/assets/pc-polish-v2-implemented/` | 实施截图（合成数据） |

## 3. 验证

| 命令 / 场景 | 结果 |
|---|---|
| `node --check app/src/app.js` | 通过 |
| `node build/jsdom-smoke.js`（临时安装 jsdom） | **134 / 134 通过** |
| `node build/check-css-forms.js` | 通过（形态门 7 个容器） |
| `cargo test --lib`（`app/core`） | **407 passed / 0 failed / 8 ignored** |
| `cargo check`（`app/src-tauri`） | 通过 |
| 真机 CDP：打开/关闭、Esc、遮罩、焦点回归、Qoder 详情动作 | 全部通过 |
| 真机 CDP：单列 4 渠道一屏、双列 2×2 一屏 | 列表 `scrollHeight == clientHeight`，无滚动 |
| 真机 CDP：深色/浅色、面板/紧凑条/胶囊 | 文字、边框、状态色可读 |

真机验证使用本地调试构建（`cargo build` + WebView2 CDP）；注入 4 个合成渠道后，
单列列表 `scrollHeight == clientHeight`，双列同样无需滚动，均无需滚动。
没有改动用户配置（注入数据只在内存，结束后 `refresh()` 还原）。

## 4. 截图

- [首页 · 窄 · 深色](../assets/pc-polish-v2-implemented/home-narrow-dark.png)
- [首页 · 宽 · 暖白](../assets/pc-polish-v2-implemented/home-wide-light.png)
- [WorkBuddy 到期详情](../assets/pc-polish-v2-implemented/detail-workbuddy-dark.png)
- [Qoder 账户到期 + 逐笔未知](../assets/pc-polish-v2-implemented/detail-qoder-dark.png)
- [设置索引](../assets/pc-polish-v2-implemented/settings-index-dark.png)
- [设置 · 显示 / 主题](../assets/pc-polish-v2-implemented/settings-display-dark.png)
- [紧凑条 · 暖白](../assets/pc-polish-v2-implemented/compact-light.png)
- [胶囊 · 暖白](../assets/pc-polish-v2-implemented/pill-light.png)

## 5. 遗留与边界

- Qoder 真实接口本轮未再跑通（环境没有有效会话）；结构化字段由合成样本 + 单元测试覆盖，
  上线后应以真实 `expiresAt` 再确认一次。旧快照回退已按日期-only 处理。
- `accountExpiry` 是**只增不改**的可选字段；同步 / Android 消费端本轮未升级协议，
  旧客户端会忽略它，不影响现有字段。
- 详情动作区随主体滚动（规格允许「可固定」而非必须）。在最小高度下需滚动到主体底部操作。
- 未构建 NSIS 安装包；交付为可运行调试构建与源码。发布前仍应跑一次
  `build/build.ps1 build` / `bundle` 与真机快捷键、托盘、更新链路复核。
- 截图使用合成余额与渠道数据，仅用于视觉对照，不代表真实账户。
