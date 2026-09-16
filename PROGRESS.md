# TokenScope 开发进展

> 最后更新:2026-09-16
> 状态:**三种形态固定尺寸 + 视觉对齐设计稿 + 「设置与管理」页 + OpenCode 三窗口展示 + 紧凑条切换入口,全部落地并验证;单 exe 交付仍待定夺**

## 一句话现状

Rust 侧完成并通过 14 个单元测试,release 编译通过(5.24 MB exe)。
三种形态的窗口尺寸固定不可拖拽、视觉按原型稿对齐、密钥与设置合并进
「设置与管理」页 —— 三项均已落地并做了运行时验证(截图 + 断言,见
「界面改版」章节)。仓库已迁到纯英文路径 `G:\game\nw\Grandettoken`,
构建不再镜像到用户目录。
**遗留一个交付层问题**:GNU 工具链下 exe 加载期依赖 `WebView2Loader.dll`,
裸单文件启动报系统错误 —— 见「进行中」章节,三个可选方案已论证。

## 产物约束(用户确认)

**单 exe、免安装、绿色运行。** 现状与该约束的差距:功能与体积没问题
(5.24 MB、全部系统 DLL 依赖),唯一例外是 WebView2 加载器 ——
Tauri 在 GNU 工具链下默认动态链接 `WebView2Loader.dll`(MSVC 工具链才静态链接),
裸 exe 缺它会拒绝启动。解决路径见「进行中」章节;无论选哪条,产物都无需安装。

---

## 已完成

### 1. 产品设计与原型

- `token-dashboard-prototype.html` —— 交互原型,四种折叠形态(面板 / 紧凑条 / 胶囊 / 边缘吸附)
- `providers.js` —— 渠道适配器配置的 JS 版本(设计阶段的产物,保留作参考)

### 2. 技术选型

| 项 | 选择 | 理由 |
|---|---|---|
| 框架 | Tauri 2 | 实测 release exe **4.99 MB**、常驻内存 **39.9 MB**;Electron 同功能约 100MB / 200–300MB |
| HTTP | reqwest + **native-tls** | Windows 走系统 SChannel。用 rustls 会因为 ring/aws-lc 需要 nasm 汇编器,在 GNU 工具链下容易失败 |
| 数据库 | rusqlite(bundled) | 单文件 SQLite,存用量快照 |
| 密钥 | Windows 凭据管理器(keyring) | 见下方安全设计 |
| 开机自启 | winreg 写 HKCU Run 键 | 免管理员权限;每次启动重写 exe 路径,绿色版移动位置不失效 |
| 工具链 | Rust **GNU** 工具链 + MinGW-w64 | 本机无管理员权限,装不了 MSVC Build Tools |

### 3. 代码结构

```
app/
  core/                    纯逻辑层,不依赖 Tauri —— 可独立跑测试
    src/providers.rs       四个渠道的响应解析
    src/store.rs           SQLite 快照 + 消耗推算
    src/secrets.rs         Windows 凭据管理器封装
    src/fetch.rs           取数、失败降级、视图组装
    src/config.rs          设置项
  src-tauri/               Tauri 外壳:窗口、托盘、命令、轮询调度
  src/                     前端(纯静态,无打包器)
build/                     工具链安装 / 构建 / 诊断脚本
```

**为什么把逻辑层拆成独立 crate**:一开始逻辑和 Tauri 在同一个 crate,
测试二进制会把整个 WebView2 栈链接进去,启动时报
`STATUS_ENTRYPOINT_NOT_FOUND` 直接跑不起来。拆开后测试只链接 serde/rusqlite,
**14 个测试 0.01 秒跑完**。

### 4. 四个渠道的适配(已实测解析正确)

| 渠道 | 接口 | 类型 | 换算 |
|---|---|---|---|
| 4SAPI | `GET https://4sapi.com/api/usage/token` | 金额型 | 500000 积分 = ¥1 |
| OpenCode Go | `GET https://opencode.ai/zen/go/v1/usage` | 配额型 | 只返回百分比 |
| DeepSeek | `GET https://api.deepseek.com/user/balance` | 金额型 | 直接取 total_balance |
| Hapi | `GET https://ai.yuchuantest.com/v1/usage` | 金额型 | 20 积分 = ¥1 |

