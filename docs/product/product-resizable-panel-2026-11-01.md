# 产品设计 · 非胶囊形态的缩放与自适应

> 2026-11-01 · 已实现 v0.3.0 · 设计 + 技术方案 + 落地修订
> 范围：面板（panel）与紧凑条（compact）；胶囊（pill）与贴边（dock）保持固定/自动。
> 关联：`DESIGN.md` §4 形态系统、`app/src/app.js` 的 `SIZES / formSize / applyFormInner / fitCompact`、`app/src-tauri/tauri.conf.json`、`app/src-tauri/capabilities/default.json`。

> **落地修订（2026-11-01 晚）**:M0 实测发现 `startResizeDragging` 在真实 pointerdown 下不稳定；
> 实际采用 **Tauri 原生 undecorated resize border**（`resizable:true` + `WS_THICKFRAME`）+
> 卡片右下角 7px 可见手柄（manual pointer drag）。下面的“方案 B”保留为原提案，落地以本注、
> §3.1 的“M0 实测结果”和代码为准。权限 `core:window:allow-start-resize-dragging` 未启用。

## 0. 先说人话

现在面板固定 **400×580**，紧凑条固定 **46px 高**，胶囊和贴边也都是写死的尺寸。用户唯一能调的是「字号」三档，而字号会把整个窗口按倍率重新钳死。结果是：

- 4K / 大屏用户想一次多看几个渠道，只能开「大」字号，但文字变大、窗口也变大，信息密度反而没提高；
- 小屏 / 多窗口用户想把面板收窄一点，没有入口；
- 设置页已经改成索引，但窗口高度仍然固定，渠道多时要频繁滚；
- 用户手动拉大窗口后，下一次轮询 / 切形态 / 换字号又会被 `applyFormInner` 的精确 `setSize` 拉回原尺寸。

本轮要解决的是**“窗口大小”这件事本身**，不是再加一个设置开关：

> 面板可以拖动边缘缩放（360×480 → 760×940，默认仍是 400×580），内容按宽度做真正的自适应；紧凑条可以拉宽以露出更多渠道；胶囊和贴边保持原样。

关键取舍：**不把挂件变成自由窗口。** 不加系统标题栏、不开放最大化、不改变默认尺寸、不改变“卡片刻在桌面”的质感。缩放是“在挂件身份内多给一点空间”，不是转成普通桌面应用。

---

## 1. 现状与硬约束

### 1.1 四种形态的现状

| 形态 | 内容区 | 窗口 | 进入方式 | 现在可缩放 |
|---|---|---|---|---|
| 面板 | 380×560 | 400×580 | 默认 / 从折叠形态展开 | 否 |
| 紧凑条 | 动态宽 ≤380 × 46 | 内容宽+20 × 66 | 设置启用后，`⋯` 菜单 / `Ctrl+2` | 否（宽度由内容算） |
| 胶囊 | 动态宽 132–280 × 46 | 内容宽+20 × 66 | `⋯` 菜单 / 胶囊按钮 | 否（宽度由内容算） |
| 贴边 | 8×64 | 8×64 | 拖到屏幕左/右边缘 | 否（固定） |

窗口本身：`decorations:false`、`transparent:true`、`shadow:false`、`skipTaskbar:true`、`resizable:false`、`maximizable:false`；`body` 四周留 10px 透明边距，卡片、圆角、外阴影都画在这 10px 以内。

### 1.2 为什么现在是固定尺寸（不能直接删掉的约束）

1. **固定尺寸不是“没做”，是刻意的产品决定。** `SIZES` + `formSize` + `applyFormInner` 每次形态切换都用 `setMinSize/setMaxSize/setSize` 把窗口钳到精确值；`setResizable(false)` 是第二道锁。`build/win-style.ps1` 还会断言窗口样式里没有 `WS_THICKFRAME`，防止用户从系统边框拖大。
2. **10px 透明边距是为了阴影和圆角。** 卡片边缘在窗口内 10px；如果把缩放手柄画在卡片边缘，会直接压住列表的 8px 滚动条。
3. **窗口位置已有持久化，尺寸没有。** `config.win_x / win_y` 已有 `save_window_pos` 命令；没有 `win_w / win_h`，所以“上次摆在哪”记得，“上次多大”不记得。
4. **字号和窗口是绑死的。** `cfgFontScale` 改变 `--u`，`applyForm` 会按 `SIZES × u` 重新钳窗口；字号变大 = 窗口变大。用户手动缩放后，换字号会把他的手选尺寸覆盖掉。
5. **贴边/胶囊/紧凑条的几何链是共用的。** `applyForm` 走全局 `geomQueue`，`dockLayout`、`fitCompact`、`fitPill` 都会改窗口尺寸；如果缩放逻辑不进入同一条队列，很容易出现“类=panel、尺寸=46”的错位（历史上踩过）。
6. **窗口是透明的，系统缩放边框可能露边。** 直接 `resizable:true` 可能让 Windows 在透明区域画一圈 resize 边框，和卡片阴影/圆角打架；必须实机验证。

### 1.3 明确不做

- 不做系统标题栏 / 最大化按钮 / 自由拖拽改变窗口层级。
- 不开放胶囊缩放；胶囊的宽度由内容和轮播决定。
- 不开放贴边竖条的尺寸（8×64 是“最省地方”的语义）。
- 不改变默认尺寸；老用户升级后看到的仍是 400×580。
- 不把窗口变成任意比例的画布；有最小/最大，且最小尺寸随字号放大。
- 不为缩放新增数据口径；宽屏只改变布局，不新增“本周/本月合计”这类需要新聚合的指标。

---

## 2. 产品设计

### 2.1 哪些形态可以缩放

