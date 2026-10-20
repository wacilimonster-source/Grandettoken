# Grandettoken 开发者说明

> 产品早期叫 **TokenScope**,2026-09 下旬改名 Grandettoken。凭据服务名(`TokenScope`)
> 与 bundle identifier(`com.wacil.tokenscope`)仍沿用旧名 —— 改了会让已装用户收不到更新、
> 凭据条目全丢。**新代码一律写 Grandettoken,旧名只在这两处出现。**

常驻 Windows 桌面的多平台 AI Token 余额与用量监控小组件。支持四级折叠:完全展开 → 紧凑条 → 胶囊 → 吸附屏幕边缘。

安装 / 使用 / 下载入口看[根 README](../README.md),产品设计看 [DESIGN.md](../DESIGN.md),
进展与踩坑看 [PROGRESS.md](../PROGRESS.md),本文只讲代码内部的事。

## 当前渠道

| 渠道 | 接口 | 类型 | 说明 |
|---|---|---|---|
| 4SAPI | `GET https://4sapi.com/api/usage/token` | 金额型 | 积分制,500000 积分 = ¥1 |
| OpenCode Go | `GET https://opencode.ai/zen/go/v1/usage` | 配额型 | **未公开接口**,只返回百分比 |
| DeepSeek | `GET https://api.deepseek.com/user/balance` | 金额型 | 充值型,无月限额 |
| Hapi | `GET https://ai.yuchuantest.com/v1/usage` | 金额型 | 积分制,20 积分 = ¥1 |
| Trae | `POST https://api.trae.cn/.../user_current_entitlement_list` | 积分型 | 读本机登录态;两端点互为备份 |
| WorkBuddy | `POST https://www.codebuddy.cn/v2/billing/meter/get-user-resource` | 积分型 | 读桌面端凭据,accessToken 到期自动 refresh |
| Codex | `GET https://chatgpt.com/backend-api/wham/usage` | 配额型 | 读本机 Codex 登录态;5 小时/周/月三个窗口 |

### 三类渠道的区别

**金额型**有 `remaining` / `used` / `total`,可以算钱、推"预计可用天数"、画进度条。
**配额型**(OpenCode Go / Codex)接口只给已消耗百分比和重置时间,无法折算金额,所以:

- 卡片主数值显示百分比而非金额
- 行内三格换成限流窗口(OpenCode Go 是 5 小时/本周/本月,Codex 是 5 小时/周)的剩余配额
- **不参与底部消耗合计** —— 百分比与金额不能相加

**积分型**(Trae / WorkBuddy)有剩余量,但每一笔各有到期日,量最大的风险是"过期"而非"花完",
所以核心信息是逐笔到期列表(升序),"还剩多少"只是总数。

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

这些接口都是账务/元数据接口,不是推理接口 —— 查询余额不产生任何 token 计费。
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
      lib.rs              模块清单(17 个)
      providers.rs        7 个内置渠道定义 + 响应解析(万元组测试)
      store.rs            SQLite 快照、消耗推算、365 天归档、变动台账
      secrets.rs          Windows 凭据管理器封装(渠道密钥 + 密钥箱两个服务名)
      vault.rs            密钥箱的校验/去重/搜索/排序
      config.rs           设置项(含申请制额度、胶囊渠道、自定义排序)
      fetch.rs            取数、失败降级、峰谷、计划成本、视图组装
      appauth.rs          本机已登录应用的凭据(Trae tc 容器解密 / Codex OAuth)
      workbuddy.rs        WorkBuddy 桌面端凭据信封(AES-256-GCM)+ refresh 续期
      weblogin.rs         WorkBuddy 网页会话(兜底路径)
      checkin.rs          每日签到的纯逻辑(北京日、退避、幂等、状态机)
      inject.rs           5 个客户端的写入契约(备份/校验/回滚/幂等)
      models.rs           模型页多来源(价目表、可用集合、单价换算、缓存)
      yamledge.rs         极小 YAML 子集解析与行范围编辑(DSH 的 cordis.patch.yml)
      peak.rs             DeepSeek 峰谷时段与法定节假日表
      fx.rs               公开汇率接口解析(OpenCode Go 按天摊费用)
      custom.rs           自定义渠道的字段与校验
  src/                    前端(纯静态,无打包器)
    index.html
    styles.css
    app.js
    logos/                渠道官方图标(来源与版权见该目录 README)
  src-tauri/
    src/
      lib.rs              43 个命令、托盘、轮询调度、开机自启、置顶
      main.rs             入口
    capabilities/         Tauri 2 ACL:窗口尺寸/位置/置顶等权限
    build.rs              链接静态 WebView2 加载器 + 断言两处版本号一致
    tauri.conf.json
    icons/
