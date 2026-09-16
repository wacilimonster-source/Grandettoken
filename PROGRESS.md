# TokenScope 开发进展

> 最后更新:2026-09-16
> 状态:**可编译、可运行、逻辑层已测试通过;前端渲染有一个未解决的布局缺陷**

## 一句话现状

Rust 侧全部完成并通过 14 个单元测试,应用能编译成 5MB 的单文件 exe 并启动,
IPC 正常(能从 Rust 取到渠道数据并渲染出渠道行);
**但前端布局塌陷成"紧凑条"样式,尚未定位,见下方「未解决问题」**。

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

## 未解决问题(下次继续)

### 前端布局塌陷成"紧凑条"样式

**现象**:应用启动后,窗口是面板尺寸(380×560),但内容按"紧凑条"形态渲染 ——
标题栏不可见、渠道行横向排列、空态文字被挤成竖排、列表底部出现横向滚动条。

**已排除的可能**(都实际验证过):

1. ❌ **不是 JS 逻辑问题**。用 Node 加载真实的 `app.js`、mock DOM 和 `__TAURI__` 跑了一遍,
   最终 `document.body.className` 是 **`"form-panel"`**(正确),且无任何异常。
   诊断脚本留在 `build/probe-frontend.js`,可重复运行。
2. ❌ **不是 CSS 写错**。`styles.css` 里 `body.form-compact ...` 选择器书写正确,
   且只匹配 `form-compact`,不会误匹配 `form-panel`。
3. ❌ **不是二进制里的前端过期**。`tokenscope.exe` 的修改时间(16:11)晚于
   `app.js`(15:16)、`styles.css`(15:15),Tauri 在编译期打包前端,时间上不可能落后。
4. ❌ **不是 Rust 侧返回的配置不对**。`Config::default()` 里 `form: "panel"`。

**矛盾点**:JS 说 class 是 `form-panel`,但渲染结果只能由 `body.form-compact` 解释
(只有那条规则会把 `.tbar/.sum/.foot` 设为 `display:none`、把 `.list` 设为
`display:flex; flex-direction:row; align-items:center` —— 正好对应观察到的
"横向排列 + 垂直居中 + 横向滚动条")。理论推断和实际渲染对不上,尚未找到原因。

**下一步建议**(按推荐顺序):

1. **用 WebView2 CDP 直接查运行中的 DOM**(最推荐)。
   `build/cdp-probe.js` 已经写好,它通过 `--remote-debugging-port=9222` 连接,
   能读出真实的 `document.body.className`、各元素的 `getComputedStyle`、
   以及 `document.styleSheets` 是否加载成功。
   配合 `build/launch-debug.ps1`(用 `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`
   环境变量开调试端口)使用。**上次任务就是停在这一步之前。**

   注意:`document.title` 改不动窗口标题(Tauri 不同步),别再用那个办法做诊断。

2. 若 CDP 显示 class 确实是 `form-panel` 却仍渲染成紧凑条,
   则检查 `styles.css` 是否真的被加载(`document.styleSheets` 里应有 1 张表且
   `cssRules.length > 0`)。Tauri 用自定义协议提供资源,若 CSS 静默 404,
   页面会呈现"部分样式生效"的假象。

3. 检查是否存在 WebView2 资源缓存导致的旧版本混合。

**临时绕过**:如果要先看面板形态的效果,可以直接改 `index.html` 里
`<body class="form-panel">`,或在 `app.js` 的 `applyForm` 里把 class 写死。

---

## 环境与构建

本机**无管理员权限**,工具链全部装在用户目录:

| 组件 | 路径 |
|---|---|
| Rust GNU 工具链 | `%USERPROFILE%\.cargo\bin`(rustc 1.98.1) |
| MinGW-w64 14.2.0 | `build/mingw64/bin` |
| PortableGit 2.55 | `build/git/cmd` |

```powershell
.\build\install-toolchain.ps1   # 一次性:Rust + MinGW(约 250MB 下载)
.\build\install-git.ps1         # 一次性:PortableGit
.\build\build.ps1 test          # 跑单元测试(只链接 core,秒级)
.\build\build.ps1 check         # 检查 Tauri 侧编译
.\build\build.ps1 build         # 出 release 二进制(约 8 分钟)
```

### 已知构建坑

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

## 尚未实现

- 开机自启注册(设置里有开关,但没接系统 API)
- 屏幕边缘吸附与自动折叠(原型里有设计,代码里 `collapse_on_blur` 开关未接线)
- 胶囊 / 边缘吸附两种窗口形态(前端有 CSS,`applyForm` 有分支,但没实测过)
- 托盘图标角标显示最紧张渠道的百分比(当前是静态图标)
- 余额低于阈值的系统通知
- NSIS 安装包(`build.ps1 bundle` 未跑过)