| 形态 | 宽度 | 高度 | 交互 | 持久化 |
|---|---|---|---|---|
| **面板** | 内容 340–740；窗口 360–760 | 内容 460–920；窗口 480–940 | 四边 + 四角拖动；双击右下角恢复默认 | `winW/winH`（内容逻辑 px） |
| **紧凑条** | 内容 220–580 | 固定 46（窗口 66） | 只允许左右拖动；拉宽 = 露出更多 chip | `compactMaxW` |
| **胶囊** | 自动 132–280 | 固定 46 | 不提供手柄 | 不存 |
| **贴边** | 8 | 64 | 不提供手柄；拖动是“拖离边缘” | 不存 |

> 面板默认仍是 **380×560 内容 / 400×580 窗口**。最小/最大在 `u`（字号倍率）下按比例放大：`min = 340×u`、`max = 740×u`；用户实际看到的物理尺寸不会因为字号变大而被裁掉。

### 2.2 尺寸模型：内容尺寸、窗口尺寸、字号三者分开

- **内容尺寸**：卡片内部可见区域，不含四周 10px 透明边距。
- **窗口尺寸** = 内容 + 20（左右/上下各 10）。
- **逻辑 px**：Tauri 的 `LogicalSize`；Windows DPI 由系统换算，不把物理像素写进配置。
- **字号倍率 `u`**：md=1 / lg=1.15 / xl=1.3。它只改变排版和最小尺寸，不再强制窗口尺寸。
- **有效宽度** = 内容宽度 / `u`。断点用有效宽度判，保证“13px 字时能放几列”在 lg/xl 下仍然成立。

```
有效宽度 = (window.innerWidth - 20) / u
有效高度 = (window.innerHeight - 20) / u
```

**字号变化规则（建议）：**

1. 首次启动 / 老配置没有尺寸：默认内容 = `380×560 × u`（与现在的行为一致）。
2. 用户手动缩放后：窗口尺寸是权威；换字号只改排版。
3. 字号变大导致当前内容 < 新的最小尺寸：**自动长到最小可用尺寸**，并提示一次「已放大到最小可用尺寸」；不自动缩小用户的大窗口。
4. 字号变小时不自动缩小窗口；用户可手动缩，或点「恢复默认」。
5. 设置页「字号」的说明从「窗口尺寸自动跟随」改成「只调整文字大小；窗口大小可拖动调整」。

### 2.3 交互：手柄、反馈与边界

#### 手柄的位置

**手柄放在窗口最外缘的 10px 透明边距里，而不是卡片可见边缘。**

原因：

- 卡片右侧有 8px 滚动条；手柄如果压在卡片边缘，会和滚动条抢鼠标（真实冲突）。
- 卡片四周有 10px 透明边距专门用来画阴影；这 10px 正好是天然的 resize gutter。
- 标题栏是拖动区，但它在卡片内部；窗口最上缘的 10px 在标题栏之上，不冲突。
- 底栏在卡片内部；窗口最下缘的 10px 在底栏之下，不冲突。

手感上，用户把鼠标移到窗口最外缘就会看到 resize 光标；卡片边缘会亮一条 2px 强调色，告诉他“你正在拉这张卡”。

| 区域 | 面板 | 紧凑条 | 胶囊 | 贴边 |
|---|---|---|---|---|
| 上/下 | ✓ | — | — | — |
| 左/右 | ✓ | ✓ | — | — |
| 四角 | ✓ | — | — | — |
| 双击右下角 | 恢复默认大小 | 恢复默认最大宽 | — | — |

- 手柄厚度 8px（窗口最外缘）；透明、`pointer-events:auto`。
- 光标：`ns-resize` / `ew-resize` / `nwse-resize` / `nesw-resize`。
- 悬停：卡片对应边出现 2px 强调色线（300ms 淡入），离开淡出；不改变卡片底色。
- 拖动中：窗口右下角浮出一个小尺寸标签 `512 × 720`（10px，等宽数字）；松手 800ms 后淡出。不做布局动画，内容实时重排。
- 双击右下角：恢复到 `380×560 × u`；toast「已恢复默认大小」。
- 设置 → 显示 → **窗口尺寸**：显示当前 `宽 × 高`，提供「恢复默认」；同时给「小 / 标准 / 大」三个预设（360×480 / 400×580 / 480×700 窗口）作为键盘/鼠标不便拖动时的退路。
- `⋯` 菜单新增 **恢复默认大小**（仅面板；紧凑条显示「恢复默认宽度」）。

#### 拖动与贴边的冲突

拖动缩放时窗口边缘会移动，现有 `onMoved → scheduleSettleCheck` 会把它当成“用户把窗口拖到屏幕边缘”，可能触发吸附。规则：

- 进入 resize（手柄 pointerdown / OS sizing loop）时置 `RESIZING=true`；
- `RESIZING` 期间 `previewTick` 和 `scheduleSettleCheck` 直接 return，不画吸附预览线、不吸附；
- 松手后延迟 280ms 清除 `RESIZING`，并 `markSelfMove(600)`，让尾部 `moved` 事件不触发吸附；
- 缩放结束后保存尺寸与位置，但**不**因为“停在屏幕边缘”而吸附。用户要吸附，仍然是把标题栏/整块拖到边缘。

#### 最小/最大反馈

- 达到最小/最大时，尺寸标签停在边界值；不再有额外动画。
- 如果用户拖到小于最小尺寸，OS 会钳住；界面不弹错，只保持尺寸标签。
- 多显示器工作区小于最大尺寸时，最大尺寸临时钳到工作区的 95%；换到更大的显示器后恢复配置里的用户尺寸（不覆盖）。

### 2.4 自适应断点

断点用**有效宽度**（内容宽 / `u`），不是物理像素：

| 类 | 有效宽度 | 名称 | 主要变化 |
|---|---|---|---|
| `xs` | < 380 | 窄面板 | 摘要改两行；隐藏 7 日分布柱；隐藏标题栏“上次更新”；详情口径 1 列 |
| `sm` | 380–519 | 标准 | 当前 0.2.0 布局（3 格摘要、单列列表） |
| `md` | 520–719 | 宽面板 | 单列但更舒展；金额行常驻进度条；详情口径 2–3 列；设置内容居中 |
| `lg` | ≥ 720 | 仪表盘 | 渠道列表 **两列网格**；分组标题/失败横幅跨两列；展开行跨两列 |