### 5. 单元测试(14 个,全部通过)

```
providers::tests::four_s_api_converts_points            ok
providers::tests::four_s_api_falls_back_when_fields_missing  ok
providers::tests::opencode_picks_tightest_window        ok
providers::tests::opencode_reports_entitlement_error    ok
providers::tests::deepseek_splits_balance               ok
providers::tests::hapi_divides_points_by_20             ok
providers::tests::hapi_error_is_surfaced                ok
store::tests::consumption_sums_decreases_and_ignores_topups  ok
store::tests::consumption_is_none_without_baseline      ok
store::tests::settings_roundtrip                        ok
fetch::tests::month_start_is_first_day                  ok
fetch::tests::week_start_is_monday                      ok
fetch::tests::today_start_is_before_now_and_same_day    ok
fetch::tests::week_start_not_after_today_start          ok
```

---

## 关键设计决策(后续改动请勿破坏)

### 取数失败绝不能降级成 0

余额类工具最危险的 bug 是把"请求失败"渲染成"余额归零",用户会以为欠费。
所以:取数失败时沿用上次成功快照 + 标注时间 + 显示真实错误原因;
`remaining` 取不到时一律返回 `null` 而非 `0`。测试 `four_s_api_falls_back_when_fields_missing`
就在锁这个行为。

### 两类渠道分开渲染

- **金额型**(4SAPI / DeepSeek / Hapi):显示货币余额、可画进度条
- **配额型**(OpenCode Go):接口只给已消耗百分比,**不参与消耗合计**(百分比和金额不能相加),
  行内三格改为 5 小时 / 本周 / 本月三个限流窗口的剩余

充值型渠道(DeepSeek / Hapi)没有"限额"分母,所以**不画进度条**。

### 消耗是推算值

DeepSeek / Hapi 的接口只给当前余额、不给累计消耗,日/周/月消耗靠本地快照差值算:

```
consumption(t0,t1) = Σ max(0, remaining[i-1] - remaining[i])
```

负差值(充值、配额重置)clamp 成 0。**窗口起点没有基线时返回 `null` 而不是 0**
(测试 `consumption_is_none_without_baseline` 锁定),界面显示"—— / 数据不足"。
程序未运行的时段形成数据空洞,数字偏小,界面已标注"推算"。

### 取数不消耗 token

四个接口都是账务/元数据接口,不是推理接口,查询余额**不产生任何 token 计费**。
所以轮询间隔可以压得比较短:展开 60s / 折叠 300s / 连续失败退避 900s,带抖动。

### 安全

- 密钥只写 Windows 凭据管理器(服务名 `TokenScope`,账户名 = 渠道 id)
- 密钥**永不**进 JSON / SQLite / 日志 / 导出文件;配置可随便备份分享
- 出网请求只发往渠道自身域名,无遥测、无云同步
- `.gitignore` 已排除 `*.db`(快照含余额与用量历史)

---

## 已解决:前端布局塌陷(2026-09-16 定位并修复)

根因是**两个 bug 叠加**,都不是技术栈问题:

1. **`app.js` 用了 Tauri v1 的 API 位置**:`new T.window.LogicalSize(w, h)` 在
   Tauri 2 里抛 `is not a constructor`(v2 把尺寸类型移到了 `dpi` 命名空间,
   正确写法是 `new T.dpi.LogicalSize(...)`),而 `applyForm` 的 try/catch 把
   异常静默吞掉 —— **窗口尺寸从启动起就从未改变过**,永远停在 380×560。
2. **数据库里持久化了 `form: "compact"`**:此前测试时点过"切换紧凑条"按钮,
   `Config::load` 从 SQLite 读回 compact,启动时 body 被设为 `form-compact`,
   而尺寸切换又因上一条失败 —— 于是精确复现"面板尺寸的窗口 + 紧凑条的内容"
  (横向排列、竖排文字、横向滚动条)。