build/
  install-toolchain.ps1   免管理员工具链安装
  build.ps1               构建入口(check/test/dev/build/bundle,含单 exe 静态加载器准备)
  make-webview2-static.ps1 把 MSVC 静态加载器 + CRT 垫片重打成 GNU 归档
  make-latest-json.js     生成更新清单(校验版本/tag/.sig/安装包四件事)
  gen-icons.js            图标生成(手写 PNG/ICO,无依赖)
  shot-panel.js           给 README 截三形态图(仿真数据注入 + 还原现场)
  verify-form.js          三形态 / 固定尺寸 / 圆角透明的 CDP 断言
  verify-ui-0923.js       2026-09-23 UI 轮的运行时断言
  win-style.ps1           读运行中窗口的 Win32 样式位
  launch-debug.ps1        带调试端口启动,stdout/stderr 落盘
  cdp-probe.js            读运行中实例的 DOM 状态
  probe-frontend.js       假 DOM 里跑 app.js,看 body 最终 class
  scan-secrets.ps1        推送前扫凭据(文件名 + 内容 + minisign 三条)
  verify-push.js          验证推送结果,并确认 token 没落到本地文件
  stop-app.ps1            停掉运行中的实例
  webview2-static/        垫片源码(msvc-shim.c / msvc-alias.S)
.github/workflows/
  ci.yml                  push/PR:core cargo test + 两个 crate 的 check
  release.yml             v* tag:bundle 签名发布 + latest.json
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

### 第五个写入目标:DeepSeek Harness(两份 YAML)

落点是 `~/.dsh/profiles/<profile>/cordis.patch.yml` 里 `llm-pi-ai` 条目的 `config.providers.<route>`
与 `~/.dsh/.credentials.yaml` 的 `refs.<ROUTE>_API_KEY`(一次备份、一起校验)。

- **route 键与显示名**:`4sapi` → `foursapi`、`hapi` → `hapi`、`opencode-go` → 同名;
  模型显示名按来源分前缀 `4S-` / `HP-` / `OG `(如 `OG DeepSeek V4.1 Flash`)。
- **目录内外按模型分派**:pi-ai 已安装目录里有的模型留在目录 route(不写 `api`/`baseURL`,能力照目录抄);
  目录里没有的写到 `<route>-custom`,自带 `api: openai-completions` + `baseURL`。
- **引用名必须合法**:DSH 的凭据引用名要匹配 `^[A-Za-z_][A-Za-z0-9_]*$`,否则整份凭据文件
  连同 pi-ai 全部 route 一起失效 —— 所以 route 键不能以数字开头。
- 它是唯一**允许在运行中写入**的客户端;`agent-default-model` 只读不写。

### 加一个新客户端

1. `inject.rs` 的 `Client` 枚举加一项(短标签 / 名字 / id / 进程名);
2. `sites()` 里加落点检测(配置目录存在才返回);
3. 写一个 `write_xxx_site(&Site, &ModelSpec, ...) -> Result<SiteOutcome, String>`,
   落盘走 `commit()`(备份 → 写 → 读回校验 → 失败回滚),不要自己拼;
4. `preview()` / `state()` 各加一个分支(预告要说清"改哪个文件的哪一条");
5. 前端的 `CLIENT_NAME` 加一行,模型行上的按钮是**按 `CLIENTS` 状态动态渲染**的,不用改。

**规矩**:认不出来的结构就放弃(不写、不改、不猜);密钥只从凭据管理器现读现写,
不落盘、不进日志、不进备份。

## 每日签到(Trae / WorkBuddy)

两个平台的每日积分可以替你去领。**默认关**,首次在渠道卡上点一次「开启签到」才开始发请求。

- **纯逻辑**在 `core/src/checkin.rs`:北京自然日为签到日、抖动的触发时刻、退避序列、幂等(先查后领、领后回查)、
  以及把平台的响应解读成状态机的 `parse_trae` / `parse_workbuddy`。
- **判因只看 `code` / HTTP / 域名,永远不看 message**:9074 的文案是"当前参与用户太多"(真实成因常是设备/IP),
  10001 在国区是"今天已签"、国际版是"活动未开启" —— 唯一按文案分支的地方就是这里,且只在认得出明确字样时才认。
