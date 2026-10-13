# TokenScope

常驻 Windows 桌面的多平台 AI Token 余额与用量监控小组件。支持四级折叠:完全展开 → 紧凑条 → 胶囊 → 吸附屏幕边缘。

## 当前渠道

| 渠道 | 接口 | 类型 | 说明 |
|---|---|---|---|
| 4SAPI | `GET https://4sapi.com/api/usage/token` | 金额型 | 积分制,500000 积分 = ¥1 |
| OpenCode Go | `GET https://opencode.ai/zen/go/v1/usage` | 配额型 | **未公开接口**,只返回百分比 |
| DeepSeek | `GET https://api.deepseek.com/user/balance` | 金额型 | 充值型,无月限额 |
| Hapi | `GET https://ai.yuchuantest.com/v1/usage` | 金额型 | 积分制,20 积分 = ¥1 |

### 两类渠道的区别

**金额型**有 `remaining` / `used` / `total`,可以算钱、推"预计可用天数"、画进度条。
**配额型**(OpenCode Go)接口只给已消耗百分比和重置时间,无法折算金额,所以:

- 卡片主数值显示百分比而非金额
- 行内三格换成 5 小时 / 本周 / 本月三个限流窗口的剩余配额
- **不参与底部消耗合计** —— 百分比与金额不能相加

### 充值型渠道的消耗是推算值

DeepSeek 和 Hapi 的接口只返回当前余额,不含累计消耗。今日/本周/本月消耗由本地
SQLite 快照的余额差值推算:

```
consumption(t0,t1) = Σ max(0, remaining[i-1] - remaining[i])
```

负差值(充值、配额重置)被 clamp 成 0。**代价是程序未运行的时段形成数据空洞,
数字会偏小**;窗口起点无基线时返回 `null` 而非 `0`,界面显示"—— / 数据不足",
绝不把缺失数据渲染成"没花钱"或"已耗尽"。

## 取数不消耗 token

四个接口都是账务/元数据接口,不是推理接口 —— 查询余额不产生任何 token 计费。
所以可以放心定时轮询,唯一约束是对方的限流礼貌性而非费用。

轮询策略(可在设置里调):

- 面板展开:1 分钟
- 折叠 / 托盘:5 分钟
- 连续失败 3 次后:退避到 15 分钟
- 带 5 秒抖动,避免多渠道同秒并发

## 密钥存储

界面明文输入,不做二次确认、不设主密码。落盘写入 **Windows 凭据管理器**
(服务名 `TokenScope`,账户名为渠道 id),由当前 Windows 账户的登录凭据保护。

密钥**永不**写入 JSON、SQLite、日志或导出文件,配置可以随便备份或分享。
出网请求只发往渠道自身域名,无遥测、无云同步。

## 构建

```powershell
.\build\install-toolchain.ps1   # 一次性:装 Rust GNU 工具链 + MinGW(免管理员)
.\build\build.ps1 test          # 跑单元测试
.\build\build.ps1 build         # 出 release 二进制
.\build\build.ps1 bundle        # 出 NSIS 安装包
```

工具链装在用户目录,不需要管理员权限。Windows 上走系统 SChannel 做 TLS,
避免 rustls 的 ring/aws-lc 汇编依赖在 GNU 工具链下需要 nasm 的问题。

## 结构

```
app/
  core/                   纯逻辑层,不依赖 Tauri(可独立跑测试)
    src/
      providers.rs        渠道定义 + 响应解析(带单元测试)
      store.rs            SQLite 快照与消耗推算(带单元测试)
      secrets.rs          Windows 凭据管理器封装
      config.rs           设置项(含申请制额度、胶囊渠道、自定义排序)
      fetch.rs            取数、失败降级、视图组装(带单元测试)
  src/                    前端(纯静态,无打包器)
    index.html
    styles.css
    app.js
    logos/                渠道官方图标(来源与版权见该目录 README)
  src-tauri/
    src/
      lib.rs              命令、托盘、轮询调度、开机自启、置顶
    capabilities/         Tauri 2 ACL:窗口尺寸/位置/置顶等权限
    build.rs              链接静态 WebView2 加载器(单 exe 交付)
    tauri.conf.json
    icons/
build/
  install-toolchain.ps1   免管理员工具链安装
  build.ps1               构建入口(含单 exe 的静态加载器准备)
  make-webview2-static.ps1 把 MSVC 静态加载器 + CRT 垫片重打成 GNU 归档
  webview2-static/        垫片源码(msvc-shim.c / msvc-alias.S)
  verify-form.js          三形态 / 固定尺寸 / 圆角透明的 CDP 断言
  win-style.ps1           读运行中窗口的 Win32 样式位
  launch-debug.ps1        带调试端口启动,stdout/stderr 落盘
  cdp-probe.js            读运行中实例的 DOM 状态
  gen-icons.js            图标生成(手写 PNG/ICO,无依赖)
```