此前排查全部落空的原因:排查第 1 步的 Node mock 里 `get_config` 返回的是**默认值**
(panel),掩盖了数据库里的真实值;排查第 4 步验证的是 `Config::default()` 而非
数据库实存值 —— 两步都测错了对象。

修复内容(`app.js` / `styles.css`):

- `new T.dpi.LogicalSize(...)` 改正命名空间,catch 里已有 console.error
- 紧凑条 / 胶囊形态先 `setMinSize(null)` 再缩放(否则 46px 高度被
  `minHeight: 120` 钳住);回面板时恢复 `setMinSize(320×120)`
- 删除 `styles.css` 里 `body.form-pill{background:transition}` 非法声明
- **数据库无需清理**:修复后持久化的 compact 会以正确的 380×46 渲染,
  点标题栏按钮即可切回面板

`build/cdp-probe.js` / `launch-debug.ps1` 保留,仍是排查 WebView2 运行时
DOM 的有效工具。

### 运行时验证才发现:还有第二层根因 —— ACL 权限集为空(2026-09-16 修复)

上一轮的修复(命名空间 + minSize 顺序)是必要的,但**不充分**。真正让
"窗口尺寸永不变化"的还有 Tauri 2 的 ACL:

- 项目里**没有 `capabilities/` 目录**,`tauri.conf.json` 也没声明 capability,
  构建后解析出的权限集是空 `{}`(见 `gen/schemas/capabilities.json`);
- Tauri 2 默认不授予任何核心命令权限,前端 `appWindow.setSize` /
  `setMinSize` / `setResizable` / `setAlwaysOnTop`(以及标题栏
  `data-tauri-drag-region` 的 start-dragging)全部被 IPC 拒绝;
- `applyForm` 的 `try/catch` 把拒绝异常静默吞掉 —— 现象与命名空间笔误
  **完全一样**(DOM 切了、窗口没动),所以纯静态排查看不出区别;
- 应用自己的 `#[tauri::command]`(`window_cmd` 等)**不受 ACL 约束**,
  这也解释了为什么"部分窗口操作看起来是好的"。

修复:新增 `app/src-tauri/capabilities/default.json`,授予 `core:default` +
`core:window:allow-set-size` / `allow-set-min-size` / `allow-set-resizable` /
`allow-set-always-on-top` / `allow-start-dragging`。

**验证(新增 `build/verify-form.js`)**:通过 CDP 在真实窗口上调 `applyForm`,
断言 body class 与真实窗口尺寸同步变化:

```
compact  body=form-compact tbar=none   window=380x47  expected=380x46   PASS
pill     body=form-pill    tbar=none   window=200x47  expected=200x46   PASS
panel    body=form-panel   tbar=flex   window=380x560 expected=380x560  PASS
```

(47 而非 46:dpr=1.25 下物理像素取整,±1 属正常。)

教训记一笔:**"改完没在真实进程上跑过"的修复等于没修**。这一轮的形态切换、
注册表同步都是实测通过后才写"已完成"。

## 界面改版:固定尺寸 / 视觉对齐设计稿 / 设置与管理(2026-09-16)

三条需求一次落地,全部做了运行时验证(截图见对话,断言在
`build/verify-form.js`)。

### 1. 所有形态固定尺寸,不能拖拽改变大小

- `tauri.conf.json`:`resizable:false`、`maximizable:false`(挡掉 Win+↑ 吸附),
  移除 minWidth/minHeight(尺寸由 JS 按形态设定,避免启动瞬间的旧约束打架)。
- `applyForm` 里每次切换都 `setResizable(false)` 并设置 min=max=当前尺寸。
- **机制的真相(实测)**:`setMinSize`/`setMaxSize` 调用了也成功返回,但
  Windows 对**程序化** `SetWindowPos` 不套用 tracking 约束 —— 在控制台里
  调 `setSize(500,600)` 窗口真的会变成 500×600。所以真正挡住用户拖拽的是
  **窗口样式里没有 `WS_THICKFRAME`**(缩放边框)和 `WS_MAXIMIZEBOX`。
  `build/win-style.ps1` 读 `GWL_STYLE` 做这条断言(实测 `0x14ca0000`,
  两个位都不在)。min/max 保留作兜底,但别再把它当成固定尺寸的机制。