- **请求强制直连**(`direct_client`,`no_proxy`):平台按出口 IP 限流,代理 IP 更容易被拒。
- **凭据**:Trae 用本机登录态(令牌 + `iCubeAuthInfo://icube-dc:` 键名里的设备号);WorkBuddy 用挂件内网页登录的会话。
  都不落盘、不进日志。
- **流水**在 SQLite 的 `checkin_log`:渠道 / 北京日 / 结果码 / 数额 / 次数 / 说明。

## 排查日志

用户报"某渠道取不到数"时,唯一能把现场带回来的东西。

- **落在哪**:`%APPDATA%\com.wacil.tokenscope\diag.log`(与 SQLite 同一个文件夹)。
  设置 → 关于 → 「排查日志」里有路径、大小,以及「复制最近日志 / 打开目录 / 清空」。
- **记什么**:取数链路的每一步 —— 启动环境(盘符、数据目录、相关环境变量)→ 找桌面端 exe
  (命中哪个目录 / 整条候选链)→ 扫到的凭据文件 → 是否加密、keyId 是否匹配 → 解出的令牌
  (长度 + 指纹前 8 位)→ 续期结果 → 每个取数入口的 HTTP 状态与业务 code。
- **不记什么**:任何令牌、Cookie、`atRestSecretKey`、API Key。要区分时用
  「长度 + sha256 前 8 位」;写入前还有一道 `redact()` 兜底(连 `session=` 的值一起掩)。
- **不会写满磁盘**:单文件 1 MB,超过滚成 `diag.log.1`(只留一代),写失败静默并熔断一分钟。
- **重复的失败不会刷屏**:同一状态 5 分钟内只落一次,之后补一行"又重复了 N 次";
  状态一变(错误不同)立刻落新行。
- **关掉**:设环境变量 `TOKENSCOPE_NODIAG=1`(设置页会如实显示"已关闭")。
- 单元测试默认**不写**这个文件(`diaglog` 的测试闸门),只有显式 `set_dir` 的用例才落盘。

### WorkBuddy 装在 D 盘 / 自定义目录

`core/src/workbuddy.rs` 的 `find_app_exe()` 解析顺序:

1. `WORKBUDDY_APP_EXECUTABLE`(可给 exe 或目录,最高优先);
2. **注册表** —— `App Paths\WorkBuddy.exe`(HKCU/HKLM/32 位视图)与卸载项的
   `InstallLocation` / `DisplayIcon`。这是**唯一不靠猜**的来源:
   官方安装器无论装到哪个盘都会写在这儿(本机实测 5.8.3 的 `InstallLocation` 为空,
   只有 `DisplayIcon` 给出路径,所以两种都要读);
3. `LOCALAPPDATA` / `APPDATA` / `PROGRAMFILES` / `PROGRAMFILES(X86)` 下的默认位置;
4. **每个盘符**(A–Z 里真实存在的)× 常见目录名:`WorkBuddy`、`Programs\WorkBuddy`、
   `Program Files\WorkBuddy`、`Apps\WorkBuddy` 等。

数据目录(`workbuddy-desktop*.info`)同样兼容非默认位置:除了三个环境变量,
还认 `WORKBUDDY_AUTH_DIR`,并从找到的 exe 位置反推 `<exe 目录>\CodeBuddyExtension\...`
与 `<盘根>\CodeBuddyExtension\...`。

> 装到别的盘、找不到 exe 时,以前只会说"没装桌面端"(明明装着)。现在失败原因分档
> (找不到 exe / 问不出密钥 / keyId 不匹配 / 凭据过期 / 401),并且**整条探测链进日志**。

## 已知限制

- OpenCode Go 的接口未公开,无稳定性保证,响应结构变化时该渠道会显示取数失败。
  渠道定义里标了 `unstable: true`,便于界面上区分。
- OpenCode Zen 的按量付费 credits 余额没有公开接口,只能在网页控制台查看。
- 开机自启已接通系统 API(winreg 写 HKCU Run 键,启动时按当前 exe 路径重写);
  贴边吸附也已落地(拖到屏幕左/右边缘松手即吸附,鼠标移入展开、移出 1 秒收回)。
- 单 exe 交付:exe 静态链接 WebView2 加载器,不再需要旁边的 `WebView2Loader.dll`
  (见 `app/src-tauri/build.rs` 与 `build/make-webview2-static.ps1`)。