高度用有效高度：

| 类 | 有效高度 | 变化 |
|---|---|---|
| `h-xs` | < 560 | 隐藏摘要的次要副行；列表滚动 |
| `h-sm` | 560–759 | 标准 |
| `h-lg` | ≥ 760 | 列表多显示几行；不新增区块，留白随行数自然分配 |

#### `xs`（有效 340–379）：两行摘要

```
┌──────────────────────────────┐ 340 content
│ ◈ Grandettoken        ⟳  ⋯  │ 44
├──────────────────────────────┤
│ 近 30 天临期      │ 签到      │ 44
│ 2,253 分          │ 2/3       │
├──────────────────────────────┤
│ 最先到期 WorkBuddy  10-26     │ 40
│ 剩 3 天 · 2,193 分            │
├──────────────────────────────┤
│ 需要处理 2                    │
│ [CL] Claude Code   0.0%  ›   │
│ ...                           │
│ 其他 6                        │
│ [4S] 4SAPI        ¥120  ›    │
├──────────────────────────────┤
│ ●5 正常  ●2 预警  ●1 失联  ⚙ │ 34
└──────────────────────────────┘
```

- 摘要网格变成 `grid-template-areas:"hero ck" "risk risk"`；风险卡整行，渠道名和日期不再挤。
- 7 日分布柱隐藏（340 下没有足够宽度同时放文字和柱子）。
- 标题栏隐藏“上次更新”文字；刷新和 ⋯ 保留。
- 底栏“设置与管理”在 xs 下收成齿轮图标（保留 `aria-label`），状态计数保留。
- 行内右侧 `.p` 保留；`.nm .s` 允许省略；值列最小 64px。

#### `sm`（有效 380–519）：当前布局

```
┌────────────────────────────────────┐ 380 content
│ ◈ Grandettoken     刚刚  ⟳  ⋯      │
├────────────────────────────────────┤
│ 近 30 天将过期 │ 最先到期    │ 签到 │
│ 2,253 分       │ WorkBuddy   │ 2/3  │
│ 2 个渠道 · 可筛选│ 10-26 剩3天 │      │
├────────────────────────────────────┤
│ 需要处理 2                          │
│ ...                                 │
└────────────────────────────────────┘
```

与 0.2.0 一致；这是默认体验。

#### `md`（有效 520–719）：宽面板、单列

- 摘要三格保留，内边距从 8/10 提到 10/12，7 日柱更宽。
- 金额型行（有分母）**常驻 3px 进度条**，不再只在告急行显示；右侧口径 `剩 X%` 常驻。
- 行头左右内边距 12 → 16；名称和元信息更舒展。
- 展开详情：核心三数保持 3 列；口径 `.kv` 从 2 列变 `repeat(auto-fit,minmax(180px,1fr))`（通常 3 列）；7 日柱每根更宽。
- 设置页内容最大宽 640，居中；索引行不拉成一条超长横线。

#### `lg`（有效 ≥720）：两列渠道网格

```
┌──────────────────────────────────────────────────────┐ 740 content
│ ◈ Grandettoken              刚刚   ⟳  ⋯             │
├──────────────────────────────────────────────────────┤
│ 近 30 天将过期    │ 最先到期        │ 签到           │
│ 2,253 分          │ WorkBuddy 10-26 │ 2/3            │
├──────────────────────────────────────────────────────┤
│ 需要处理 2                                           │
│ ┌──────────────────────┐ ┌──────────────────────┐   │
│ │ [CL] Claude Code 0.0%│ │ [DS] DeepSeek ¥0.03  │   │
│ └──────────────────────┘ └──────────────────────┘   │
│ 其他 6                                               │
│ ┌──────────────────────┐ ┌──────────────────────┐   │
│ │ [4S] 4SAPI   ¥120.00 │ │ [OC] OpenCode  62.0% │   │
│ └──────────────────────┘ └──────────────────────┘   │
├──────────────────────────────────────────────────────┤
│ ●5 正常  ●2 预警  ●1 失联        设置与管理 退出   │
└──────────────────────────────────────────────────────┘
```

- `#list` 改成 `display:grid; grid-template-columns:repeat(2,minmax(0,1fr)); gap:0 12px`。
- 分组标题、失败横幅、签到提示条、空态、骨架整行跨两列（`grid-column:1/-1`）。
- 单行仍复用现有 380px 行组件：每列约 365px，信息密度不变，不需要重新设计行。
- 展开行跨两列（`grid-column:1/-1`）；展开详情占满 740，口径 3 列、7 日柱更宽。
- 列表滚动条仍在卡片右缘；缩放手柄在窗口最外缘的透明边距里，不冲突。
- 两列的顺序是行优先（左→右、上→下），与“紧迫度优先”的分组顺序一致；不改变后端排序。

#### 展开详情

| 宽度类 | 核心三数 | 口径 `.kv` | 7 日柱 | 动作 |
|---|---|---|---|---|
| xs | 3 列（数字可能缩到 15px） | 1 列 | 7 根，gap 2 | 换行 |
| sm | 3 列 | 2 列 | 7 根，gap 4 | 右对齐 |
| md | 3 列 | 3 列 | 7 根，gap 6 | 右对齐 |
| lg | 3 列 | 3 列 | 更宽 | 右对齐 |

- 详情卡仍是内嵌卡；`width:100%`，不设固定 356px。
- 行展开时列表滚动位置保持；缩小时详情内容不被裁切（列表滚动）。

#### 设置与管理

- 面板缩放后，设置页仍占满窗口；内容最大宽 640 居中，避免宽屏下每行文字过长。
- 索引行 54px 不变；宽屏下两行摘要不省略。
- 两个设置页签的控件仍在同一列；不引入双栏表单（会打乱阅读顺序）。