- 拖标题栏移动窗口不变(`data-tauri-drag-region` 保留)。

### 2. 视觉按原型稿对齐

- 窗口改为 `transparent:true` + `shadow:false`:body 自己就是那张卡片,
  面板圆角 12 / 紧凑条 10 / 胶囊 999 —— 圆角外是真透明,不再有窗口底色露角。
- 紧凑条按设计稿做成 chip 条:底色 `--bg3`、只留「状态点 + 数值」、
  金额舍掉小数位而百分比保留 `%`;**放不下的渠道收进 `+N` 徽标**
  (`fitCompact()` 实测溢出后隐藏尾部,与设计稿的截断规则一致)。
- 胶囊对齐设计稿:mark + 状态点 + 「OC 1.3%」单行,悬停显示完整渠道名。
- 未配置任何密钥时不再铺一列灰行,改为居中的引导卡(钥匙图标 +
  「去配置密钥」按钮,直接进管理页)。

### 3. 「设置与管理」页:密钥与设置同页,和展示区分开

- 原来设置是一个下压面板、密钥藏在每个渠道的展开区里 —— 两处分散,
  且"管理密钥"入口实际是展开第一行,名不副实。
- 现在是一个**独立视图**(`body.view-manage`):顶部「← 返回 / 设置与管理」,
  内容依次为 密钥(每渠道:图标、名称、状态点、删除、输入框、保存)、
  刷新、显示、阈值、启动。展示区不放任何设置项。
- 行详情里不再放密钥输入,只留一句「密钥、刷新与阈值在『设置与管理』里配置」
  的入口链接。
- 入口:标题栏 ⚙、底栏「设置与管理」、空态引导按钮、行详情链接;
  Esc / ← 返回关闭。

### 顺带修掉的真 bug:hasKey 字段名不匹配(界面一直误报"未配置")

验证密钥保存回路时发现:写入假 key 后 `get_channels` 明明有数据(还发了
请求,报"无余额信息"),但界面状态仍是"未配置"。

根因:Rust 的 `ChannelView` 上有 `#[serde(rename_all = "camelCase")]`,
JSON 字段是 **`hasKey`**,而前端 14 处全在读 `c.has_key` → 恒为 `undefined`
→ `toneOf` 永远 "off"、汇总条永远为空、底栏永远"未配置渠道"、胶囊永远
"未配置"。**界面从第一天起就没显示过真实状态**,只是空 key 状态下
两者长得一样,所以没被发现。

修复:前端统一改为 `c.hasKey`。教训:跨语言边界的字段名要有一处契约检查 ——
这次是靠"保存后状态没变"这个反常现象才挖出来的。

### 追加调整(2026-09-16,按用户反馈;第 3 条交互方案经用户确认)

1. **OpenCode Go 按三个限额窗口展示**(用户确认的"三行窗口 + 迷你条"方案):
   大数字仍是「最紧窗口的剩余%」,副标注点名是哪个窗口(`本月窗`);
   下方 5 小时 / 本周 / 本月 三行,每行「标签 + 迷你条 + 剩余%」,
   迷你条按阈值染色、`rate-limited` 的窗口整行标红。
   配额型不再画顶部大条(三条迷你条已表达余量,避免重复)。
2. **金额型渠道不再解释"充值余额 · 无限额"**:DeepSeek / Hapi 这类
   没有分母的渠道,拿到多少就是可用多少,副标题与"可用"标签一律去掉;
   4SAPI 的「额度 ¥500 / 剩 37%」保留 —— 那是真实存在的分母,不是说明文字。
   (4SAPI 的兜底分支不带 total,此时同样不显示任何说明。)
3. **紧凑条补齐形态切换入口**(用户确认的"两个图标按钮"方案):
   条尾 ▲ 展开为面板、● 收成胶囊;两个方向都可见。
   按钮占位后 chip 区变窄,`+N` 徽标会自动多收一个,属预期。