## 加新渠道

1. 在 `app/core/src/providers.rs` 写一个 `extract_xxx(&Value) -> FetchResult`
2. 加一条 `ProviderDef` 到 `PROVIDERS` 数组
3. 加一个 `#[test]` 覆盖正常返回和错误分支

前端会按 `kind` 自动选择渲染方式,无需改动。

## 模型页(多来源)+ 一键配置到客户端

### 三个来源

| 来源 | 列表接口 | 价格 | 列表公开 |
|---|---|---|---|
| 4SAPI | `/v1/models` + `/api/pricing` | 有 | 价目表公开 |
| Hapi | `/v1/models` | 无(站点不提供) | 否(要密钥) |
| OpenCode Go | `/zen/go/v1/models` | 无(订阅制) | 是 |

来源定义在 `core/src/models.rs` 的 `SOURCES`。加一个来源 = 加一条 `SourceDef`
(列表路径 / 是否有价目表 / 写进客户端的 base_url / 判定用的域名),前端芯片是后端返回的,不用改。

**「已配置」按来源判定**:客户端条目里的 `url` 命中哪个来源的域名就属于哪个来源 ——
同一个模型名可能在多个来源里都有,只看 id 会串台。



「模型」页签列出 4SAPI 的可用模型,并把选中的模型写进本机的 Trae / WorkBuddy / WorkBuddy AI / ZCode。

两个**元数据接口**(不产生 token 计费):`GET /api/pricing` 是公开价目表(160 个模型,含说明、
标签、厂商、倍率、支持分组),`GET /v1/models` 带密钥返回**这把 key 真能调**的集合。写进客户端的
接入地址优先用"实测接受过这把密钥"的域名。

单价口径与站点自己的前端逐行一致(见 `DESIGN.md` 12.2):

```text
输入单价(元/百万 token) = 2 × model_ratio × 分组倍率
输出单价                = 输入单价 × completion_ratio
```

### 写入四条共同前提(缺一不写)

1. **目标进程不在运行** —— 它们的配置是内存态落盘,边跑边写会被覆盖(Trae 的 SQLite 还可能写坏);
2. **先备份** —— `<配置目录>/backup-inject/<时间戳>/`,含 `-wal/-shm`,保留最近 5 份;
3. **只增改自己写的那条** —— 不碰账号、登录态、内置模型、别人的 provider;
4. **写完读回校验** —— 不过就自动回滚到备份并如实报错。

**取消写入**(写入的逆操作)走同一套规矩:`inject::remove()` 只把这一条删掉 ——
其余条目与用户的手改都不动(这与「回滚这次写入」把整个文件盖回备份是两件事,两个都保留)。
Trae 是每个 agent 组各一条;客户端的选中项若正指向这条模型会一并清掉并在预告里说明;
ZCode 删到最后一个模型时连 provider 与它的 providerRule 一起收走。

四份契约在 `core/src/inject.rs`,每个客户端一张表:WorkBuddy 是明文 JSON 数组(带 BOM、2 空格),
ZCode 是两份 JSON(`schemaVersion` 不是 1 就放弃),Trae 是 SQLite 的 `ItemTable`(按后缀找键名,
键前缀是区域/租户号,会随版本变)。

### 加一个新客户端

1. `inject.rs` 的 `Client` 枚举加一项(短标签 / 名字 / id / 进程名);
2. `sites()` 里加落点检测(配置目录存在才返回);
3. 写一个 `write_xxx_site(&Site, &ModelSpec, ...) -> Result<SiteOutcome, String>`,
   落盘走 `commit()`(备份 → 写 → 读回校验 → 失败回滚),不要自己拼;
4. `preview()` / `state()` 各加一个分支(预告要说清"改哪个文件的哪一条");
5. 前端的 `CLIENT_NAME` 加一行,模型行上的按钮是**按 `CLIENTS` 状态动态渲染**的,不用改。

**规矩**:认不出来的结构就放弃(不写、不改、不猜);密钥只从凭据管理器现读现写,
不落盘、不进日志、不进备份。

## 已知限制

- OpenCode Go 的接口未公开,无稳定性保证,响应结构变化时该渠道会显示取数失败。
  渠道定义里标了 `unstable: true`,便于界面上区分。
- OpenCode Zen 的按量付费 credits 余额没有公开接口,只能在网页控制台查看。
- 开机自启已接通系统 API(winreg 写 HKCU Run 键,启动时按当前 exe 路径重写);
  贴边吸附也已落地(拖到屏幕左/右边缘松手即吸附,鼠标移入展开、移出 1 秒收回)。
- 单 exe 交付:exe 静态链接 WebView2 加载器,不再需要旁边的 `WebView2Loader.dll`
  (见 `app/src-tauri/build.rs` 与 `build/make-webview2-static.ps1`)。