### 2.5 紧凑条：拉宽 = 露出更多渠道

- 紧凑条的高度固定 46；只允许左右拖。
- 宽度 = `min(实际 chip 总宽 + 控件, compactMaxW)`；`compactMaxW` 默认 380，范围 220–580。
- 用户往右拉：提高 `compactMaxW`，放不下的 chip 从 `+N` 里逐步出现；如果所有渠道都露出，再拉不会留空白，窗口宽度会回落到内容宽（只记住 max，不记住“空宽”）。
- 用户往左拉：降低 max，更多 chip 进入 `+N`。
- `+N` 仍可点：展开面板并定位到第一个被藏渠道。
- 紧凑条拉宽的持久化只存 `compactMaxW`，不存“当前宽度”；这样换字号/换渠道数量时仍然自适应。

### 2.6 持久化与启动

- 新增配置：
  - `winW: Option<u32>`、`winH: Option<u32>`：面板内容逻辑 px。
  - `compactMaxW: Option<u32>`：紧凑条内容最大宽。
  - `resizeHintSeen: bool`：一次性提示是否已看过。
- 保存时机：resize 结束后 250ms 防抖，只写一次；不随每帧写。
- 启动顺序：
  1. 读配置、应用 `--u`；
  2. 计算 min/max；把 `winW/winH` 钳进范围；
  3. 设置窗口尺寸（隐藏状态下完成）；
  4. 按 `win_x/win_y` 恢复位置，并用**新尺寸**做可见区裁剪；
  5. 应用形态（panel/compact/pill），显示窗口。
- 形态切换：
  - 面板 → 紧凑/胶囊：保留面板尺寸；
  - 回面板：恢复上次面板尺寸；
  - 贴边：保留吸附前的形态与面板尺寸；展开/拖离时恢复；
  - 胶囊：不参与尺寸持久化。
- 多显示器：窗口移动到不同 DPI 的显示器时，Tauri 的逻辑尺寸可能触发 `onResized`；只把**用户主动 resize**（`RESIZING`）写盘，避免 DPI 切换把尺寸改掉。若新工作区小于当前尺寸，临时收缩并提示；换回大屏后用保存的用户尺寸。

### 2.7 文案

| 位置 | 文案 |
|---|---|
| 设置 → 显示 → 窗口尺寸 | 当前 `400 × 580`　[恢复默认]　小 / 标准 / 大 |
| 字号说明 | 只调整文字大小；窗口大小可拖动调整 |
| 首次升级提示（一次） | 现在可以拖动面板边缘调整大小，双击右下角恢复默认 |
| ⋯ 菜单 | 恢复默认大小（面板）/ 恢复默认宽度（紧凑条） |
| 达到最小尺寸（一次） | 已到最小尺寸；再大字请调高字号或恢复默认 |
| 双击右下角 | 已恢复默认大小 |

---

## 3. 技术方案

### 3.1 窗口策略：自定义边缘 + `startResizeDragging`（推荐）

#### 方案 A：原生 `resizable:true`

- 做法：`tauri.conf.json` 改 `resizable:true`；`applyFormInner` 只对 pill/dock `setResizable(false)`。
- 优点：系统原生 resize 循环，代码最少；DPI/多屏由 Windows 处理。
- 缺点：
  - 透明 + 无边框窗口会在窗口外缘出现系统 resize 边框/发光，和卡片阴影打架；
  - 手柄在窗口外缘，离卡片可见边缘 10px；
  - 紧凑条的高度也会被原生边框允许拖（除非 min/max 高相等）；
  - `win-style.ps1` 的「无 WS_THICKFRAME」断言要改，历史上这是固定尺寸的机制。
- 结论：作为 fallback，不作为首选。

#### M0 实测结果（2026-11-01）

- `startResizeDragging` 在**没有物理按下**时能工作（实机把窗口从 250 拉到 370）；
  但在真实 pointerdown 后调用时，注入/捕获路径下不稳定，400×580 没有响应。
- 直接用 `PostMessage(WM_NCLBUTTONDOWN, HTRIGHT)` 到父窗口可以稳定进入系统 sizing loop
  （400→250 / 250→370）。
- 因此落地选择：`resizable:true` + Tauri 自带的 `TAURI_DRAG_RESIZE_BORDERS` 子窗口
  （它会在窗口最外缘提供 8px 原生 resize 边框），再加一个卡片右下角 7px 可见手柄做
  **manual pointer drag**（`setSize`，不依赖系统 loop），兼顾可发现性与可测试性。
- `WS_MAXIMIZEBOX` 仍然关闭；`win-style.ps1` 的断言改为“panel/compact 有 WS_THICKFRAME、
  没有 WS_MAXIMIZEBOX”。

#### 方案 B：自定义边缘 + `startResizeDragging`（原提案，未采用）

- 保持 `resizable:false`（或按 spike 结果临时 true），在窗口最外缘放 8px 透明手柄；pointerdown 调 `getCurrentWindow().startResizeDragging("East" | "West" | "North" | "South" | "NorthEast" | ...)`。
- Tauri 2 的 `start_resize_dragging` 最终是 `WM_NCLBUTTONDOWN` + `HT*`；系统进入原生 sizing loop，但**不画** WS_THICKFRAME 的边框（是否成立要 spike 验证）。
- 优点：
  - 手柄可以精确到东/西/南/北/四角；紧凑条只给 E/W；
  - 与滚动条、标题栏拖动区都不冲突（手柄在 10px 透明边距里）；
  - 保留“无 WS_THICKFRAME”的现有断言；
  - 可以自定义悬停高亮、尺寸标签、双击复位。