验证(CDP 注入仿真数据后断言,全部 PASS):OC 窗口行数=3、大条已移除、
副标注=最紧窗口名;金额型副标题/标签为空;▲ → 面板 380×560、
● → 胶囊 200×47、按钮只在紧凑条出现。`verify-form.js` 全项回归通过。

### 验证工具(本轮新增/扩展)

- `build/verify-form.js`:三形态切换 + `resizable` + Win32 样式位(固定尺寸)。
- `build/win-style.ps1`:读运行中窗口的 `GWL_STYLE`(ASCII-only)。
- 现场注入仿真数据(`CHANNELS = [...]; render()`)后用 CDP
  `Page.captureScreenshot` 出 2x 截图 —— 不需要真 key 就能逐形态检查渲染。

## 开机自启(2026-09-16 新增)

设计约束:绿色单文件 exe、无管理员权限。

- Rust 命令 `set_autostart`:winreg 写 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`,
  值为 `"当前exe完整路径" --hidden`
- **每次启动 setup 里按 `current_exe()` 重写注册表** —— exe 被移动位置后自启
  依然指向新位置,无需重新设置;配置关闭时删除该键
- `--hidden` 参数:开机自启直接进托盘不弹窗。窗口配置改为 `visible: false`,
  由 setup 统一控制显隐(正常启动立即 show)
- 前端开关(`bindSwitch` 增加 onChange 回调):注册表写失败会回滚开关与配置,
  不让界面显示与系统实际状态不一致
- **实测(2026-09-16)**:
  - 写路径:调用 `set_autostart(true)` 后注册表键值确为
    `"G:\...\target\release\tokenscope.exe" --hidden`(引号 + 隐藏参数正确);
  - 启动同步:重启应用后因配置为关,该键被 setup 里的同步逻辑自动删除 ——
    "配置为准"的行为符合设计。

## 进行中:单 exe 的最后一个阻塞 —— WebView2Loader.dll

### 现象与误判

把 `tokenscope.exe` 单独拷进空目录启动,弹系统错误对话框:
`由于找不到 WebView2Loader.dll,无法继续执行代码`。
**教训**:首次测试时 `Start-Process` 报告 "RUNNING" 是假象 —— 进程卡在
错误对话框上没退出,被当成了启动成功。判断 GUI 程序是否真的起来,
必须看窗口/日志,不能只看进程存活。

### 根因(已定位到源码行)

`webview2-com-sys 0.38.2` 的 `src/lib.rs`:

```rust
#[cfg_attr(target_env = "msvc",
    link(name = "WebView2LoaderStatic", kind = "static"))]
#[cfg_attr(not(target_env = "msvc"),
    link(name = "WebView2Loader.dll"))]