- 需要新增权限：`core:window:allow-start-resize-dragging`。
- 风险点（必须 spike）：
  1. `resizable:false` 时 `startResizeDragging` 是否真的进入 sizing loop；
  2. sizing loop 会不会吞掉 webview 的 pointerup；用 `onResized` 防抖收尾；
  3. 系统 loop 是否遵守 `setMinSize/setMaxSize`；
  4. 透明 + `decorations:false` 下是否出现 1px 边框；
  5. 拖动缩放时 `onMoved` 是否触发；会不会误吸附；
  6. 125/150/200% DPI、多显示器。
- 如果第 1 条不成立：改 `resizable:true`（方案 A）+ 自定义手柄；系统边框若可接受就接受，若不可接受再上方案 C。

#### 方案 C：Rust `WM_NCHITTEST` 子类化（兜底，不推荐首选）

- 在窗口 proc 里对 8px 边缘返回 `HTLEFT/HTRIGHT/...`，由 `DefWindowProc` 启动 sizing。
- 优点：完全掌控，不依赖 Tauri 的 resize 命令。
- 缺点：要处理窗口子类化、透明窗、DPI、与 Tauri 事件循环的关系；回归面积大。
- 仅在 A/B 都不满足时使用。

**落地结论：走 A 的变体**——原生 `resizable:true` + Tauri undecorated border 负责窗口边缘，
右下角手柄负责可见入口/可测试的 manual drag。B 的 `startResizeDragging` 不再使用。

### 3.2 几何模型与代码改造

#### 常量

```js
// 内容尺寸（逻辑 px，u=1 时）；窗口 = 内容 + 20
const PANEL_MIN = [340, 460];     // 窗口 360×480
const PANEL_DEFAULT = [380, 560]; // 窗口 400×580（不变）
const PANEL_MAX = [740, 920];     // 窗口 760×940
const COMPACT_MIN_W = 220;
const COMPACT_MAX_W = 580;
const COMPACT_H = 46;
```

- `u = fsU()`；实际 min/max/default 都乘 `u`。
- 最大尺寸再按当前显示器工作区 95% 临时钳制。

#### `SIZE` 状态

```js
let SIZE = { w: 380, h: 560, form: "panel", ready: false };
```

- `readSizeFromWindow()`：`outerSize / scaleFactor - 20`。
- `applyPanelSize({growOnly})`：钳进 min/max；如果 `growOnly`（字号变大）只在当前小于 min 时长到 min；否则按用户尺寸。
- `formSize(form)`：
  - panel：`[SIZE.w + 20, SIZE.h + 20]`（SIZE 未初始化时用默认 ×u）；
  - compact：`[compactW + 20, COMPACT_H*u + 20]`；
  - pill：保持现有动态宽；
  - dock：`[8,64]`。

#### `applyFormInner` 的改造点

现在的逻辑（简化）：

```js
const [w,h] = formSize(form);
await setMinSize(null); await setMaxSize(null); await setResizable(false);
await setSize(w,h);
await setMinSize(w,h); await setMaxSize(w,h);
```

改为：

```js
const form = resolveForm(form);
if (form === "panel") {
  const [cw, ch] = clampPanel(SIZE.w, SIZE.h);
  await setResizable(false);          // B 方案；A 方案为 true
  await setMinSize(minWindow());
  await setMaxSize(maxWindow());
  if (SIZE.form !== "panel" || SIZE.ready !== true) {
    await setSize(windowOf(cw, ch));   // 只在形态真的变了/首次时设，不覆盖用户尺寸
  }
} else if (form === "compact") {
  await setResizable(false);
  await setMinSize([COMPACT_MIN_W+20, COMPACT_H+20]);
  await setMaxSize([COMPACT_MAX_W+20, COMPACT_H+20]);
  await setSize([compactW+20, COMPACT_H+20]);
} else { // pill
  await setResizable(false);
  await setSize(exactPill);
  await setMinSize(exactPill); await setMaxSize(exactPill);
}
SIZE.form = form;
```

- **关键**：面板在轮询/字号重绘/`fitCompact` 触发的 `applyForm("panel")` 中不能再无条件 `setSize`；否则用户刚拉大的窗口会被拉回。用 `SIZE.form` 与 `force` 参数区分“形态切换”与“同形态重算”。
- 字号变化：`applyFontScale()` 后调用 `applyPanelSize({growOnly:true})`，只保证不小于新 min。
- 紧凑条：`fitCompact()` 用 `min(measured, CFG.compactMaxW || 380)`；用户拉宽后更新 `compactMaxW`，再 `fitCompact`。不把“空宽”写死。
- 贴边：`dockLayout` 用 `formSize(currentForm())`；展开时恢复面板尺寸；收起/展开不写尺寸。
- `pillPeek`：胶囊不在本范围，但 `formSize("pill")` 仍返回动态宽；peek 高度基于当前胶囊尺寸，不受影响。

#### 缩放手柄

```html
<div id="rz" aria-hidden="true">
  <i data-dir="North"></i><i data-dir="South"></i>
  <i data-dir="East"></i><i data-dir="West"></i>
  <i data-dir="NorthEast"></i><i data-dir="NorthWest"></i>
  <i data-dir="SouthEast"></i><i data-dir="SouthWest"></i>
</div>
```

```css
#rz{position:fixed;inset:0;z-index:80;pointer-events:none}
#rz i{position:absolute;pointer-events:auto}
#rz [data-dir="North"]{left:10px;right:10px;top:0;height:8px;cursor:ns-resize}
#rz [data-dir="South"]{left:10px;right:10px;bottom:0;height:8px;cursor:ns-resize}
#rz [data-dir="East"]{top:10px;bottom:10px;right:0;width:8px;cursor:ew-resize}
#rz [data-dir="West"]{top:10px;bottom:10px;left:0;width:8px;cursor:ew-resize}
#rz [data-dir$="East"]:not([data-dir="East"]),
#rz [data-dir$="West"]:not([data-dir="West"]){width:14px;height:14px}
#rz [data-dir="NorthEast"]{top:0;right:0;cursor:nesw-resize}
#rz [data-dir="NorthWest"]{top:0;left:0;cursor:nwse-resize}
#rz [data-dir="SouthEast"]{bottom:0;right:0;cursor:nwse-resize}
#rz [data-dir="SouthWest"]{bottom:0;left:0;cursor:nesw-resize}
body.form-pill #rz,body.docked #rz,body.no-resize #rz{display:none}
body.form-compact #rz [data-dir="North"],
body.form-compact #rz [data-dir="South"],
body.form-compact #rz [data-dir*="North"],
body.form-compact #rz [data-dir*="South"]{display:none}
```