```

MSVC 工具链静态链接加载器;**GNU 工具链(Free 约束下的选择)动态导入**。
Tauri dev 运行时把 160 KB 的 `WebView2Loader.dll` 放在 exe 旁边所以一直没暴露。
objdump 确认 exe 导入表:`KERNEL32 / advapi32 / ws2_32` 等全是系统 DLL,
唯一非系统项就是 `WebView2Loader.dll`。WebView2 Runtime 自带这个 DLL,
但不在标准搜索路径上,救不了加载期导入。

### 实验:强行静态链接(GNU ld + WebView2LoaderStatic.lib)

用 rustc 最小程序直接链 MSVC 格式的静态库 —— **失败**,未定义符号共 20 种
(59 KB 报错去重统计):

| 符号 | 次数 | 性质 | 可解性 |
|---|---|---|---|
| `__security_cookie` / `__security_check_cookie` | 43+21 | MSVC /GS 栈保护 | 可垫(shim 全局 + no-op 校验) |
| `_Init_thread_header/_footer/_epoch` | 7×3 | MSVC 魔法静态初始化线程同步 | **危险**:语义与编译器紧耦合,垫错有并发隐患 |
| `__imp_RegOpenKeyExW` 等 advapi32/ole32 | 11 | 常规 API 导入 | 易:补 `-ladvapi32 -lole32` |
| `??2@`/`??3@`/`??_U@`/`??_V@`/`std::nothrow` | 5 | MSVC C++ ABI 的 operator new/delete | 可用 asm 别名垫到 malloc/free,但已在补 ABI 缝隙 |

### 三个方案(待定夺)

| 方案 | 单 exe? | 风险 | 备注 |
|---|---|---|---|
| **A. exe + DLL 双文件** | ✗(2 文件) | 零 | Tauri 对 GNU 的官方行为;DLL 仅 160 KB、微软允许再分发;依旧免安装绿色,可打成 zip |
| B. 换 MSVC 工具链 | ✓ | 无(官方路径) | 需管理员权限装 VS Build Tools,**本机装不了**(Free 约束的由来) |
| C. 符号垫片强行静态链接 | ✓ | 中高 | 上表 20 个符号可垫齐,但 `_Init_thread_*` 并发语义最难对齐;WebView2Loader 内部是 MSVC C++,公共 API 是 C 边界,理论上自洽,需充分回归 |

**推荐 A**:交付 `tokenscope.exe + WebView2Loader.dll`(或 zip 打包),
把工程资源留给产品本身;将来若能上 MSVC 工具链,自然升级成真单文件。

### 顺带的验证结论(不受阻塞影响)

- exe 体积 **5.24 MB**,VersionInfo 正确(TokenScope 0.1.0)
- 导入表其余全是系统 DLL(api-ms-win-* 是 UCRT),无其他第三方依赖
- SQLite / native-tls / keyring 均静态编入,单 exe 约束只差这一个加载器

---

## 环境与构建

**仓库已迁到纯英文路径 `G:\game\nw\Grandettoken`**,不再镜像、不再重定向
CARGO_TARGET_DIR:构建直接在仓库里跑,产物落在 `app/src-tauri/target`。
本机**无管理员权限**,工具链装在用户目录:

| 组件 | 路径 |
|---|---|
| Rust GNU 工具链 | `%USERPROFILE%\.cargo\bin`(rustc 1.98.1) |
| MinGW-w64 14.2.0 | `%USERPROFILE%\mingw64`(GNU 工具链自身也要求 ASCII 路径,故不进仓库) |
| git | 系统安装的 2.55.0.windows.3(`build/git` 便携版已不存在,无需再装) |
| 构建产物 | `app/src-tauri/target`(仓库内,已被 .gitignore 覆盖) |

> 迁移时把旧的重定向 target 缓存(1.9 GB)搬进了 `app/src-tauri/target`
> 复用,依赖产物大多命中;`%USERPROFILE%\tokenscope-build` 镜像与
> `%USERPROFILE%\.cargo\target` 已删除,用户目录不再有本项目文件。

```powershell
.\build\install-toolchain.ps1   # 一次性:Rust + MinGW(约 250MB 下载)
.\build\install-git.ps1         # 一次性:PortableGit
.\build\build.ps1 test          # 跑单元测试(只链接 core,秒级)
.\build\build.ps1 check         # 检查 Tauri 侧编译
.\build\build.ps1 build         # 出 release 二进制(约 8 分钟)
```

### 已知构建坑

- **构建前先退出正在运行的应用**。否则链接器写 `target/release/tokenscope.exe`
  时被占用,报 `另一个程序正在使用此文件 (os error 32)`,错误信息落在
  build script 输出里,不看完整日志容易误判成编译错误。
- **仓库路径必须保持纯 ASCII**(历史坑,现由 build.ps1 快速失败兜底):
  GNU binutils 打不开非 ASCII 路径的输入文件 —— ld 读不了 .o/.rlib,
  windres 打不开 icon.ico(路径显示为 GBK 乱码,`can't open icon file`)。
  早前仓库在中文路径下时,试过 target 重定向 + robocopy 镜像 + junction
  (junction 会被 Rust `canonicalize()` 看穿,无效),最终靠镜像绕过。
  现在仓库已是 ASCII 路径,镜像逻辑已删除;若将来路径又含中文,
  build.ps1 会直接报错而不是产出难懂的链接错误。