- 手柄在 `position:fixed; inset:0` 的覆盖层里，位于窗口最外缘；`body` 的 10px 边距不会把它们裁掉。
- 悬停时给 `body` 加 `rz-n/e/s/w/ne/...` 类，CSS 在卡片对应边画 2px 强调线（`body::after` 或边框变色）。
- 手柄本身不进入 Tab 顺序（`aria-hidden`）；键盘用户通过设置页预设/恢复默认操作。
- `pointerdown` 调 `startResizeDragging`；同时 `RESIZING=true`、记录起始尺寸、显示尺寸标签。

#### `RESIZING` 生命周期

```js
let RESIZING = false, resizeIdleTimer = null;

function beginResize(dir) {
  if (DOCK.on || currentForm() === "pill") return;
  if (currentForm() === "compact" && !/^(East|West)$/.test(dir)) return;
  RESIZING = true;
  document.body.classList.add("resizing");
  appWindow.startResizeDragging(dir).catch((e) => {
    RESIZING = false;
    document.body.classList.remove("resizing");
    showToast("缩放失败:" + e, true);
  });
}

appWindow.onResized(() => {
  readSizeFromWindow();
  updateSizeClass();
  if (RESIZING) {
    clearTimeout(resizeIdleTimer);
    resizeIdleTimer = setTimeout(endResize, 280);
  }
});

function endResize() {
  RESIZING = false;
  document.body.classList.remove("resizing");
  markSelfMove(600);
  saveSizeSoon();
  // 位置可能因左/上边缩放而变;保存一次
  savePosSoon();
}
```

- 不能依赖 webview 的 `pointerup`：OS sizing loop 可能吞掉它；用 `onResized` 的 280ms 静止判定。
- 安全兜底：`setTimeout(() => { if (RESIZING) endResize(); }, 5000)`。
- `onMoved` / `previewTick` / `scheduleSettleCheck` 在 `RESIZING` 时直接返回。
- 尺寸标签在 `onResized` 里更新；`endResize` 后 800ms 淡出。

#### 配置与命令

`app/core/src/config.rs`：

```rust
#[serde(default)]
pub win_w: Option<u32>,
#[serde(default)]
pub win_h: Option<u32>,
#[serde(default)]
pub compact_max_w: Option<u32>,
#[serde(default)]
pub resize_hint_seen: bool,
```

- 默认 `None/false`；老配置自动兼容。
- `save_window_pos` 的同类命令新增 `save_window_size(state, w, h, compact_max_w)`；只写这几个字段，避免整份配置在 resize 结束瞬间与其他保存竞争。
- `save_window_pos` 已有的 `win_x/win_y` 逻辑不变；启动时先按 `win_w/h` 设尺寸，再用新尺寸做 `clamp_to_monitors`。
- 配置合并（`lib.rs` 里 incoming/current 的 `None` 保留）也加上这三个字段，避免旧前端保存时把尺寸清空。

#### 启动顺序

1. Rust `setup`：读配置，若 `form == panel` 且 `win_w/h` 有值，先 `set_size`；再 `clamp_to_monitors` 位置；窗口仍隐藏。
2. 前端 `init`：读配置、`applyFontScale`、`readSizeFromWindow`、`applyForm`（同尺寸，不重复 setSize）。
3. `show` 后 `ResizeObserver` 第一次回调设置 `data-w/h`。
4. 若配置损坏（`CONFIG_BROKEN`），尺寸用默认，不写盘（现有纪律）。

### 3.3 自适应布局实现

- `ResizeObserver` 挂在 `document.body`（或 `#app`），rAF 合并：
  ```js
  const ro = new ResizeObserver(() => {
    requestAnimationFrame(() => {
      const u = fsU();
      const effW = (document.body.clientWidth) / u;
      const effH = (document.body.clientHeight) / u;
      document.body.dataset.w = effW < 380 ? "xs" : effW < 520 ? "sm" : effW < 720 ? "md" : "lg";
      document.body.dataset.h = effH < 560 ? "xs" : effH < 760 ? "sm" : "lg";
      syncResizeHandles();
      if (currentForm() === "compact") scheduleFitCompact();
    });
  });
  ro.observe(document.body);
  ```
  - 不调用 `render()`；只切 `data-w/h`，CSS 完成布局，保持滚动位置与焦点。
  - `u` 变化时重新计算（`applyFontScale` 后触发一次）。
- CSS：
  - `.sum` 在 `body[data-w="xs"]` 下用 `grid-template-areas` 两行；其余宽度用当前三列。
  - `body[data-w="sm"]` 保持现状；`md` 加内边距/进度条；`lg` 的 `#list` 两列网格。
  - `body[data-w="lg"] .grp, .fstrip, .ckstrip, .empty, .sk-wrap{grid-column:1/-1}`；`.row.open{grid-column:1/-1}`。
  - `.row` 设 `min-width:0`；两列下 `border-bottom` 仍按单元格画。
  - `body[data-w="xs"] .sum .expbars{display:none}`；`.sum` 的 `grid-template-areas` 把风险卡放第二行。
  - `body[data-w="xs"] .tb-live{display:none}`；`.foot .lk .lk-tx{display:none}`（需要给 `#lkKeys` 加图标/文本包裹，见 §3.5）。
  - `.kv{grid-template-columns:repeat(auto-fit,minmax(180px,1fr))}`；`md/lg` 自然 3 列。
  - `body[data-w="md"] .mindex, body[data-w="md"] .mbody{max-width:640px;margin-inline:auto}`。
  - `body[data-w="md"] .row:not(.urg) .bar{display:block}`（金额型有分母时）；`bar` 的显示由 JS 的 `bar` 是否生成决定，CSS 只控制显示。