- **不要把 cargo target 缓存直接搬到新路径复用**(本轮踩坑):
  cargo 的新鲜度指纹不包含 target 目录位置,搬过来的 build script
  `output` 里缓存的 `DEP_*` 绝对路径仍指向旧目录,表现为
  `tauri` 读权限文件时 `os error 3` 失败,或最终链接找不到
  webview2 / sqlite 的 link-search。受影响包(实测):
  `tauri` / `tauri-plugin-opener` / `webview2-com-sys` / `libsqlite3-sys`
  (其中 `windows_x86_64_gnu` 的路径指向 registry,稳定,无需处理)。
  修法:`cargo clean -p <包> --release` 后重编即可,不必全量 clean。
- **`crate-type` 不能带 `cdylib`**。Tauri 模板默认 `["staticlib","cdylib","rlib"]`
  是为移动端,但 Windows GNU 工具链下 cdylib 会导出全部符号,依赖树规模超过
  DLL 导出序号上限,报 `export ordinal too large: 127822`。桌面端已改为 `["rlib"]`。
- **PowerShell 脚本必须纯 ASCII**。本机 PowerShell 按 GBK 读 `.ps1`,
  UTF-8 中文注释会变乱码并破坏字符串解析。所有 `build/*.ps1` 因此只写英文。
- **`.ps1` 里不要用 `->`、`$var` 嵌套转义**,容易触发解析错误,复杂逻辑写成脚本文件跑。
- **下载慢时走代理** `http://127.0.0.1:7897`(用户提供),实测比直连快约 5 倍
  (14 MB/min vs 2.9 MB/min)。**仅在下载依赖时需要,应用运行时不需要。**

---

## Git

- 仓库:`git@github.com:wacilimonster-source/Grandettoken.git`
- 首次推送已成功(commit `272e791`),33 个文件
- 推送时用的是 HTTPS + 临时 token,已确认 **token 未写入 `.git/config`、未落在任何文件**;
  `origin` 仍保持用户指定的 SSH 地址
- 已验证 `.git/config` 无凭据;全盘扫描 33 个文本文件,token 出现 0 次

> ⚠️ **安全提醒**:用于推送的 PAT 曾以明文出现在对话里,建议到
> https://github.com/settings/tokens 撤销并重新签发。它当前是 fine-grained token,
> 需要 `Contents: Read and write` 权限才能推送。

---

## 尚未实现 / 待验证

- ~~布局修复的运行时验证~~ ✅ 已完成(2026-09-16):`build/verify-form.js`
  三形态全部 PASS,并顺带挖出 ACL 第二层根因(见上)
- ~~开机自启的注册表写入验证~~ ✅ 已完成(2026-09-16):写入格式正确,
  启动同步(配置关则删键)实测通过
- ~~固定尺寸 / 视觉对齐 / 设置与管理~~ ✅ 已完成(2026-09-16,见「界面改版」)
- **待定夺**:单 exe 交付方案 A/B/C(见「进行中」章节)
- 屏幕边缘吸附与自动折叠(原型里有设计,代码里 `collapse_on_blur` 开关未接线)
- 边缘吸附形态(原型里有设计;胶囊形态已实测通过)
- 托盘图标角标显示最紧张渠道的百分比(当前是静态图标)
- 余额低于阈值的系统通知
- NSIS 安装包(`build.ps1 bundle` 未跑过;若选双文件交付,NSIS 仍可作为分发形态)
- **待观察**:一个修复前的旧实例在运行 3~5 分钟后进程消失,当时没捕获 stderr,
  原因未知(不排除是手动关闭)。此后多个实例(含多次形态切换、管理页操作、
  多个轮询周期)稳定;`launch-debug.ps1` 现在把 stdout/stderr 落到
  `%TEMP%\tokenscope-debug\`,若再复现直接看 stderr.log
- ~~从紧凑条/胶囊切回面板后 alwaysOnTop 不复位~~ 已解决:形态切换不再
  强制置顶(改版时删掉了 `setAlwaysOnTop(true)`),置顶只由标题栏 📌 控制
- **待真机确认**:真实密钥下的完整展示效果(本轮用仿真数据验证渲染;
  当前机器上四个渠道都还没配密钥)