- 高度：`#list` 仍 `flex:1;overflow-y:auto`；两列网格同样滚动；`h-xs` 隐藏摘要副行/7 日柱，保证列表有空间。
- 紧凑条：`fitCompact` 的 max 从 `CFG.compactMaxW` 读；`endResize` 时把当前内容宽写入 `compactMaxW`；`scheduleFitCompact` 在宽度变化后重算。
- 设置页：`max-width:640px` 居中；两列网格不应用到 `.mbody`。

### 3.4 测试与可观测性

#### 新增 `build/verify-resize.js`（CDP）

- 尺寸矩阵（内容逻辑 px）：
  - `340×460`（xs）、`380×560`（sm 默认）、`520×700`（md）、`740×920`（lg）；
  - 边界：`360×480`（min window）、`760×940`（max window）、`400×900`（窄高）、`760×480`（宽矮）。
- 每个尺寸 × 字号（md/lg/xl）：
  - 断言 `body.dataset.w/h` 正确；
  - 复用 `visual-forms.js` 的越界测量：无横向 `overflow`、无 `vover`、无不可接受的 `cut`；
  - 断言摘要区域、头部、底栏不互相覆盖；两列网格下分组标题跨列、行不重叠；
  - 断言 `#rz` 手柄数量/方向（panel 8、compact 2、pill/dock 0）。
- 持久化：
  - 模拟 `setSize` → `endResize` → `get_config` 有 `winW/winH`；
  - 重载页面 → 窗口尺寸与配置一致；
  - 面板 → 紧凑 → 面板，面板尺寸恢复；
  - 贴边展开/收回，面板尺寸不丢。
- 字体：
  - `xl` 下尝试 `setSize(360×480)`，断言被钳到 `340×1.3 + 20` 等最小窗口；
  - 字号从 `md` 切 `xl` 且用户窗口小于新 min 时，自动长到 min，不回到默认 400×580。
- 双击/菜单：
  - 双击右下角 → 默认尺寸 + toast；
  - `⋯` → 恢复默认大小。
- 多显示器/DPI：人工清单（自动化只覆盖当前屏幕）。
- 回归：`verify-form` 改为断言 panel/compact 有手柄、pill/dock 无手柄；`visual-forms` 增加尺寸场景；`jsdom-smoke` 增加 `data-w/h` 计算、配置持久化、`fitCompact` max 的纯逻辑断言；core 增加新配置字段的 serde/默认值测试。
- 现有 `win-style.ps1`：
  - 若走方案 B（保留 `resizable:false`）：断言不变，另加“窗口有 8 个手柄且 compact 只有 2 个”的 DOM 断言；
  - 若走方案 A（原生 resizable）：断言改为 panel/compact 有 WS_THICKFRAME、pill/dock 无；这条改动必须在 PR 里写明。

#### 手工清单

- Windows 10/11，DPI 100/125/150/200%；双屏不同 DPI；拔掉副屏。
- 面板四边/四角分别拖；最小/最大；拖到屏幕边缘不吸附；拖离后再吸附。
- 紧凑条左右拖；`+N` 出现/消失；所有 chip 露出后继续拖不产生空白。
- 双击右下角；`⋯` 恢复默认；设置页恢复默认。
- 折叠到胶囊/贴边再展开，尺寸恢复；重启后尺寸恢复。
- 设置页输入框聚焦时拖边缘；列表滚动条可正常拖。
- 大字号 + 最小窗口无裁切；`prefers-reduced-motion` 下手柄高亮仍可见（只淡入淡出）。

#### 性能

- 缩放期间只做 CSS 重排 + `data-w/h` 切换；不调用 `render()`、不重建列表。
- `save_window_size` 防抖 250ms，一次 resize 只写一次。
- `ResizeObserver` 用 rAF 合并；`fitCompact` 只在 compact + 宽度变化时调度。
- 2 列网格下 13 个渠道的行组件仍然轻量；不做虚拟列表。

### 3.5 需要的 HTML/结构改动

- `index.html`：新增 `#rz` 手柄层；`#lkKeys` 内部拆成 `<svg class="lk-ico">` + `<span class="lk-tx">设置与管理</span>`（xs 下只留图标）。
- `app.js`：
  - `SIZES`/`formSize`/`applyFormInner`/`fitCompact`/`dockLayout` 改造；
  - `ResizeObserver`、`onResized`、`onScaleChanged`、`RESIZING`、`save_window_size`；
  - 设置页「窗口尺寸」行与预设按钮；
  - `⋯` 菜单「恢复默认大小」；首次提示。
- `styles.css`：手柄层、`data-w/h` 断点、摘要两行、两列网格、详情 3 列、设置居中、hover 高亮。
- `capabilities/default.json`：`core:window:allow-start-resize-dragging`（方案 B）。
- `tauri.conf.json`：方案 B 保持 `resizable:false`；方案 A 改 `true` + 初始 min/max。
- `app/core/src/config.rs` + `lib.rs`：新配置字段 + `save_window_size` + 合并时保留 `None`。
- `build/`：`verify-resize.js`、`verify-form.js` 更新、`visual-forms.js` 尺寸场景、`win-style.ps1`（视方案）、`jsdom-smoke.js` 配置断言。

---

## 4. 实施计划

| 阶段 | 内容 | 产出 | 估时 |
|---|---|---|---|
| **M0 Spike** | `resizable:false` + `startResizeDragging` 实机验证；透明窗边框、滚动条、DPI、吸附误判 | 一页结论 + 选定 A/B/C | 0.5–1 天 |
| **M1 几何** | 配置字段、启动顺序、`SIZE`/`formSize`/`applyFormInner`、手柄、`RESIZING`、持久化、恢复默认 | 面板可拖、尺寸能记住、贴边不误吸 | 2 天 |
| **M2 自适应** | `ResizeObserver` + `data-w/h`；xs 两行摘要；md 舒展；lg 两列网格；详情 3 列；设置居中；紧凑 max | 4 档尺寸无越界 | 2–3 天 |
| **M3 测试/发布** | `verify-resize`、`verify-form`/`visual-forms`/jsdom/core 更新；多 DPI/多屏手工；0.3.0 打包 | 全量回归 + 安装包 | 1.5–2 天 |

总计约 **6–8 个工程日**，不含用户评审。建议作为 **0.3.0** 发布；默认尺寸不变，老用户升级后不会“突然变大”。

---

## 5. 风险与需要拍板的点

| # | 风险/决策 | 建议 | 影响 |
|---|---|---|---|
| 1 | 手柄在窗口外缘还是卡片边缘 | **窗口外缘**（10px 透明边距里） | 避开滚动条；手柄离卡片视觉边缘 10px，用悬停高亮补偿 |
| 2 | `startResizeDragging` 在 `resizable:false` 下是否可用 | **已验：不可靠**；改用原生 border + manual grip | 已改 `WS_THICKFRAME` 断言；`start-resize-dragging` 权限未启用 |
| 3 | 面板最大尺寸 | **760×940**（内容 740×920） | 再大就不像挂件；两列网格刚好 |
| 4 | 紧凑条是否允许手动宽 | **允许，但只作为 max** | 拉宽只露出更多 chip，不留空宽 |
| 5 | 字号是否自动改窗口 | **只保证不小于 min，不自动缩小** | 用户的大窗口不会被字号切回去 |
| 6 | 两列阈值 | **有效 ≥720** | 每列约 365px，行组件不用改 |
| 7 | 设置/管理页是否允许缩放 | **允许** | 面板就是面板；内容居中限宽 |
| 8 | 是否加预设按钮 | **加**（小/标准/大 + 恢复默认） | 键盘用户与“不知道怎么拖”的用户有退路 |
| 9 | 是否加一次性提示 | **加**（`resizeHintSeen`） | 手柄不可见，不给提示很难发现 |
| 10 | 是否改默认尺寸 | **不改** | 0.2.0 用户升级后布局不变 |

---

## 6. 验收标准

- [ ] 面板可从四边/四角拖到 min/max；内容实时重排，无横向溢出、无裁切、无行重叠。
- [ ] 340/380/520/740 四个有效宽度下，摘要、头部、底栏、列表、详情均可用；两列网格下分组标题跨列、展开行跨列。
- [ ] 紧凑条可左右拖；拉宽露出更多渠道；全露出后不产生空白；`+N` 仍可点。
- [ ] 胶囊/贴边没有手柄；贴边展开/收回、胶囊展开/收起不改变面板尺寸。
- [ ] 面板尺寸、位置、紧凑 max 持久化；重启、切形态、贴边往返都恢复。
- [ ] 字号 md/lg/xl 下 min/max 按 `u` 缩放；字号变大不会把用户窗口缩回默认。
- [ ] 双击右下角、`⋯` 菜单、设置页三种方式都能恢复默认。
- [ ] 缩放过程中不误吸附、不触发预览线；列表滚动条可正常拖动；不重复写盘。
- [ ] 多显示器/125–200% DPI 下窗口不出屏、不裁切；拔副屏后能夹回。
- [ ] 现有 0.2.0 回归（core 361、jsdom 120、视觉 30、Codex 48、设置/签到/焦点/形态）全绿。

---

## 7. 附录：代码触点

| 文件/函数 | 作用 | 本轮改动 |
|---|---|---|
| `app/src/app.js` `SIZES / formSize` | 形态尺寸 | 改为用户尺寸 + min/max；panel 用 `SIZE` |
| `applyFormInner` | 形态切换 + 精确尺寸 | 只在形态变化/首次设尺寸；panel/compact 不再 `setResizable(false)` 锁死；加入 min/max |
| `fitCompact` / `scheduleFitCompact` | 紧凑条宽度 | 用 `compactMaxW`；用户拉宽后更新 max |
| `dockLayout` / `DOCK` | 贴边 | 展开/收回用面板尺寸；缩放期间不吸附 |
| `onMoved` / `scheduleSettleCheck` / `previewTick` | 拖动吸附判定 | `RESIZING` 时跳过 |
| `cfgFontScale` change | 字号 | 不再 `applyForm` 重置尺寸；改为 `growOnly` 钳制 |
| `app/src-tauri/capabilities/default.json` | ACL | 加 `core:window:allow-start-resize-dragging` |
| `app/core/src/config.rs` | 配置 | `win_w/win_h/compact_max_w/resize_hint_seen` |
| `app/src-tauri/src/lib.rs` `save_window_pos` | 位置持久化 | 新增 `save_window_size`；启动先设尺寸再夹位置 |
| `app/src/styles.css` | 布局 | `#rz`、`data-w/h` 断点、两行摘要、两列网格、详情 3 列 |
| `build/verify-form.js` / `visual-forms.js` / `jsdom-smoke.js` | 回归 | 尺寸/手柄/持久化断言；视觉尺寸矩阵 |
| `build/win-style.ps1` | Win32 样式 | 方案 B 不变；方案 A 改断言 |

---

## 8. 一句话结论

**面板做成“有边界的缩放”：默认 400×580 不变，可拉到 760×940；手柄放在窗口最外缘的透明边距里，避开滚动条；宽到 720 就自然变成两列渠道网格；紧凑条只拉宽露出更多渠道；胶囊和贴边不动。** 技术首选“自定义边缘 + `startResizeDragging`，保留 `resizable:false`”，M0 先验证这条，再决定是否退到原生 resizable。
