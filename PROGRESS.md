# TokenScope 开发进展

> 最后更新:2026-10-17
> 状态:**v0.1.36 已发布(补丁:胶囊速览气泡样式重做;v0.1.35 的四包不变)**
> 更早:v0.1.35(小鲸鱼研读四包)、v0.1.34(汇总条瘦身)、v0.1.33(全量体检第二轮:36 条缺陷,P1 六条)、
> v0.1.32(模型「基本信息」逐项可复制)、v0.1.31(写入 Trae 真库验证通过)

## 速览气泡样式重做(2026-10-17,v0.1.36)

**起因**:v0.1.35 发布后用户贴了张真机截图,说"这个样式显示的很奇怪"。

**真机截图读出三个问题**:
1. 蓝色内边框 + 深色内底 + 阴影 —— 在本来就大的卡片里又套了一个框,像弹窗不像气泡;
2. 三行字在 164px 宽的胶囊上被截成「今日合计 ¥0.00 (3…」「最紧张 OpenCod…」;
3. 最小预留高度给到 84px(实测内容只有 41px),于是"信息在上、胶囊在下、中间一大块空"。

**改法**:
- 内容压成两行短语:「今日 ¥0.00」「最紧 4SAPI 92%」(百分比按状态色);放得下用全名、
  放不下退简称(OC),不再出现半截字;
- 去掉内框/内底/阴影与"单击收起"提示行 —— 胶囊本身上半部分就是信息区,
  与下半部分之间只有一条 `border-top` 细线;
- 预留高度改成**贴着内容**(实测 + 12px,下限 44px):窗口从 160px 降到 120px。

**验证**:真机(125% 缩放)复测 67→120 向上长高、底边不动、胶囊 45px 不被压、4 秒自动收回;
`node --check` 与假 DOM 探针通过。发版:tag `v0.1.36`,三个资源齐,更新端点 200 且与本地产出
逐字一致(sha256 `ee41eea1d3cb4c9dff04c8cfb97c986db758a6b23100f7e53b51a181d79f5336`)。

## 小鲸鱼插件研读 → 四包落地(2026-10-16,v0.1.35)

**起点**:研读了 DeepSeek Harness 上的社区挂件 `dsh-whale-widget`(本机 desktop profile 实装版,
v0.4.2;代码注释回指到 issue #232),写了一份设计文档并拿到 10 项裁决
(`design-whale-inspiration-2026-10-16.html`)。裁决结果:**第 4 项「今日预算」作废**;
第 10 项选了「一版全做」→ 全部进 v0.1.35,并用两条措施对冲(提交按包分组、每件都带开关)。

**信任包(核心:让每个数字有出处)**

- `peak.rs`(新模块):DeepSeek 峰谷时段。工作日 9–12 / 14–18 高峰、周末与法定节假日全天谷价;
  只需**北京时间**(固定 +8,不吃系统时区);只报"贵不贵 / 还有多久变",**不落任何单价数字**
  (规则两个月被官方改过三次)。节假日清单进代码,`table_stale()` 从**11 月**起如实提示"该补次年表"。
  11 项测试。
- `store.rs` 快照层四件:
  ① **密钥指纹分段**(迁移到 user_version 3,加 `key_fp` 列 + 索引):换 key 起新账,
  消耗只在同一本账里推算;首次拿到稳定指纹时**认领**无指纹的历史行,让没换过 key 的用户账目连续;
  ② **缺口覆盖**(`OBSERVE_GAP_SEC = 10 分钟`):窗口"从几点起才有数据"、逐日覆盖;
  ③ **变动台账**:同方向连续轮询聚合成一条(20 分钟窗),方向一变立刻另起,带空档秒数;
  ④ **90 天明细 → 365 天日级归档**:`prune` 删之前先压(可重入、分界日累加),`trend_series` 把
  归档与明细拼成一条 24 桶曲线并标出缺口桶。9 项测试。
- `fetch.rs`:`ChannelView` 新增 `peak / gap_day_from / gap_week_from / gap_month_from / gap_days /
  accounts`;消耗窗口、申请检测、近 7 天日均**全部改走指纹分段**;`key_fp()` 用 sha256 前 12 字节
  (不落明文);`probe_report()` 供体检用,**不写快照**(手动测试不该填上观测缺口)。
- 体检:`probe_channel`(逐渠道真打一次,报域名/耗时/解析值/错误)+ `run_diagnostic`
  (全部渠道 + 本机四项:快照库可写 / 开机自启注册 / 系统代理 / 数据目录),产出可复制纯文本,
  **报告里没有密钥与完整响应体**。

**提醒包**

- 系统通知(新依赖 `tauri-plugin-notification`,走代理从 crates.io 拉取):跌破红线时响一次、
  一天最多一次、按渠道各自记账;**第一轮只记基线**(不让刚开机时的一堆旧账刷屏);
  通知本体不回跳(插件不支持动作),改为窗口隐藏时闪一下任务栏。
  判定 `crit_now()` 与前端 `toneOf()` 逐条同口径,并补上"余额真 0 也进红线"。
- DeepSeek 行内峰谷:副标题报状态、口径位报倒计时(1 秒一跳,本地时钟推算,不依赖轮询频率)。

**拓展包(自定义渠道)**

- `custom.rs`(新模块):`path` 解释器(`a.b` / `data[0].quota`)+ 通用取数(GET / POST + body
  `{key}` 占位 + `scale` 换算 + 探活模式)。**模板只提供默认值**,留空沿用,用户可自救。
- 渠道页新增表单:**测试通过才允许保存**(任改一个字段立刻作废,已实测);
  `custom_add` 先写密钥再落配置(配置失败回删密钥,不留孤儿凭据);`custom_remove` 只清定义与密钥,
  历史快照保留。它们和内置渠道共用消耗窗口 / 缺口 / 分段 / 密钥纪律,取数只发往用户自填地址。

**气质包**

- 胶囊按压微缩(transform-origin 定在下缘,底边不动)+ **单击速览气泡**:窗口临时向上长高一条,
  气泡画在新增空间里,4 秒自收;单击与拖动按"位移 <4px 且 <400ms"分离,▲ 仍是唯一的展开入口。
- 音效两声(签到领到 / 跌破红线),**默认关**,WebAudio 现场合成 —— 不新增任何音频资源文件。
- 前端:行详情加缺口行、台账、"斜纹柱 ≠ 没花钱"说明、近 3 月 / 近 12 月区间、历史账户提示;
  显示页加"提醒与声音"两个开关(通知默认开、音效默认关);关于页加"运行体检 + 复制报告";
  渠道卡加"测试连接"与自定义渠道的"删除"。

**验证(本机)**

- core `cargo test`:**181 passed / 0 failed**(净增 28:峰谷 11 + 快照/曲线 9 + 视图/指纹 3 +
  自定义渠道 3 + 台账/归档等);壳 `cargo check --all-targets` **0 错误 0 警告**(touch 逼真重查过)。
- 前端:全部 `.js` 过 `node --check`;`build/probe-frontend.js`(假 DOM)通过。
- **桩 E2E(真 DOM + 桩 `__TAURI__`)**:3 行渠道(含自定义渠道)渲染正常;DeepSeek 副标题"谷价中"、
  口径位倒计时元素存在;**展开行**里出现"今日数据自 01:48 起 / 最近变动 2 条(推算消费(5 次轮询)、
  入账(充值/额度)、含 3 小时未观测)/ 历史账户提示 / 区间含近 3 月与近 12 月 / 斜纹柱 ×2";
  体检按钮跑出报告且复制按钮出现;自定义渠道表单"测试 → 保存可点 → 改一个字段又变灰"整链通过;
  全页 0 异常(只有一个桩页缺 logo 的 404)。产物在 `%TEMP%\ts-e2e.html`,不进仓库。

**未在真机验(不阻塞发版,列在这里):**

1. 系统通知**是否真的弹出来** —— 只在真实宿主里看得到(桩里发不出);
2. 胶囊速览的窗口几何(向上长高 + 收回)只在真窗口有几何语义;
3. 音效两声的音色/音量(桩里没有音频设备);
4. 自定义渠道接**真实**中转站接口的一次完整取数(桩里只有假响应);
5. 点通知的"回跳"没有实现 —— 插件不支持动作,已改为闪任务栏。

**遗留(记录在案,未改):**

- `providers.rs` 里 Trae `credits_amount` 与 WorkBuddy `RemainPrecise` 两处,字段缺失时按 0 处理
  (界面上就是"用了 0"或"剩 0")。本轮**未取证**上游是否真会出现"有 size 没 remain"的组合,
  没有样本就不盲改(改方向错了比 0 更糟);先记在这里。
- 点通知不回跳(见上),要做的话得自己起通知窗口,不值当。

**构建与发版(2026-10-16)**

- 版本号已升 **0.1.35**(`app/src-tauri/Cargo.toml` + `tauri.conf.json`)。
- `build/build.ps1 bundle` 跑通:产物 `Grandettoken_0.1.35_x64-setup.exe`(4.95 MB)+ `.sig`,
  两条防"假绿"的断言都过 —— `ok Grandettoken.exe rebuilt (SHA-256 changed)`(证明本轮真的重链过)
  与 `ok import table clean (no WebView2Loader.dll)`。新依赖 `tauri-plugin-notification` 经本地代理
  从 crates.io 拉取并已进缓存(此后构建不再需要网络)。
- **已发版**(2026-10-17):tag `v0.1.35` → `3e1688a`,release
  <https://github.com/wacilimonster-source/Grandettoken/releases/tag/v0.1.35>,
  三个资源都在 —— `Grandettoken_0.1.35_x64-setup.exe`(4,948,446 字节,
  sha256 `d69248904606b9f5a70b5a1dd982daebc9c79cf3df830f14b42fbc01c4d3d8b7`)、`.sig`、`latest.json`。
  更新端点 `releases/latest/download/latest.json` 实测 200,内容与本地产出逐字一致(含签名),
  安装包 URL 302 → 200 可下载。
- 提交分三次(便于回退与二分):core 数据层 / 壳+前端 / 文档;版本号提升在第二次里。
- **真机走查抓到并已修**的两个 bug(修复后重出安装包再发,故 02:46 那个才是发布产物):
  ① 速览窗口用 `PhysicalSize` 传逻辑尺寸 —— 125% 缩放下窗口只长 61px、胶囊被压成 14px;
  ② 气泡挂在 `.pill-body`(position:relative + overflow:hidden)里,被定位到胶囊内部并裁掉半截。

## WorkBuddy 登录:真正要抄的是会话 Cookie,不是令牌(2026-10-08,v0.1.19)

**症状**(用户扫码登录成功后的反馈):二维码扫过、账号也登进去了,但登录窗停在页面上
"没反应",挂件一直没有数;手动关掉窗口则提示「已取消登录」。

**根因**:v0.1.17 抓令牌的**前提本身是错的**。设计阶段是从 usercenter 的 JS 包里读到
`Authorization: Bearer` 逻辑的,但真机上把控制台的请求全看了一遍(CDP 抓完整请求头):
`/console/*`、`/billing/meter/*` 全都**没有 Authorization 头**,`sessionStorage` 里也
**没有** `growth-center-token`;认证全在 httpOnly 的 `session` / `session_2` 两个 Cookie 上。
于是三条抓取路(fetch/XHR 头、URL 参数、sessionStorage)一条都不可能命中 —— 挂件永远等不到
令牌,用户看到的就是"登录成功了却没反应"。

**还有第二道门**:网关(openresty)按 **User-Agent** 分流。同一批 Cookie,
浏览器 UA → 200,挂件客户端 UA(`TokenScope/0.1`)→ 401。用 curl 逐项对照过:
`browser UA + 两个 Cookie` 是 200 且 `TotalDosage` 正确,`session` 与 `session_2`
**缺任何一个都 401**(只带其中一个也 401),加不加 `tgw_l7_route`/设备号都不影响。

**改成什么**(v0.1.19):
- 登录窗**不再注入任何脚本、不再拦导航**:on_navigation 只当信号用(记下"页面已经落到
  个人中心/控制台"),一律放行。页面是第三方的,挂件只负责看它走到哪儿。
- 观察者每秒问一次:会话可用吗?**判据是"用这份会话真打一次接口能不能通"**,不是
  "cookie 罐里有没有东西"(没登录的访客也可能有 session Cookie)。通了就关窗报「已登录」,
  验不过且确实刚登录完才报原因(改用访问密钥那条备选路)。
- **什么都不落盘**:会话就住在 WebView2 的 profile 里(登录窗与主窗口共用一份),
  取数时现读(`WebviewWindow::cookies_for_url`,httpOnly 也读得到)。两个理由:
  Windows 凭据管理器的 blob 上限实测只有约 1280 字符(2000 字符就报
  `longer than platform limit of 2560`),而这两个 Cookie 加起来近 6 KB;**而且**平台一旦
  轮换 Cookie,存下来的副本会静默失效 —— 现读天然跟着新的走。
- 「清除登录」现在会一并删掉 profile 里的会话 Cookie(否则按钮点了等于没点),并作废
  取数的 5 秒结果复用;`set_key` 同样作废(换完密钥立刻刷新不会再端上旧结果)。

**顺带修掉一个会崩掉整个挂件的地雷**:`cookies_for_url` 这类调用内部是**阻塞派发**
(worker 线程发消息给主线程,再 `rx.recv().unwrap()` 等回信)。目标窗口正在销毁时回信
永远不来,unwrap 直接 panic —— 而 release 是 `panic = "abort"`,一次派发失败 = 挂件原地消失。
实测踩到过(刚关掉登录窗就去读它的 Cookie,日志里留下 `RecvError`)。现在所有要碰 webview
的调用都改成"投递到主线程 + 自己带超时地等",拿不到就当没读到、退回"未登录",绝不 panic;
观察者也因此改成读主窗口(它从启动活到退出),并且不去问那个可能正在销毁的登录窗
(用户关窗靠 Destroyed 事件置位,落地判定靠 on_navigation 置位)。

**验证**(debug 构建真机跑,CDP 全程观察):
- 已登录状态再点「登录」:窗口 1.2 秒内自己关掉,toast「已登录 WorkBuddy · 读数已刷新」,
  渠道显示 `浏览器网页会话` + 真实读数(剩余 5462.54 credits / 已用 2637.46);
- 清除登录 → profile 里两个会话 Cookie 消失、渠道变「未登录 WorkBuddy 账号」;
- 此时点「登录」:窗口停在登录页(确认是未登录态),用户按 X 关掉 → toast「已取消登录」、
  按钮恢复可点;**日志零 panic**;
- 把会话 Cookie 原样写回 profile → 读数立刻回来(证明"现读"这条路不依赖任何本地副本);
- core 55 项单测全绿(weblogin 的 Cookie 拼装/前缀校验/落地路径判定共 4 组),`cargo check` 零告警。

**尚需真机**:会话有效期(实测两个 Cookie 都是 7 天后到期,所以大约每周要重登一次);
以及真实扫码登录那条路(v0.1.18 的死锁修掉之后,第一次登录就是用户自己扫码完成的)。

## WorkBuddy 登录窗打开后全白、整个挂件卡死(2026-10-08,v0.1.18)

**症状**(用户真机反馈 —— 正是 v0.1.17 设计稿里留的那条"还差你一次真机登录"):点
「登录 WorkBuddy 账号」后窗口壳出来了,里面**一片空白**,什么也没加载;此后连"刷新"之类的
按钮也全都没反应,挂件像死了一样。

**根因**:`workbuddy_login` 是**同步**命令,而 Tauri 的同步命令在主线程上就地执行 ——
这条命令又恰恰是在 WebView2 的 `WebMessageReceived` 回调里被调起的(它是前端 invoke 进来的)。
在 COM 回调里再同步创建第二个 webview,`CreateCoreWebView2Controller` 的完成回调就再也
送不回主线程:wry 卡在 `wait_with_pump` 死等,主线程从此不再处理任何消息
(**所以无关命令 `get_channels` 也一起超时**,界面看着像卡死),而新 webview 的页面
永远停在 `about:blank`。

**取证方式**(`--remote-debugging-port` + CDP,逐层排除):
- 窗口树:登录窗有 wry 的容器 `WRY_WEBVIEW`(560×700),但**没有** WebView2 的
  `Chrome_WidgetWin_1` / `Chrome_RenderWidgetHostHWND`(主窗口两样都有)→ 控制器没建完;
- `/json/list`:invoke 之后多出一个 `about:blank` 目标,之后 40 秒不动,`workbuddy.cn` 目标
  始终不出现;对它发起 CDP `Page.navigate` 也一直不返回;
- 主窗口 JS 还活着(`1+1` 能算、DOM 完好),但任何 invoke 都超时 → 排除"前端卡死",
  定位到 Rust 侧主线程。
- 排除项:CSP(只作用于 tauri 自定义协议,不碰外部页面)、站点可达性(直接 curl 登录页
  200 + CDN 带 `Access-Control-Allow-Origin: *`)、注入脚本(根本没跑到)。

**修法**:`workbuddy_login` 改成 `async fn`。async 命令跑在 async runtime 的线程上,
窗口创建因而变成一条投进事件循环的消息,执行的上下文与"启动时按 tauri.conf.json 建主窗口"
完全一样 —— 那条路一直是通的。**这条 async 不能改回同步**,`lib.rs` 里留了注释说明原因。

**验证**(debug 构建真机跑,CDP 全程观察):
- 命令立即返回;登录页真正加载出来(标题 `WorkBuddy - AI Agent 办公新范式`,微信登录 /
  手机号 / 邮箱 / SSO 全在,截图核对);
- 注入脚本在真实页面里生效(`__tsWbHook=1`,fetch 与 XHR 包装都在);
- 抓取链路两条**实测都通**:①sessionStorage 令牌;②请求头 `Authorization: Bearer …`。
  都走到哨兵地址 → `on_navigation` 拦下 → 写进凭据管理器(`key_hint` 回显掩码尾号)→
  窗口自动关闭 → 渠道状态变「已登录」(hasKey=true)。
- 取消链路:模拟用户按 X 关窗 → 主窗口收到 `{"ok":false,"cancelled":true}`,
  按钮从「登录中(禁用)」恢复可点;未抓到令牌时凭据槽位保持空;
- 点按钮的真实路径(设置 → 「登录 WorkBuddy 账号」→ 关窗)整条跑通,页面 console 零报错;
- 测试注入的假令牌已 `delete_key` 删除,槽位确认回到空。

**尚需真机**:用真实登录(扫码/手机号)拿到的令牌打 `get-user-resource` 是否 200 ——
这条只有你自己的账号能验。

## WorkBuddy 凭据改道:挂件内网页登录(2026-10-02,v0.1.17)

方案与取证见 `design-workbuddy-web-login.html`。用户裁决:**1A 2A 3B 4A 5A 6A 7B**。

**为什么必须改**:`C:\Users\wacil\AppData\Local\CodeBuddyExtension\Data\Public\auth\workbuddy-desktop.info`
里 `auth.accessToken` 从明文字符串变成 `{"$wbEncrypted":1,"envelope":…}`(AES-256-GCM,
`{suite,keyId,nonce,authTag,ciphertext}`),`keyId = sha256(32B 静态钥) 前 16 位` = `9127dea1b44020a7`;
静态钥编译期内置在客户端安装包(`atRestSecretKey`,`at-rest-crypto` 包),用户目录里既无明文也无
DPAPI 密文(全盘扫过 `.workbuddy` + 两个 WorkBuddy 数据目录,12 万个文件)→ 本机不可解。
生效时刻:凭据文件与 `~/.workbuddy/keyblob` 同为 2026-10-01 21:06,库里面板最后一笔成功快照是 21:01。
同目录 6 月备份那份仍是明文,实测打两个域名都 **401**,已是废票。
**与"装了国内版+国际版两个客户端"无关**:国际版写独立文件 `workbuddy-desktop-ai.info`
(`domain=www.workbuddy.ai`),原本就被域名白名单排除,两版互不覆盖。

**改成什么**(`AuthKind::WebSession`,只给 WorkBuddy 用,裁决 5A):
- 挂件开独立窗口(560×780、置顶)加载 `www.workbuddy.cn` 登录页;页面自己的令牌来源已核实:
  跳转参数 `?token=` → `sessionStorage["growth-center-token"]` → 请求拦截器加
  `Authorization: Bearer …`(见 `download.codebuddy.cn/web/usercenter/…/config-wXDbhNun.js`)。
- 注入脚本(与页面脚本同时安装)钩 `fetch` / `XMLHttpRequest.setRequestHeader`,并轮询 URL 参数与
  sessionStorage,任一路径先抓到 Bearer 就跳哨兵地址 `https://tokenscope-capture.invalid/c?t=…`;
  `on_navigation` 拦下解析(**`.invalid` 是保留顶级域,导航也被取消,令牌不出网**)。
- 解析与合法性判定在 `app/core/src/weblogin.rs`(主机名整串相等、拒绝变形域名、
  长度/控制字符/空格校验、剥 `Bearer ` 前缀)—— src-tauri 不挂测试,所以这段放 core 才守得住。
- 令牌存 Windows 凭据管理器条目 `workbuddy`(与手动粘贴同一个槽位,裁决 4A);取数失败 401/403 的
  文案改成「网页登录已过期 · 在挂件里重新登录一次即可」,不再说"打开一次 WorkBuddy"(裁决 2A)。
- **不再读本机 `.info`**(裁决 3B):`appauth` 里的 WorkBuddy 扫描路径、`.info` 备份排序、
  CN 域名白名单与两个单测一并删除;Trae / Codex 的本机登录态复用不受影响。

**验证**:core `cargo check` 无告警、**55 项单测全绿**(新增 weblogin 4 项 + 凭据来源文案 2 项);
src-tauri `cargo check` 无告警;注入脚本用 lib.rs 原文抽出后在真实浏览器里跑四种场景
(fetch 头 / XHR 头 / sessionStorage / URL 参数)全部正确跳到哨兵地址并带上令牌,
「主机不在名单内」与「页面根本没有令牌」两种情况确认不跳;前端用 `__TAURI__` 桩在浏览器里
验过未登录 / 已登录 / 已过期三种卡片状态与登录按钮的 invoke、事件回流、toast 文案,零 console 报错。
**尚需真机**:用真实登录拿到的令牌打 `get-user-resource` 是否 200、令牌实际能用多少天。

## UI 优化第二轮(2026-09-23,设计稿 `design-ui-motion-2026-09-23.html`)

先出设计稿(用户裁决:全做、只做深色),再按「设计 − 代码现状」差集实施 ——
实测发现 v0.1.13 已含动效层/高对比/热键/位置持久化,以下为本轮**实际**改动,
全部通过运行时验证(51 单元测试 + `verify-form.js` 三形态 + 新增
`build/verify-ui-0923.js` 17 项断言,全 PASS;面板截图 `build/shot-panel-0923.png`)。

**视觉**
- U2 状态点分层:未配置 = 空心灰环 `.dot.off`,取数失败/失联 = 斜杠灰点 `.dot.st`
  (原来一个灰点两种语义)。列表行/胶囊/渠道卡/底栏共用新 `dotCls()`。
- U4 卡片外阴影:窗口四周五 10px 真透明边距,卡片画在内部带
  `0 2px 8px -2px rgba(0,0,0,.6)`(**阴影外延必须 ≤10px,否则被窗口边界裁出硬边**,
  设计稿的 `0 18px 48px` 外延 50px+ 不可用)。尺寸标定全部 +20:面板窗口
  380×560 → **400×580**,紧凑条/胶囊内容高 46 → 窗口 66;吸附展开时窗口向贴边侧
  多探出一个边距让卡片贴齐屏幕边;`tauri.conf.json` 初始尺寸、`verify-form.js`
  期望值同步更新。圆角真透明不受影响(cornerAlpha=0 PASS)。
- U6 紧凑条图标:16px 档官方位图回退字母方块(`iconHtml` 内嵌 `.ltr` 副本 +
  `--cc` 渠道色,CSS 切换),28px 档继续用官方标。
- U7 键盘可达:全部可点元素统一 `:focus-visible` 焦点环;6 个设置开关 +
  额度申请开关从 `<div>` 改为 `<button role="switch" aria-checked>`。

**动效与反馈**
- M2 刷新真实进度:取数期间「刷新」持续脉冲禁用,完成才停;结束 toast
  「已更新 · 刚刚 / N 个渠道取数失败」(新增 `#toast` 轻提示组件,2.6s 自动消失)。
- M5 胶囊轮播:5s → **8s**,加底部 3px 指示点(当前页高亮);悬停不暂停但节奏 ×3。
- M6 贴边:展开内容从贴边侧滑入+淡入 180ms(不再瞬变);拖拽进 40px 阈值显示
  2px 吸附预览线(可发现性);I6 展开态右下角 ✕ 显式收回(Esc 同效)。
- M7 跌破红线一次性红晕:`data-dtone` 记录上轮状态,非红 → 红的行当播一次
  1.2s `breatheOnce`(无限循环呼吸仍由 alertPulse 开关另管,默认关)。

**交互**
- I1 排序前置:标题栏「↓ 剩余」点击循环 剩余/余额/消耗/自定义,与管理页下拉双向同步。
- I2 `+N` 可交互:悬停列被藏渠道名;点击展开面板、滚动定位并高亮 1s。
- I3 原生对话框清零:删除密钥改行内确认条(`.cfrm`),全部 `alert()` 改 toast。
- I5 密钥保存成功:卡片绿色左边框亮 1.4s + 立即回显尾号。
- 管理页 ↔ 展示页 120ms 淡入过渡。

**后端(U8 汇总环比)**
- `ChannelView` 新增 `dayPrev/weekPrev/monthPrev`:上一周期**同时段**消耗
  (今日 vs 昨日同时段等 —— 拿整段上周期比会系统性偏大);新增
  `local_prev_month_start()` 与单测(51 个全过)。前端在"本期与上期都有数"的
  渠道集合上算环比,涨红降绿,基线不足显示「——」。

**踩坑**
- 并行多个 Edit 调用改同一文件会互相覆盖(后写的赢):fetch.rs 的 build_view
  字段、index.html 的 `#btnSort` 各丢了一次,症状是"编辑明明成功,文件里没有"。
  同一文件的多个编辑**必须串行**。
- 会话内 bash 后台 `&` 启动的 GUI 进程会随命令结束被杀;验证用实例要用
  `run_in_background` 托管(或 Start-Process,但本机 PowerShell 工具只执行不回显,
  构建走 bash 直跑 cargo + Windows 路径环境变量)。

**未做(用户裁决)**:浅色主题(只做深色);B2 吸附轻量预览行、B5 点击穿透、
B10 通知接线维持原优先级挂起。

## Trae / WorkBuddy 积分(v0.1.4,2026-09-17)

**开发在分支 `feat/trae-workbuddy-points`(commit `52c22da`)上完成,用户确认效果后
fast-forward 合并进 main 并随 v0.1.4 发版。**分支保留在本地与 origin,可随时回看。

### 需求与结论

用户要的是"近期过期的积分要精确到哪天过期多少"。**第 0 步探测结论:两家都能逐笔**

| 平台 | 凭据来源 | 接口 | 逐笔到期 |
|---|---|---|---|
| Trae(SOLO CN) | `%APPDATA%\TRAE SOLO CN\User\globalStorage\storage.json` 的 `iCubeAuthInfo://icube.cloudide`(`tc` 容器,需解密) | `POST api.trae.cn/trae/api/v2/pay/user_current_entitlement_list` | ✅ 每个额度包一条:数量 `quota.credits_limit`、已用 `usage.credits_amount`、到期 `expire_time` |
| WorkBuddy | **现状(0.1.17 起)**:挂件内网页登录拿到的 Bearer 令牌,存 Windows 凭据管理器条目 `workbuddy`。下表是 **0.1.4~0.1.16 的历史做法**,客户端 5.6.2 起该文件里的 accessToken 已改加密存储,不再可读:`%LOCALAPPDATA%\CodeBuddyExtension\Data\Public\auth\*.info`(明文 JSON) | `POST www.workbuddy.cn/billing/meter/get-user-resource` | ✅ 每个包一条:`CycleCapacitySizePrecise` / `RemainPrecise`,到期取 `DeductionEndTime ‖ ExpiredTime ‖ CycleEndTime` |

### 实机验收数据(2026-09-17,与官方口径交叉核对一致)

- **Trae**:剩余 3,855.28 分(4050 发放 − 194.72 已用);22 笔有剩余,最近 9-18 到期 5.28 分
- **WorkBuddy**:剩余 6,075.52 分 = 本周期 8300 − 已用 2224.48
  (与 `/get-user-resource-summary` 的 5575.52 + 500 完全对齐);30 笔包,27 笔有剩余

### 实现要点

- 新模块 `app/core/src/appauth.rs`:**只读复用本机登录态,绝不写回、绝不刷新**。
  刷新会轮换 refreshToken、把客户端挤下线,所以 token 失效只提示"打开一次 X 即可"。
- Trae 的 `tc` 解密:base64 → [6 字节头][32 字节随机密钥][密文],`SHA512(rb)` →
  `SHA512(h + 固定盐)` 取 key/iv,AES-128-CBC;明文前 64 字节是校验和。
  单测里有一份 **OpenSSL 生成的交叉验证向量**(不是自证)。
- 凭据候选排序:WorkBuddy 的 `auth.domain` 必须是 `www.codebuddy.cn`/`www.workbuddy.cn`,
  且**备份文件(文件名带 ISO 时间戳)排后** —— 一份 6 月备份的 expiresAt 更晚但 token 已作废
  (实测 401)。取数遇到 401/403 才换下一份候选(`another_credential_may_help`)。
- 新数据模型 `Kind::Points` + `expiring: [{at, amount, label}]` + `expiringSoon`(近 30 天合计)。
- 前端:**主数值不带 ¥**,副标题「近 30 天将过期 N 分」,行内列最近三笔到期,
  展开是逐笔到期全量表(7 天内红 / 30 天内黄);密钥页对这两个渠道显示"已读取本机登录"、不给输入框。

### 顺手修掉的坑

- `fitPill` 用 `getComputedStyle(el).font` **简写**量文字宽度,Chromium 里该简写常是空串 →
  按 13px/400 量,窗口比文字窄几像素,胶囊里出现 `WB 6,075…` 半截数字。改成逐项拷字体长手属性。
- 积分型不参与"剩余百分比/余额"排序(`score = 4000`,排在可比渠道之后、取数失败之前);
  状态色按**近 7 天**到期占剩余的比例(≥30% 红 / ≥10% 黄)—— 用 30 天窗口会让 Trae 常红
  (它的签到积分本来就 31 天有效)。
- WorkBuddy 查询时间窗从"近 7 天起"改回"当前时刻起"(与官方前端一致):
  窗口开宽会多捞回 4 个旧包,"本周期发放/已使用"比官方口径多 400 积分。

### 已知边界 / 未做

- ~~**未做** WorkBuddy 的 DPAPI 兜底(`state.vscdb` 的 Safe Storage):明文 `.info` 在本机一直存在,
  再引 `windows-sys` + `aes-gcm` 两条依赖、且无法端到端验证,收益不成立;真读不到就提示"打开一次 WorkBuddy"。~~
  **2026-10-02 作废**:客户端 5.6.2 起明文 `.info` 不再存在,那句提示也把人往错方向带 ——
  WorkBuddy 已改走挂件内网页登录(见本文顶部 v0.1.17 一节),`appauth` 里的 WorkBuddy 扫描路径已删除。
- 两个渠道都标 `unstable: true`(未公开接口),失败静默降级沿用上次快照。

### 发版之后的两处改动(2026-09-17,未发版)

1. **一次失败的检查不再清掉"有新版本"提示**(`app/src/app.js` 的 `checkUpdate`)。
   原来只要 `available:false` 就把 `UPD.info` / 标题旁标签 / 卡片一起清掉 —— 检查失败
   (没网 / 被限流)也走这条分支,于是"用户刚看到新版本、还没点更新,一次自动检查失败
   提示就消失了"(v0.1.4 验证时实测到一次)。现在**只有真的问到"没有新版本"**(或被
   「跳过此版本」)才清;检查失败只给反馈,不动已有状态。
   **验证**:临时把 `plugins.updater.endpoints` 指向 `https://127.0.0.1:9/latest.json`
   (拒绝连接),先人造"已知有新版本"的状态再走真实 `check_update` ——
   命令返回 `error: "error sending request for url (...)"`,而 `UPD.info` / 标签 / 卡片
   全部保留(PASS);端点还原后确认 `error:null` 的那次会正常清空。
   注:页面里没法用 `mockIPC` 打桩 —— `window.__TAURI_INTERNALS__` 是
   **不可写不可配置**属性,只能这样真造失败。
2. **Trae / WorkBuddy 换成软件真实图标**(与 OC / DeepSeek 同一套做法):
   - `trae.svg` ← 本机 TRAE SOLO CN 客户端自带素材 `resources/app/out/media/trae-logo.svg`
     (深底 `#1A1B1D` + 荧光绿几何标,和任务栏里看到的是同一个);
   - `workbuddy.png` ← 本机 WorkBuddy 客户端 `Assets/Square44x44Logo.targetsize-256.png`(256×256)。
   来源与版权说明写进了 `app/src/logos/README.md` 的来源表。

---

## 一句话现状

Rust 侧完成并通过 14 个单元测试,release 编译通过(5.24 MB exe)。
三种形态的窗口尺寸固定不可拖拽、视觉按原型稿对齐、密钥与设置合并进
「设置与管理」页 —— 三项均已落地并做了运行时验证(截图 + 断言,见
「界面改版」章节)。仓库已迁到纯英文路径 `G:\game\nw\Grandettoken`,
构建不再镜像到用户目录。
**遗留一个交付层问题**:GNU 工具链下 exe 加载期依赖 `WebView2Loader.dll`,
裸单文件启动报系统错误 —— 见「进行中」章节,三个可选方案已论证。

## 产物约束(用户确认)

> 更正(2026-09-20):交付形态早已改为 **NSIS 安装包**(per-user,`%LOCALAPPDATA%\Grandettoken`),
> 绿色单 exe 已下线 —— 本段及下方「进行中」章节是当时的决策记录,保留作历史;
> 现行发版流程见 RELEASE.md。

**单 exe、免安装、绿色运行。**(历史决策,已被「追加调整 12」取代) 现状与该约束的差距:功能与体积没问题
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

### 追加调整 2(2026-09-16,按用户反馈:胶囊的入口与内容)

1. **胶囊里加可见的切换按钮**:之前胶囊只有"点哪都能展开"这个隐藏手势,
   界面上看不到任何切换入口。现在胶囊右侧常驻 `▲`(展开为面板),
   与紧凑条的 ▲ 语义一致;整块点击展开的行为保留。
2. **胶囊显示哪些渠道可配置**:管理页新增「胶囊显示」一组多选芯片
   (每个渠道一个),规则:
   - 一个都不选 = 自动,显示**最紧张的一个**(原行为,默认值);
   - 选 1 个 = 固定显示该渠道;
   - 选多个 = 每 5 秒轮播一次,鼠标悬停**不**暂停。
   悬停暂停本来做了,实测发现挂件上指针常停在胶囊附近,一停就像轮播坏了
   (作者自己都被骗过一次),故去掉 —— 点击只是展开面板,内容变化无副作用。
   提示气泡里会标"自动:最紧张的一个"或"2/3 轮播",便于确认当前处于哪种模式。
3. **配置字段**:`Config.pill_channels: Vec<String>`(JSON `pillChannels`)。
   `Config` 上有容器级 `serde(default)`,老库缺这个字段照样能读;新增两个
   单元测试锁这条(共 **16 个测试**):`old_config_without_pill_channels_still_loads`、
   `pill_channels_roundtrip`。

验证(CDP 注入 + 真实数据两轮,全部 PASS):自动模式显示最紧渠道并标注;
单选固定;多选在 12 秒窗口内覆盖全部已选渠道(4S/OC 交替,每 5 秒一个);
选择器芯片数=渠道数、点击加入/再点移出会写回配置;`verify-form.js` 全项回归通过。

> 备注:验证期间用户在应用里配好了真实密钥,胶囊与面板已在跑真实数据
> (取数 → 渲染 → 状态色的完整链路),不再只是仿真数据。

### 追加调整 3(2026-09-16,方案 A:申请制额度;方案经用户确认)

**背景**:4SAPI 是"申请制"—— 定期申请一次把余额补到 ¥200,之后超过 2 周
可以再申请(不是固定日历周期)。而原来的剩余百分比用的是接口给的
**累计发放**(`total_granted`,只增不减):实测 ¥18.99 / ¥399.83 = "剩 5%",
这个数只会越来越低,还被染成告警色 —— 分母本身就选错了。

**用户确认的四个决策**:分母换成单次额度 ¥200;申请是"补到 200"(可用永远
≤ 200,分母是常数);上次申请时间自动检测;只在界面上显示状态,不做系统通知。

口径修正(申请制渠道):

| 位置 | 之前 | 现在 |
|---|---|---|
| 副标题 | 额度 ¥399.83 | 本轮额度 ¥200 · 再等 6 天可申请 |
| 大数字下方 | 剩 5%(分母=累计发放) | 剩 9%(分母=200) |
| 进度条 | 4.7% 宽 | 9.5% 宽 |
| 展开详情 | —— | 本轮额度 / 上次申请(含来源)/ 可再申请 / 日均消耗 / 预计可用 / 累计发放 |

**自动检测申请时间**:每次轮询写快照前比对可用余额,跳升 ≥ ¥20 记为一次申请
(`store.last_jump_since`,取最近一次,回溯 90 天与快照保留期对齐)。低于阈值
的抖动不算 —— 接口金额有四舍五入,不设阈值会被误判。**手动修正优先**,除非
检测到比"手动写入时刻"更晚的跳升(说明又申请了一次,自动值接管);这样
"程序没运行期间申请的"也能被手动校准而不被自动值顶掉。

**撑不到下次申请预警**:近 7 天日均消耗(数据覆盖不足 1 天不估算,样本太短
斜率会离谱)→ 预计可用天数 < 距可申请天数时,行内黄字提示。

**配置**:管理页新增「额度申请」一节(只列金额型渠道),每渠道一个开关,开启后
展开 单次额度 / 最短间隔 / 上次申请日期,以及「记一次申请=今天」「清除手动值」。
`Config.claim_channels: HashMap<String, ClaimConfig>`,容器级 `serde(default)`
保证老库可读;4SAPI **默认开启**,其他渠道默认关。

**测试**:26 个单元测试(新增 10 个)—— 倒计时与到点、手动/自动优先级、
撑不到预警、未记录时的降级、快照跳升检测(含抖动不误判)、窗口消耗不依赖
前置基线、老配置兼容;另有跨模块组装测试(快照跳升 → 自动识别为一次申请)。
运行时断言(CDP):行内文案与分母、管理页只列金额型渠道、填日期后倒计时立即
生效、清除后回到自动。

### 追加调整 4(2026-09-17,官方图标;方案经用户确认选 A)

OpenCode Go 与 DeepSeek 换成**官方标**,4SAPI / Hapi 仍是字母方块。

| 文件 | 渠道 | 来源 | 体积 |
|---|---|---|---|
| `app/src/logos/opencode.svg` | OpenCode Go | https://opencode.ai/favicon.svg | 612 B |
| `app/src/logos/deepseek.png` | DeepSeek | https://www.deepseek.com/favicon.ico(225×225 帧转 PNG) | 6.4 KB |

- 方案 A(用户确认):官方标原样用,不再叠渠道色方块。OpenCode 的标自带
  深色底(`#131010`,与界面底几乎同色)呈现为白色方框;DeepSeek 的鲸鱼是
  品牌蓝 `#4d6bfe` 透明底,直接放在界面底色上 —— 与列表里 DeepSeek 的
  主题色正好一致,所以**不要**再给方块,否则同色叠加会看不见。
- 统一入口 `iconHtml(c, cls, off)`:列表行 / 密钥行 / 额度申请行 / 紧凑条
  chip 都用它;`off`(未配置或取数失败)时字母方块变灰、官方标
  `grayscale + 50% 透明`,两边语义一致。
- 素材本地内嵌(前端 CSP 只允许 `'self'` 与 `data:`),出处与版权说明见
  `app/src/logos/README.md`(指明性使用,版权归各自所有者)。
- 验证:CDP 断言四个渠道的图标形态 + `naturalWidth > 0`(图真的加载成功,
  404 时会静默变空白所以必须查这一条);面板行、管理页两处、紧凑条 4x 截图核对;
  `verify-form.js` 全项回归通过。

### 追加调整 5(2026-09-17,用户 7 条反馈)

1. **OpenCode 大数字固定取「本月」窗剩余**(不再取最紧窗口):订阅按自然月结,
   5 小时 / 本周的波动不该左右整行观感。三行窗口明细照旧全部展示。
   副标题整行清空 —— 顺带去掉 `unstable` 的"未公开接口"前缀(内部实现细节,
   用户不需要知道;静默降级的行为不变)。
2. **字号整体 +1px**:渠道名 12→13、主数值 19→20、副标题/口径 10→11、
   三格标签 10→11 与数值 11.5→12、三窗口 10→11 / 10.5→11.5、汇总 9.5→10.5、
   底栏 10.5→11、管理页字段 11.5→12、胶囊 13→14、紧凑条 chip 11→12。
   放大后行高增加,把行内边距收紧 2px 才让四个渠道仍在一屏内
   (实测 listH 420 / contentH 414.65,不滚动);标题栏计数改短写法 `4/4`
   (完整说明进 tooltip)并禁止换行,否则六个按钮 + 放大字号会把标题挤成两行。
3. **贴边(吸附屏幕边缘)**:26×96 竖标签贴在当前显示器右边缘,按状态色渐变、
   竖排文字显示最紧张渠道;**鼠标移入展开**成吸附前的形态(右边缘保持贴齐),
   **移出 0.7 秒收回**;再点贴边按钮退出并恢复吸附前的尺寸与位置。
   贴边期间强制置顶(否则鼠标移过去也看不见它);正在设置页里操作时不收回。
   入口:标题栏 `⌷` 与胶囊上的 `⌷`。需要新增 `core:window:allow-set-position`。
   两个坑:`set-position` 被 ACL 拒绝时和当初 setSize 一样只留一行 console 报错,
   必须实测窗口坐标才发现;**收回定时器被 400ms 兜底轮询反复重置导致永不触发**
   (改成"已在倒计时就不重置")。
4. **全局置顶开关**:`Config.always_on_top`(默认开),Rust 新增 `set_pin`
   作为唯一写入口 —— 窗口状态与配置一起改,不会出现"按钮亮了其实没置顶";
   启动时按配置应用,前端启动读回真实窗口状态对齐 UI;折叠形态没有置顶按钮
   也能受控。
5. **置顶按钮选中态**:根因是图标用了 emoji 📌 —— emoji 自带颜色,CSS 的
   `color` 对它无效,选中/未选中只差一个 12% 透明度的淡蓝底。换成
   `currentColor` 的内联 SVG 图钉,选中态改**实心强调色**(蓝底白图标),
   与设置页开关双向同步。
6. **托盘左右键分工**:`show_menu_on_left_click(false)` —— 左键只切换显示/隐藏,
   右键才出菜单(Tauri 在 Windows 上默认左键也弹菜单,所以此前体验是
   "左键只会出菜单")。此项无法自动断言,需手动点一次确认。

验证:26 个单元测试通过;CDP 断言(大数字=本月窗、字号、一屏容量、置顶三处状态
一致且窗口状态真跟着变);真实鼠标(SetCursorPos,按 DPI 换算)驱动贴边的
移入展开 / 移出收回 / 退出还原全通过;面板 / 紧凑条 / 胶囊 / 竖标签截图核对。

> 测试期间踩到的工具坑:PowerShell 是 DPI 无关进程,`Cursor.Position` 用虚拟化
> 坐标(2048×1152),按物理坐标给值会点到别处 —— 物理坐标要除以缩放(1.25)。

### 追加调整 6(2026-09-17,贴边重做;方案经用户确认)

去掉贴边按钮,改成**拖到屏幕边缘自动吸附**;吸附后只留一条 8×64 的**纯色竖条**
(不显示任何数字),尽量少占屏幕;三种形态都支持。

- **自动吸附**:拖动松手(停止移动 250ms)时,窗口竖直边贴齐或探出屏幕左/右边缘
  即吸附到那一侧,左右都支持(左侧镜像圆角)。
- **取消吸附**:把窗口从边缘拖走即可,不需要按钮。
- **展开/收回**:鼠标移入竖条延迟 150ms 展开成吸附前的形态(8px 太窄,不加意图
  延迟鼠标路过就会弹开),移出 0.7 秒收回;吸附期间强制置顶。
- **三种形态都能拖**:紧凑条与胶囊整块加 `data-tauri-drag-region="deep"`
  (里面的按钮不受影响 —— Tauri 的可点击元素会阻断拖拽);面板里列表显式设为
  `false`,否则行点击会被拖拽吃掉。胶囊因此不再"点哪都能展开",展开只走 ▲。
- **两个坑**:①"拖到边"不能只判恰恰贴齐 —— 真实拖动时窗口通常**探出屏幕外**
  (实测松手时右边缘在屏幕外 215px),判定要写成"贴齐 `||` 探出",否则真实拖动
  一根都吸不上;②自动吸附后竖条不上色(重写时漏了立即调用 `renderDock`,
  要等下一次轮询才染色)。
- **验证**:真实鼠标(SendInput 按下/移动/抬起)把面板与胶囊分别拖到右边缘 →
  自动吸附成功;移入展开回到**吸附前的形态**(胶囊拖过去就展开成胶囊);
  移出收回;拖离解除并留在松手位置;三种形态的拖动区属性断言全部通过。

### 追加调整 7(2026-09-17,贴边折叠判定 + 自定义排序 + 隐藏未配置)

**1. 贴边:光标还在展开窗口里就被折叠(修根因)**
- 真正的原因不是"延迟太短",而是判定信号不可靠:窗口会周期性重绘(轮询 render
  替换 DOM),之后若没有任何鼠标事件,**Chromium 不会重算 hover** ——
  `body.matches(":hover")` 会稳定地报 false,于是"光标明明在窗口里"也被判成离开。
  实测:光标停在窗口内,每秒采样 hover 为 true→false→false→false…
- 修法:折叠**只认文档级 `mouseleave`**(真的离开窗口才触发),删掉基于 hover 的
  兜底轮询与"连续两次"计数;同时不再监听竖条自身的 mouseleave(展开时它被隐藏,
  浏览器会补发一次假事件)。
- 延迟按用户要求 0.7s → **1s**。实测:窗口内停 6 秒(跨过一次轮询重绘)始终展开,
  移出后约 1.4 秒收回。

**2. 自定义排序**
- 设置里「渠道排序」新增「自定义顺序」;`Config.channelOrder: Vec<String>`
  (容器级 `serde(default)`,老配置可读)。
- 管理页新增「渠道顺序」一节:每行渠道 + `▲` `▼`,改完立即保存并重排
  (排序是前端算的,不需要重新取数)。未列入数组的渠道(如以后新增的)自动附在
  末尾;首次调整时会把完整顺序落到数组里,之后不会因新增渠道而乱。
- 按用户确认:**自定义模式下不做任何自动干预** —— 失败/未配置的渠道也按用户
  排的位置显示。

**3. 隐藏未配置密钥的渠道**
- 展示区(面板列表、紧凑条)只显示已配置密钥的渠道;一个都没配时仍是引导卡。
- 管理页(密钥 / 渠道顺序 / 额度申请)仍列出全部渠道 —— 那是配置面,
  提前排顺序、提前配规则都有意义。

> 复现时踩到的混淆点:管理页打开时按设计不自动收回,导致"窗口内不折叠"的
> 断言在一轮里假阴性 —— 测试里要先 `closeManage()`。

### 追加调整 8(2026-09-17,数值样式统一)

问题:主数值 `¥18.99` 只有整数部分"有样式",小数位是 13px 灰字(原型稿的
"强调整数"设计),看起来像半截数字没渲染好。按设计稿的对照页改成整体一套样式:

- `.val .v i` 改为 `font-size/weight/color: inherit` —— 小数位与百分号跟整数
  部分完全一致(实测两部分都是 20px / 600 / 同色)
- 胶囊里的 `<i>`(百分号)同样 inherit
- 紧凑条:百分比跟数字一致;**金额仍隐藏小数位**(紧凑条只有 46px 高,
  宽度也紧张,这条保持不变)

### 追加调整 9(2026-09-17,缺陷审查 14 处 + 四个新功能)

**A. bug-report-2026-09-17.html 的 14 处缺陷:逐条核验,全部处理**

严重 3 处:
1. `providers.rs` 4SAPI 的 valid 写成 `code.unwrap_or(true) || is_active.unwrap_or(true)`
   —— 正常响应没有 `is_active`,右侧恒为 true,`code:false` 被吞掉当成功。改成
   `code_ok && active_ok`,并补测试 `four_s_api_business_failure_is_invalid`。
2. **CSS 层叠顺序**:`body.docked .xxx{display:none}` 与 `body.form-compact .list`
   等等优先级相同(0-2-1),同优先级取后者 —— 从紧凑条/胶囊吸附时内容重新显示,
   8px 窗口被 flex 一挤,竖条直接消失(只有从面板吸附正常)。把吸附态规则移到
   样式表末尾。实测:三种形态吸附后竖条都稳定 8px。
3. `fetch.rs` 消耗推算被 `result.valid` 连坐:取数失败时 day/week/month 一起置 None,
   但那是本地快照推算、与网络无关。改为只看 `kind == Amount && remaining.is_some()`;
   汇总条"N 个渠道"改为只数真正参与求和的渠道。

中等 4 处:胶囊口径统一到本月窗;`renderDetail` 不再重置区间选择(记住 sparkHours);
两个空开关(notify / collapse_on_blur)删除 —— 功能没实现,留着就是骗人;
`key_hint` / `provider_meta` 前端从未调用,命令与注册一并删除。

轻微 6 处:接口字符串进 innerHTML 前统一 `esc()` 转义;DeepSeek 缺字段不再冒充
`¥0.00`(缺就不显示);`applyForm` 加形态白名单并顺手关掉管理页;
删除三个必然抛错的陈旧 git 脚本(`build/git` 早已不存在);
`probe-frontend.js` 的 mock 对齐 Tauri 2 API(`T.dpi.LogicalSize` + setMinSize 等,
并让探针跑完自己退出);并发 cargo 打坏增量缓存那条记录在案。

**B. 四个新功能**

1. **OpenCode Go 三种形态都取本月窗**:此前只有面板用 `mainWindow()`,胶囊用的是
   接口给的 `remaining`(最紧窗)。统一走 `remainRatio()` 后,面板/紧凑条/胶囊
   的数字与状态色一致(实测:本月 96% vs 最紧窗 91%,三处都显示 96.0%)。
2. **胶囊去掉软件图标、宽度自适应**:去掉 `.mark`,宽度用离屏 span 量文本后
   取 132~240(变化小于 6px 不动窗口,避免数值抖动导致窗口跳)。实测 200 → **132**。
3. **单 exe 交付(静态链接 WebView2 加载器)**:见下节。
4. **托盘角标**:前端 canvas 画 32×32(外框图标同款造型 + 状态色角标),RGBA 交给
   Rust 换成托盘图标,tooltip 放与胶囊完全相同的文字 —— 渠道选择、轮播位、
   状态色都共用胶囊那一套逻辑(`PILL.list[PILL.idx]` + `toneOf`)。

**C. 单 exe 是怎么做到的(重要)**

- 机制:crate 里写的是 `#[link(name = "WebView2Loader.dll")]`,GNU ld 解析它时
  **只找 `libWebView2Loader.dll.a`**。于是把 MSVC 的 `WebView2LoaderStatic.lib`
  (10.5MB)与一个 MSVC CRT 垫片重打成同名归档,放进搜索路径。
- 垫片(`build/webview2-static/msvc-shim.c` + `msvc-alias.S`)提供 MSVC CRT 里
  gcc 没有的符号:`__security_cookie` / `__security_check_cookie`、线程安全魔法静态
  三件套 `_Init_thread_header/footer/epoch`、`operator new/delete` 与 `std::nothrow`。
  修饰名带 `?` `@`,GAS 要用带引号的符号名做汇编别名(`--defsym` 不接受这种名字)。
- **关键坑**:ld 的候选后缀包含 `.lib`,所以只删 `WebView2Loader.dll` 不够 ——
  导入库 `WebView2Loader.dll.lib` 会被优先命中,照样生成 DLL 依赖。两个都要删。
  build.ps1 与 app/src-tauri/build.rs 各删一遍(前者每次构建都跑,后者让直接
  `cargo build` 也正确)。
- 验证:`objdump -p` 导入表里 WebView2Loader 计数 **0**;把 exe **单独**拷进空目录
  启动,进程起来、WebView2 子进程 6 个、CDP 能读到正常渲染的 DOM(4 个渠道/13px 字号)。
- 已知无害噪音:链接时 MSVC 对象的 `.drectve` 段会让 ld 报
  `corrupt .drectve at end of def file`(MSVC 指令格式 ld 解析不了),不影响产物。

### 追加调整 10(2026-09-17,缺陷报告复测遗留项收尾)

报告复测结论是「10 处完全修复 / 4 处部分修复」,这里把 4 处遗留一次做完:

1. **转义最后一处漏网**:`refresh()` 的 catch 分支原来直接拼 `${e}`,补上 `esc(e)`。
   至此全仓 `innerHTML` 赋值点没有未转义的插值。
2. **`Config.form` 的过时注释**:删掉 `edge` —— 贴边不是独立形态,而是三种形态
   之上的吸附态,注释里写清楚了,免得下一个人去找一个不存在的形态。
3. **`app/README.md` 同步**:结构图改为实际布局(`core/src/` 而非 `src-tauri/src/`,
   补上 `capabilities/`、`logos/`、`fetch.rs`、`build/webview2-static/` 等);
   「已知限制」里那句"开机自启尚未接系统 API"是反的,改为实际状态并补单 exe 说明。
4. **构建并发锁**:`build.ps1` 在目标目录放 `.build-lock`,已有未过期的锁就直接报错退出
   (2 小时以上视为陈旧锁并接管)。实测:手动放锁 → 构建被拒并给出提示;正常构建
   结束后锁被释放。这条对应「并发 cargo 打坏增量缓存 → rustc 报 `no entry found for key`
   / `os error 5`」那个看起来像编译器 bug 的坑。

另外两处报告点出的隐患也一起处理了:

- **汇总计数的语义陷阱**:原来用 `c[est ? "day" : "month"]` 拿"是否推算"这个布尔量
  去选字段名 —— 今天恰好对,给今日/本周也加计数时就会读错字段。改成每个格子显式
  带自己的 `field`,注释文案也一并声明。
- **补回"密钥尾号确认"能力**:报告指出删掉 `key_hint` 让这个能力消失了。重新实现:
  Rust 侧恢复命令(复用既有的 `secrets::masked()`,只回显尾 4 位),前端在管理页
  密钥行异步补上 `已配置 · ••••aTNN`。存错 key 却以为是接口坏了,是最难排查的一类
  问题,这个尾号值得显示。
  (教训:改模板时 `renderKeyRows` 与 `renderClaimRows` 的 HTML 片段完全相同,
  按字符串替换会插错函数 —— 第二次用行范围定位才改对。)

### 追加调整 11(2026-09-17,安装包 + 自动更新)

交付形态从"只有绿色单 exe"扩成**两种**:NSIS 安装包(推荐,支持自动更新)+ 绿色单 exe。
设计文档 `DESIGN-INSTALLER-UPDATE.md`,发布流程 `RELEASE.md`,更新清单生成
`build/make-latest-json.js`。

- **安装包**:`installMode=currentUser`(装到 `%LOCALAPPDATA%\Programs\TokenScope`,
  不需要管理员)、`embedBootstrapper`(内嵌 WebView2 引导程序,断网也能装)、简体中文界面。
- **自动更新**:官方 `tauri-plugin-updater`;公钥在 `tauri.conf.json`,端点 =
  `releases/latest/download/latest.json`;`createUpdaterArtifacts` 产出 `.sig`。
  更新逻辑全在 Rust(前端是无打包器的静态页,拿不到 npm 包):`check_update` /
  `install_update` / `skip_update_version` + `update-progress` 事件。
  自动检查 24 小时一次且**失败也写时间戳**(防一次网络故障变成请求风暴);
  手动「检查更新」不受节流;跳过某版本后手动也不再弹。
- **两个卡死坑(都实测踩过)**:
  1. **签名环节会等交互输入密码** —— 密钥未加密,但不设
     `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 时 Tauri 会打印
     "expect a prompt for password" 然后永久等待(日志停在那行、CPU 1.5 秒/25 分钟)。
     连续两次打包都死在这里,第一次还误判成网络问题。显式给空密码即可。
  2. 从**已断开的会话**启动打包同样会卡(父 shell 没了、子进程等管道)——
     排查这类"打包不动"时先看 CPU 与文件时间戳,别只看进程还在。
- **安全**:私钥在 `~/.tauri/tokenscope.key`(**仓库外**,已确认未入库);
  发布物只含安装包 / `.sig` / `latest.json`;更新包带 minisign 签名校验;
  私钥丢失 = 已装用户永久收不到更新,务必备份。
- **发版实测**:安装包 4.34MB + `.sig` 420B + `latest.json` 717B 已上传 v0.1.0;
  端点取回的 latest.json 与本地**逐字节一致**;公钥与 `~/.tauri/*.pub` 的 sha256 一致;
  绿色 exe 打包后依然零 WebView2Loader 依赖。

### 追加调整 12(2026-09-17,绿色 exe 下线 + 安装包实测)

用户决定:**绿色单 exe 形态下线,只留 NSIS 安装包**。理由与设计文档一致 ——
两条交付线 = 两套更新逻辑 + 双倍回归,而 per-user 安装本来就不需要管理员。
绿色版已从 GitHub Release 撤下(代码里的静态加载器垫片**保留** —— 安装包里的那份
exe 正是靠它做到零 DLL 依赖,去掉反而要往安装目录塞 WebView2Loader.dll)。

**安装包实测(静默安装 `/S`,退出码 0)**:
- 实际安装路径(当时还是 TokenScope;0.1.3 改名后为
  **`%LOCALAPPDATA%\Grandettoken`** + `Grandettoken.exe`,历史记录保留):
  不是 `Programs\TokenScope`,文档里原先写错了,已改。当时目录内容:`tokenscope.exe` 6.6MB + `uninstall.exe` 77KB。
- **没有 `WebView2Loader.dll`** ✓ —— 静态链接在安装版布局里依然成立。
- 注册表卸载项 ✓(DisplayName/DisplayVersion/InstallLocation/UninstallString 齐全)、
  开始菜单快捷方式 ✓(`Start Menu\Programs\TokenScope.lnk`)。
- 启动安装版:界面正常渲染(4 个渠道、4/4 已配置)✓,**数据完全继承** ——
  四个渠道的密钥都在(与开发版共用 `%APPDATA%` 数据库 + Windows 凭据管理器),
  印证了"升级/重装不丢数据"。
- **自动更新链路端到端验证** ✓:在安装版里调 `check_update(force=true)`,它真的去拉了
  `releases/latest/download/latest.json` 并返回
  `{"current":"0.1.0","available":false,"checkedNow":true,"error":null}` ——
  端点可达、清单可解析、签名校验无报错(版本相同,判断正确)。

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

---

## 4SAPI 可用模型查询 + 一键配置到客户端(2026-10-12,v0.1.20 发布)

设计稿 [`design-4sapi-models.html`](design-4sapi-models.html),已实现口径见 `DESIGN.md` 第 12 节。
这里记取证过程、踩坑与实测结果。

### 取证(全部可复核)

**4SAPI 侧**(真接口,经代理 127.0.0.1:7897):

| 端点 | 实测 |
|---|---|
| `GET /api/pricing` | 200 · 103 588 B · **160 个模型** · 无需密钥 · 含 `group_ratio`/`usable_group`/`vendors`/`pricing_version` |
| `GET /api/status` | `quota_per_unit=500000`、`quota_display_type=CNY`、`display_in_currency=true` |
| `GET /v1/models` | 无 key → 401 `{"error":{"message":"Invalid token",…,"type":"new_api_error"}}` → 标准 OpenAI 兼容层 |
| `GET /api/user/self` | 无 key → 401(另一套错误体,`AUTH_UNAUTHORIZED`) |

**单价公式**不是猜的:从站点 pricing 页的 JS bundle(`index.076be96f81.js`)里抠出原文
`let e = 2 * t.model_ratio * d;` + `补全价格 = 2 * model_ratio * completion_ratio * 分组倍率`,
配合 `quota_per_unit=500000`(与挂件 4SAPI 渠道的「500000 积分 = ¥1」同一个数)确定口径为
**元/百万 token**。34 个模型是 `billing_expr` 阶梯计价(只标徽标,不给确定价),12 个按次计费。

**四个客户端的落点**(本机真文件;密钥字段只记长度):

| 客户端 | 落点 | 关键字段 |
|---|---|---|
| Trae CN / SOLO CN | `%APPDATA%\<安装>\User\globalStorage\state.vscdb` 的 `ItemTable` | 两条 `*model_list_map` + `*recent_user_selection_by_agent_label` |
| WorkBuddy | `~/.workbuddy/models.json` + `settings.json` 的 `model` | 条目 12 字段,`apiKey` 明文;BOM + 2 空格 |
| WorkBuddy AI | `~/.workbuddy-ai/models.json`(当前是 `[]`) | 同结构;`settings.json` 里还没有 `model` 键 |
| ZCode | `~/.zcode/v2/config.json` + `provider_config.json` | provider 定义 + `providerRules`/`providerModelRules`(`schemaVersion:1`) |

**Trae 的 modelId 口径用三个真样本破译**(单测里对着样本回归):

```text
内置:   solo_agent_lite_1__DeepSeek-V4-Flash-Official_null
自定义: solo_agent_3_custom_openai_compatible_custom_openai_compatible//claude-sonnet-5_2607458050
→ <agentLabel>_<config_source>_<provider>_<name>_<custom_model_id 或 null>
```

中段就是条目里的 `config_source`(内置=1、自定义=3)。顺手证伪了一个猜测:`custom_model_id`
**不是**模型名的哈希 —— crc32 / fnv1a32 / djb2 / java-hash / adler32 全试过,都不等于现成样本
`2607458050`(它是应用自己分配的号,所以新条目按"比现有最大值大且不冲突"生成)。

**WorkBuddy 的 `custom-local:` 前缀**来自服务端特性开关:在 `app.asar` 里找到
`const shouldPrefixId = !!ctx.current.productFeatures?.[ProductFeature.CustomModelIdPrefix];`,
而它同时给模型加了 `aliases: [原 id]`。既然是开关决定的,就只能**现读现学**(见 `learn_prefix`):
当前选中值带前缀 → 照此写;是裸 id 且命中我们写的条目 → 不加前缀;是内置模型 → 学不到就不写。

### 踩坑

1. **`json!` 的递归上限**:Trae 的自定义模型条目是 40 个字段的字面量,`json_internal!` 每键展开一层,
   撞上默认 128 的 recursion limit → `core/src/lib.rs` 加 `#![recursion_limit = "256"]`。
2. **`split_at(len - 1)` 在中文 tag 上 panic**:`"多模态"` 的最后一个字符占 3 字节,按字节切会落在字符中间。
   改成先按字节判(末尾必须是 ASCII、前面全是 ASCII 数字)再切片,并顺手把 `2K/4K`(分辨率)排除在
   上下文标签之外。
3. **`clip.exe` 要 UTF-16LE**:按控制台代码页解释字节流,中文标签走 UTF-8 会被切坏;
   密钥经 stdin 送,不进命令行(进程命令行对其它程序可见)。
4. **测试读回带 BOM 的文件不能用 `from_str`**:我们写 WorkBuddy 的文件时照原样保留 BOM(原文件有),
   而 `serde_json::from_str` 不吃 BOM —— 测试统一走 `read_json`(它会剥)。
5. **`set_config` 会抹掉后端学到的 `selection_prefix`**:前端手里那份是旧快照,整份覆盖等于清空。
   与 `skipped_version` / `win_x` 同一类坑,已加合并(后端已知的键以库里为准)。
6. **`tasklist` 的判定不能看提示文案**:中文系统的"没有运行的任务…"与英文不同;
   改成按 CSV 首列(进程名整串、忽略大小写)比对。两个 WorkBuddy 共用 `WorkBuddy.exe`,
   分不清跑的是哪一个 → 只要有一个在跑,两个都判为"正在运行"(保守方向)。
7. **`serde_json` 要开 `preserve_order`**:默认 `BTreeMap` 会把写回客户端配置的键序按字母重排,
   等于把我们没打算动的部分也"改"了一遍。`indexmap 2.14.2` 已在锁文件里,离线可用。
8. **`rustup default` 变成了 msvc,release 链接直接炸**(2026-10-12 本轮实测):
   ```
   liblibsqlite3_sys-*.rlib(...sqlite3.o) : error LNK2019: 无法解析的外部符号 ___chkstk_ms
   G:\...\target\release\deps\tokenscope.exe : fatal error LNK1120: 1 个无法解析的外部命令
   ```
   `LNK2019/LNK1120` 是 **MSVC 链接器**的报错格式,而 `___chkstk_ms` 是 MinGW 运行库符号 ——
   就是说:sqlite3.o 是 MinGW gcc 编的,最后却交给 MSVC 的 `link.exe` 去链。
   根因是 `rustup default` 指向了 `stable-x86_64-pc-windows-msvc`(本机两套工具链都装着,
   gnu 那套是历史默认;别的项目把默认改掉了)。**本项目只能走 GNU**(见 README 的
   WebView2Loader 静态链接那段)。修法二选一,都不用动全局配置:
   ```powershell
   cargo +stable-x86_64-pc-windows-gnu build --release   # 单次指定
   rustup default stable-x86_64-pc-windows-gnu           # 或把默认改回来
   ```
   注意 `build/build.ps1` 只设 PATH/CC/CXX,**不指定工具链** —— 它默认 `rustup default` 是 gnu。
   本机默认被改掉之后,`build.ps1 build` 会以同样的方式失败。
9. **非 async 的 Tauri 命令跑在主线程上 —— 一次卡住,整个挂件不响应**(本轮最贵的一课):
   模型页的命令一开始全写成普通 `fn`,里面却在起 `tasklist` 探进程、读客户端配置、读凭据管理器。
   真机现象:点了一次「写入预告」之后,界面<b>所有</b>新命令都不再返回(连 `inject_state` 也超时),
   而且 WorkBuddy 渠道那一行从面板上消失了 —— 因为它的会话读取(`wb_session_of`)要回主线程取
   Cookie,主线程被占住就直接超时,渠道退化成"没有凭据"。
   排查路径也值得记:后端 <code>get_model_catalog</code> 单独调用 9 秒内就返回了,
   价目表/可用集合都是真的,而界面停在「正在取…」——**"后端成功 + 前端抛异常"与"请求还没回来"
   在截图上完全一样**。后来拿 CDP 直接 evaluate `renderModels()` 才看到 `timeAgo is not defined`
   (前端改名了但没同步调用点),再往后是三个命令依次超时,才定位到主线程被占。
   修法两条:命令改 `async fn` + 把阻塞段放进 `tokio::task::spawn_blocking`;`tasklist` 加 5 秒超时
   (超时就杀掉并退化为"未运行")。这跟取数路径早就写下的那条教训是同一条
   —— 文件 IO / keygen RPC / 进程探测都不是主线程该干的活。

### 单测

`cargo test`(core)= **91 passed**。本轮新增 65 个,覆盖:

- 价目表解析(缺字段 / 脏数字 NaN / 阶梯 / 按次 / 中文 tag 不 panic / 图像分辨率不算上下文)
- 单价换算(站点公式对拍、分组回退并标明、分组表缺失时不给价)
- 可用集合 join(`usable: null` 与 `false` 是两种状态)
- 缓存往返与入口域名优先级(只认"密钥被接受过"的域名)
- 四客户端写入:幂等不重复、撞名更新且保留用户起的名字、BOM 保留、结构不认就拒写、
  ZCode 复用同一个 provider、`deletedModels` 解禁、Trae 每个 agent 组各一条、
  备份 → 校验 → 回滚逐字节一致、备份只留 5 份、台账只留 20 条
- Trae modelId 口径对着**两个真样本**回归;`custom_model_id` 不冲突
- 目标在跑 → 拒写(硬规矩进单测,不只写在文档里)

### 发版(v0.1.20)

- 两处版本号同步(`app/src-tauri/Cargo.toml` + `tauri.conf.json`)= 0.1.20;
  `build/make-latest-json.js` 断言通过,`latest.json` 的 url 指向
  `releases/download/v0.1.20/Grandettoken_0.1.20_x64-setup.exe`。
- `build/build.ps1 bundle`(签名私钥 + **显式空密码**从 bash export,踩坑见 RELEASE.md)→
  安装包 4.60 MB + `.sig` 424 B;构建脚本尾部自带断言 `import table clean (no WebView2Loader.dll)` 通过。
- **`build.ps1` 本轮加了一行 `RUSTUP_TOOLCHAIN`**:把工具链钉死成
  `stable-x86_64-pc-windows-gnu`。原因是本机 `rustup default` 已被别的项目改成 msvc,
  不钉的话 `bundle` / `build` 会以 `LNK2019 ___chkstk_ms` 失败(见踩坑 8)。

### v0.1.21(2026-10-12,发布 0.1.20 之后紧接着的一处口径收紧)

用户看过 0.1.20 的模型页之后提的要求:"只看模型里只看当前密钥可用的和已配置的,不需要看全部的"。

- 默认过滤从「128 个编码/对话模型」收成 **可用 ∪ 已配置 = 20 个**(实测:可用 18 + 已配置 3,去重后 20),
  两个芯片变成「只看能用上的」「只看编码/对话」,都默认开。
- **并集而不是交集**:`gpt-5.6-luna` / `gpt-5.6-sol` 是早先配进 Trae 的,但不在当前这把密钥的分组里 ——
  交集会把它们藏掉,而"当初配了、现在用不了"恰恰是最该看见的。它们现在带着「当前 key 不可用」和
  「价按「gpt-官方」分组」两个徽标留在列表里(真机截图确认)。
- 可用集合没取到(`usableAt == 0`,比如没配密钥)时**不做这层过滤**,只在口径行标「未配密钥 / 可用集合没取到」,
  空态里直接给两句人话(去填密钥 / 点掉芯片看全部价目表)。
- 试跑时把 `modelShowAll` 点成了 `true`(页面里那份活配置与 `get_config` 返回的副本不是同一个对象,
  第一次复位没生效 —— `set_config` 落库成功了,但页面内存里仍是旧值,要改**页面里那个 `CFG`** 才立刻生效)。
  已复位为 `false`,真机复查:芯片两个都亮、口径行 `显示 20 / 160 · 可用 18 · 已配置 3`、行数 20。

### v0.1.22(2026-10-12,取消写入)

用户口径(澄清后):"需要支持把模型配置写入 trae workbuddy workbuddyai zcode,或者从其中删除。
不需要支持 codex 和 opencode。"即目标还是那四个,要的是写入的逆操作。

- 先问后做:机器上没有字面叫 "agent" 的东西,而 `codex.exe` / `OpenCode.exe` 都在跑、
  OpenCode 里还躺着一份被 `disabled_providers` 停用的手配 4sapi provider —— 差点就往那两个方向做了。
  问清之后确认"agent"指的是这四个客户端里的删除动作。**这次的教训:名词对不上就先问,别猜着写契约。**
- 实现:`inject::remove()` + 各客户端 `remove_*_site()`,与写入共用同一套规矩
  (目标没运行 / 先备份 / 只动我们的条目 / 读回校验,不过就回滚)。
  WorkBuddy 类删条目 + 选中项指向它时删 `model` 键;ZCode 删模型、摘 personalModelIds/modelOrder,
  最后一个模型时连 provider 与 providerRule 一起收;Trae 每个 agent 组各删一条 + 摘指向它的选中项。
- 幂等:本来就没有 → 不写盘、不建备份目录,回执「本来就没有」。单测里连这一点都断言了
  (包括 `backup` 为空)。
- 新增 4 个单测(95 passed):删条目+清选中、选中项属于别家时不乱清、ZCode 删到最后收 provider、
  Trae 跨组删+摘选中+幂等。
- 预告新增 `configured` / `removal` 两个字段,界面据此渲染「当前状态:已配置」与「取消写入」按钮。
  真机只读实测(未点删除):Trae CN 报 17 条(17 个 agent 组)且选中项正指向它,SOLO CN 报 17 条但未指向它。

### v0.1.23(2026-10-13,闪窗修复 + 界面去分组 + 过滤规则钉死)

用户实测三条反馈,一次做完:

1. **"打开模型功能时,会弹出多个命令行窗口闪过后关闭"** —— 真 bug,而且是这一版新引入的:
   探「客户端在不在运行」走的是 `tasklist`,它是控制台程序,而挂件是 GUI 子系统进程 ——
   Windows 默认会给每个子进程配一个新的控制台窗口。打开模型页要探 4 次(两个 WorkBuddy 共用
   `WorkBuddy.exe`,名字不同但同一个进程名也算两次),所以是"好几个黑框闪过"。
   修法:所有自起的子进程加 `CREATE_NO_WINDOW`(`0x0800_0000`,`tasklist` 与 `clip.exe` 都加),
   并把同名进程探测**去重**(一轮最多 3 次)。修完真机复查:客户端在跑/不在跑的判定与修复前一致。
   教训一句话:**GUI 程序起任何子进程都要显式关掉控制台窗口**,默认行为就是弹窗。
2. **"模型里不要列出来分组"** —— 删掉分组下拉、分组说明行、每行的「N 个分组可用」与「价按 X 分组」徽标;
   口径行只留「单价按站点自动分组折算」。**不可用那几行不再显示单价**(它的价不属于这把 key 的分组,
   要显示就得写清分组名,而那正是要拿掉的东西)。
3. **"不要列出来不可用的 / 不能用但配置在 trae wb zcode 里的要显示、能删、删没了就不可见"** ——
   规则从"默认开、可点掉的芯片"改成**固定规则**:列表 = 可用 ∪ 已配置(第 2、3 条一起看,
   并集是必须的:只有看得见才删得掉)。取不到可用集合时**只列已配置的**并在口径行说明原因;
   删掉配置后该行不再满足规则,下一次刷新自然消失。

真机复查(2026-10-13):芯片只剩「只看编码/对话」/ 分组下拉不存在 / 口径行两行 /
20 行(18 可用 + 2 已配置,后两行带「不可用」徽标且**无单价行**)。

### v0.1.24(2026-10-13,修「展开又折叠后留残影」)

用户报:"opencodego 在展开又折叠以后会有显示问题"(附截图:展开过的那一行底部露出一条被切断的按钮)。

- **根因**:行展开/折叠用的是 `grid-template-rows: 0fr ↔ 1fr` 的真高度过渡(动效 C)。而 `.rbody-in`
  自带上下内边距(4px + 13px = 17px)—— **内边距压不掉**,所以 `0fr` 的轨道最小只能收到 17px:
  折叠后那一行比"从没展开过"高 17px,并在底部露出 17px 被切断的内容(OpenCode Go 上就是那排
  区间按钮的上半截)。没展开过的行没有 `.rbody-in` 子节点,所以不受影响 —— 这也解释了为什么
  "只有展开过再折叠才看得见"。
- **排查过程**:先按 flex 收缩想过(`.list>.row{flex:none}`,排除)、又试过 `box-sizing:border-box`
  (全局 `*` 已经是 border-box,无效),最后用 CDP 在真机上量 `getComputedStyle(rb).gridTemplateRows`
  才定位:折叠态是 `17px` 而不是 `0px`。**测量优于推测**:候选修法也是在真机上注入 CSS 现测的
  (候选 D「折叠给容器 height:0」量出来轨道仍是 17px,直接淘汰)。
- **修法**:`.row:not(.open) .rbody-in{padding-top:0;padding-bottom:0}` + 给 `padding` 加
  `transition`(折叠时内边距一起收,不跳);左右内边距保留(不影响高度,负责对齐)。
- **真机复验**(0.1.24 二进制):折叠后行高 **128**(= 从未展开时的一致值)、轨道 **0px**;
  展开 **170.208px**、再展开与「折叠 → 轮询重建 → 再展开」都正常;DeepSeek 同序列 188px 正常。
- 教训一句话:**`0fr/1fr` 这种真高度过渡,容器里任何"压不掉的尺寸"(内边距、边框、最小高度)
  都会成为收不干净的残留** —— 收的时候把它们也归零。
- 另记一条流程教训:这次验证绕了弯路 —— `cargo tauri build` 会把产物改名成 `Grandettoken.exe`,
  而 `cargo build` 产出的是 `tokenscope.exe`;`launch-debug.ps1` 按"最新时间"选二进制,
  我手动写死了旧路径,导致**对着陈旧二进制量了两轮,差点误判修复无效**。
  验证前先确认在跑的那个进程的 `Path` 与构建时间。

### v0.1.25(2026-10-13,模型页多来源 + 弹层 + 价格降序)

用户四条要求一次做完(1A–7A):Hapi 与 OpenCode Go 也能看/配模型;写入确认从列表底部改成弹层;按价格从高到低排。

- **多来源**:`models.rs` 新增 `SourceDef`/`source_hosts()`/`source_of_url()`/`fetch_source_ids()`/`rows_from_ids()`;
  `ModelCache` 增加 `sources` 分桶(id-only 来源用),4SAPI 的价目表字段原样保留(不回归)。
  `inject.rs` 全面按来源判定:WB 条目看 `url` 属于哪个来源、ZCode 每来源一个 provider、Trae 删条目要求
  `base_url` 也属于该来源(否则删 Hapi 那条会把 4SAPI 同名那条一起删掉)。
- **真机实测**:Hapi 用这把密钥真取到 **14 个模型**(`gpt-6-luna` / `gpt-5.5` / `gpt-5.6-*` / `gpt-image-2.5-*`);
  Go 38 个(公开);4SAPI 18 行可用,首行 ¥10.00、末行 ¥1.00 —— 价格降序成立。
- **顺手消掉待验项**:设计稿第八节第 1 条(Hapi `/v1/models` 认不认这把 key)——**认**,14 个。
  第 2 条(价目接口认不认 API key)按 7A 留到 P4。
- **弹层**:`#mdlSheet`(列表末尾)→ `#mdlOv` 覆盖层;写入预告/取消写入/结果/客户端详情四态都在里面,
  按钮固定底部、内容自己滚、Esc 关闭。前两轮"点了没反应"的观感问题(其实是渲染在列表底部看不见)就此消失。
- 仍未真机验:真正写进客户端那一步(只读预告已验)与 Hapi 的价格探针。

### v0.1.26(2026-10-13,每日签到 M1+M2+M3 一部分)

按 `design-auto-checkin-2026-10-13.html` 的十项裁决(全 A)实现。**M0 只读取证先跑,结论回写文档**(裁决 10A)。

- **M0 结果**(真机,只读):Trae status 通 —— HTTP 200 `{"checked_in":true,"code":0,"credits":100,…}`(字段全在顶层;
  `{}` 与 `{"req_source":1}` 都行);设备号是 `icube-dc` 键名里的 16 位十进制数;
  **WorkBuddy 的 Cookie 会话能打签到接口**(`already · +100`)→ 第 4 节的 OAuth 改造**不必做**。
  今天两边都已签,所以 claim 路径与"到账终态""签到包 7/31 天"留到明天有真领取时再验。
- **落地**:`core/checkin.rs`(纯逻辑:北京日、抖动、退避、幂等、状态机 + 网络层)、`store.rs` 的 `checkin_log`、
  `config.rs` 的 `checkin` 配置、外壳的 `checkin_state`/`checkin_now` 两条命令 + 每日节拍器(独立于取数轮询),
  界面是渠道卡上一行 + 设置页「刷新与启动」里的签到区(裁决 8A)。
- **默认关**:两渠道初始 `enabled=false`,一个请求都不发;界面写「签到未开启 · 开启签到」。
- **踩坑 11**:**Tauri 的 `Builder::setup` 只有一个槽位** —— 我另加一个 `.setup()` 把原有那个
  (负责 `manage(AppState)` 与轮询)顶掉了,应用启动后第一条命令就 `state() called before manage()` panic。
  节拍器必须挂在原有 setup 的末尾。真机第一次启动即暴露,修完复查正常。
- **踩坑 12(流程)**:**验证前必须确认在跑的是哪个二进制**。中间一次 `build.ps1 build` 其实失败了(输出被
  `tail -2` 截断,只剩一行版权信息),我对着旧二进制又量了一轮,得出"修复无效"的错误结论;加探针重编后才看清。
  与 v0.1.24 那次"路径写死旧的"是同一类错误的第二次 —— 下次先 `ls -l` 看时间戳再跑。

### v0.1.31(2026-10-13,写入 Trae 真库验证 + 拒写按钮不再假死)

用户问「往 trae 里写是真的成功了吗」。当时诚实回答:**没有** —— 之前所有真机检查只走到
拒写分支(Trae 在跑 → 按钮写「先退出它」)与只读预告;成功写入只有合成库单测。这轮补上:

- **验证方法(不碰真配置)**:临时在 `inject.rs` 里加一个测试(跑完已移除),把本机真实的
  `%APPDATA%\Trae CN\User\globalStorage\state.vscdb`(921,600 B)**只读拷贝**到临时目录,
  在副本上把整条路跑一遍。
- **结果(全绿)**:写入 `added=17` —— **17 个 agent 组各一条**;读回校验 `true`;
  再写一次 `added=0 / updated=17`(幂等);取消写入 `removed=17`、校验 `true`;
  备份 → 写 → 回滚后回到写入前。最硬的两条:**写入+取消之后,ItemTable 全表按 key 排序的
  快照与原始库逐字符相同**(别的条目、顺序都没动 → 精确可逆);两次写入后同样相同(重写不留痕)。
  界面层也拿真库过了一遍:`trae_removal` 写前 `false` / 写后 `true`(读回 17 条 · 17 组)/ 取消后 `false`。
  原始库字节数不变(全程只读)。
- **顺带挖出并修掉一个"假死"**:弹层打开时按钮按当时的运行状态**禁用** —— 用户是**关掉 Trae
  之后**回来点的,按钮还是灰的、点了毫无反应,观感就是"写入没成功"(与之前「隐藏渠道点了没反应」
  同一类)。现在按钮不禁用,文案「写入(Trae CN 在运行 · 点这里重查)」:点一下重新生成预告,
  还挡着就把「HH:MM:SS 刚查过,它还在运行」就地写出来并弹提示;放开了就直接接着写。
  同一处顺手加了两条:busy 闸(连点两次写入不再各备份一份)与成功提示带上生效方式
  (「已写入 Trae CN · 下次启动 Trae 时出现在模型列表里」)。
- **真机端到端(用户授权后跑完)**:退出 Trae CN 全部进程 → 在挂件模型页点第一行
  `claude-opus-4-5-20251101` 的 Trae 点 → 弹层「写入」→ 结果「Trae CN / TRAE SOLO CN 各新增 17 条」,
  备份 `backup-inject/20261015-185058/state.vscdb`(921,600 B = 写入前那份,字节数一致)。
- **磁盘核对**:Trae CN 组内条目 192 → 209(正好 +17,17 个 agent 组各一条);ItemTable 行数不变
  (只改了两个 `model_list_map` 的值);条目字段与模板对齐(`base_url=…/v1/chat/completions`、
  `config_source=3`、`status/selectable=true`、`ak` 51 字符)。
- **Trae 自己认了(决定性证据)**:重新启动 Trae CN 后它重写了两张 `model_list_map` ——
  在**当前在用的那张**(键名 `<id>_AI.agent.model.model_list_map`)把我们 7 个 agent 组的条目
  **原样留下**(字段与 `custom_model_id=2061458791` 一字未改);旧那张(`<id>:AI.agent…`)
  里我们写的 9 组被它清掉,而它自己的内置条目一条没动。Trae 自己的日志:
  `[trae-chat-core][ModelRestrictionDiagnostic][provider] snapshot changed {"agentType":"solo_agent"…
  "groupSequence":[{"group":"preset",…},{"group":"custom_models",
  "models":["solo_agent_3_custom_openai_compatible_custom_openai_compatible//claude-opus-4-5-20251101_2061458791"]}]}`
  —— 模型进了它自己的 `custom_models` 组,模型 key 口径与设计稿 2.1 破译的完全一致
  (`<agent>_3_<provider>_<name>_<custom_model_id>`)。
- **界面侧回读**:模型页第一行 Trae 点变成「已配置 · 点开可更新或取消写入」,弹层给
  「取消写入(移除这条)」—— 写入/取消的闭环在真机上闭合。
- **两处如实记下**:(a) 报告的「新增 17 条」是"写进去 17 条",Trae 启动后会清掉它不用的那套 agent 组,
  这一代版本实际存活 **7 组**(solo_agent / builder / builder_v3 / chat_v3 / code_reviewer /
  code_review_summary / refactor);(b) `TRAE SOLO CN` 的安装已不在(只剩数据目录),挂件按
  "库存在即目标"的口径照样写了一份 17 条 —— 无害,但"没安装就跳过"是不是更好,是个产品决定。
- **遗留**:真正发一条请求让它回话会产生真实计费,仍留给用户点。

### 还没真机验的部分(诚实清单)

1. **写入后客户端真能跑起该模型**:**写入**与"客户端自己认不认"都验完了(v0.1.31:真库副本上 17 组各一条、
   幂等、精确可逆、回滚有效;用户授权后真机写了一次,Trae 重启后把我们 7 组条目留在当前在用的模型表里,
   并把它记进自己的 `custom_models` 组 —— 见上面日志)。只剩"发一条最小请求看它真的回话"会产生真实计费,
   不由挂件代跑。
2. **WorkBuddy / WB AI 的选中项写入**:前缀学到之前只写条目。第一次在客户端里选中后,
   `inject_state` 会读到并记住口径,之后才自动写选中项。
3. **Trae 的选中项**:实验开关默认关(口径已破译并有单测回归,但客户端认不认只有真机能验)。

## Trae 自定义模型:能持久的是「云端添加」,改本地库只是改缓存(2026-10-15)

### v0.1.31 之后查清的真相

用户给的文档(`~/Documents/Trae-CN/temp_20261015_01/Trae自定义模型-接口自动化添加说明.md`)
指出 Trae「添加自定义模型」是**云端动作**;这轮顺着它把契约挖到了字节级,并在真机上跑通。

- **契约(从 Trae 自己的 bundle 里挖出来,不是猜的)**:`resources/app/node_modules/@byted-icube/ai-modules-chat/dist/index.mjs`
  - transport 信封:`{service:"model", method:"add_custom_model", data:{…}}`;HTTP 体 = 字段平铺 + `Request` 信封
  - 字段:`provider`(自定义走 `custom_openai_compatible`)、`model_name`、`ak`、`is_custom:true`、
    `auth_type`(枚举 API_KEY=0 / AK_SK=1)、`sk`、`region`、`base_url`、`custom_model_type`、
    `config_detail{multimodal, model_hyper_params{max_tokens, prompt_max_tokens, thinking_enable,
    temperature, top_p, top_k}, max_turn}`、`display_name`
  - 编辑/删除:`update_custom_model`(action 枚举里有 UPDATE/DELETE,`id` 用 `custom_model_id`)
  - 鉴权:`Authorization: Cloud-IDE-JWT <token>` + `X-Trae-Client-Type: ide`,host `trae-api-cn.mchost.guru`
- **token 才是文档里那个 401 的根因**:`.trae-cn/trae-jwt-token`(925 字符,文件那份)怎么带都 401;
  **`storage.json` 里 `iCubeAuthInfo://icube.cloudide`(tc 容器,解密后 1004 字符)那份才有效** ——
  正是挂件签到一直在用的同一个凭据(签名:`Cloud-IDE-JWT` + `x-device-id`)。
  真机实测:`GET /api/ide/v1/providers` → 200,只要 `Cloud-IDE-JWT` + `X-Trae-Client-Type: ide`,设备号/版本/区域都不需要。
- **真机跑通**:`POST /api/ide/v1/add_custom_model`(4sapi 的 claude-opus-4-5-20251101)→
  `{"code":0,"message":"success"}`;重启 Trae 后它把服务端那条**刷进本地缓存**
  (`<uid>:AI.agent.model.model_list_map`,10 个 agent 组,`custom_model_id=2911517954` 由**服务端分配**),
  用户界面侧从此认它。
- **同一轮刷新把「我们写进本地库的条目」清掉了** —— 这正是 v0.1.31 那次的全部真相:
  改 `state.vscdb` 只是改**缓存副本**(`model-list-fallback`),任何一次成功刷新都会重写;
  界面列表来自服务端,所以"日志里认了、界面里没有"。
- **服务端权威条目 vs 我们本地模板的差异**(实锤了我们模板哪里不对):
  | 字段 | 我们写的 | 服务端真值 |
  |---|---|---|
  | `base_url` | `https://4sapi.com/v1/chat/completions` | **`https://4sapi.com/v1`**(由 Trae 自己拼路径) |
  | `config_name` | 缺 | `custom_openai_compatible//<model>` |
  | `custom_model_type` | 缺 | `""` |
  | `use_remote_service` | 缺 | `false` |
  | `prompt_max_tokens` | `null` | `1000000`(config_detail 里传了就有) |
  其余(`config_source=3`、`auth_type=0`、`client_connect=true`、`display_name`=裸模型名、
  `icon`/`features`/`context_window_size`/`max_turns`/`ak`/`sk=""`)与我们模板一致 —— 模板本身没白写。
- **挂件侧待裁决**:要不要把「写入 Trae」改成走云端(账号级、跨设备持久、可被 `update_custom_model` 删除),
  本地库那条降级为离线兜底(并把上面四个字段补齐)。用户 2026-10-15 已在 Trae 界面里成功手加过一条
  (`OG-deepseek-v4.1-flash`,id 2923059202),说明界面/云端这条链路对他可用。

## 全量体检第二轮:36 条缺陷一次修完(2026-10-16,v0.1.33)

**怎么查的**:先把 133 项单测跑绿当基线,再按文件切分做了一遍**全量**静态审查
(core 12 个模块、`lib.rs` 2638 行、`app.js` 4002 行、`styles.css` 798 行、7 个 `.ps1` + 8 个 `.js`),
配机器化交叉检查:37 个 `#[tauri::command]` × 48 个 `invoke()` 调用点的参数契约、
DOM id / CSS 类引用、`innerHTML` 转义扫描、生产代码 `unwrap/expect` 清点、密钥外泄扫描、编码/BOM。
每条 P1 都回到源码逐行复核过(不采信"看起来像")。**结论:0 个 P0、6 个 P1、约 30 个 P2。**

**6 条 P1(都已修)**
1. **配置坏了却仍被整份覆盖**(`lib.rs` `check_update`):启动时 `Config::try_load` 失败会置
   `CONFIG_BROKEN` 并提示"设置保存已暂停,以免覆盖还能修复的原始配置",但 `check_update`
   是唯一没问 `refuse_if_config_broken()` 的整份写入者,而默认配置 `auto_check_update=true`、
   `last_check_at=None` 两道节流都不拦 → 前端(启动 30 秒后无条件调一次)会把这行 JSON 永久覆盖。
   修:损坏时只更新内存时间戳,不落库。
2. **「取消写入」删掉用户自己的条目**(`inject.rs`):归属判定是 `url.contains(host)`,
   `https://mirror-4sapi.com/v1` 也算"我们写的";`4S-` 前缀更是用户自己的命名习惯。
   修:改成**精确主机名**比对(或整串等于我们会写的 base_url),`4S-` 前缀只在 url 为空时兜底 ——
   与读路径 `models::source_of_url` 终于同一口径。
3. **签到没到账却记成「已领」**(`lib.rs`):回查只在返回 `Already` 时才改判,其余(含
   "仍显示未签"这个最强反证)一律照抄 `Claimed` → `done_for_day` 为真 → 节拍器与手动按钮
   当天都不再试,积分静默丢掉。修:回查仍可领就降级为可重试。
4. **汇总条把没配密钥的 OpenCode Go 套餐摊进消耗**(`app.js`):后端会把 `plan` 连占位视图一起挂,
   前端只按 `plan` 过滤 → 全新安装(只配 4SAPI)凭空多出约 ¥2.37/天的"套餐摊销"。
   修:`plans` 必须带 `hasKey`。
5. **套餐周期"实测学习"永久失效**(`fetch.rs`):`cycle_cached` 只收套餐渠道(长度 1),
   而读取按 `PROVIDERS` 下标(opencode-go 是 `[1]`)→ `cycles.get(1)` 恒为 `None`,
   `learned` 永远是 false,且每轮都白写一次 SQLite。修:按 `PROVIDERS` 全表对齐。
   (旧测试传的是单元素切片,下标恰好对上,所以一直没暴露。)
6. **吸附展开态的「收回 ✕」压住底栏「退出」**(`styles.css`):`#dockClose` 固定在卡片右下
   8px、`z-index:30`,而展开时 `docked` 已移除、底栏是显示的 → 点「退出」被它截走,变成收成竖条。
   实测重叠 87%×94%;修:`body.dock-open .foot{padding-right:44u}`,留出 12px 净空。

**P2 里值得记的几条**:`url_memo` 的"失败就清备忘"分支实际不可达(渠道会永久钉在失效的备用域名上);
`main_window_reset` 回退到 `windows.first()`,与它自己的注释相反;上期窗口无快照时
`consumption_since` 报 `Some(0.0)` 而不是 `None`(环比被凭空抬高);DeepSeek 的
`balance_infos[].currency` 从没读过(USD 账户被当人民币显示并计入合计);`model_price` 缺字段
被 `unwrap_or(0.0)` 顶替成"¥0.000/次";响应体上限在读完整个 body 之后才检查;
`-N` 后缀的备份目录(同秒连点两次时自己产生的)因为只认 15 位而**永不被清理**
(里面是明文密钥);`open_path` / `inject_rollback` 的白名单是不解析 `..` 的字符串前缀
(可交给资源管理器执行任意 exe);WorkBuddy 读回校验只断言写入前就已成立的 id+url;
`legacy_left()` 先 trim 后 split,尾引号没去掉 → "旧版还在"的提醒永不出现;
`wb_clear_session` 的结果被 `let _ =` 吞掉 → 命令返回成功但 Cookie 还在;
`check_update` 用裸 `.lock()`(store 被污染时 24 小时节流失效);签到没有在途互斥
(节拍器与手动点击可同时 POST claim);`quit` 不等在途的客户端写入;`set_pin` 在主线程写 SQLite;
密钥箱没有 `UNIQUE(platform,label)`(双击保存能插进两条一样的);前端 `aria-expanded` 引用了
一个未声明的 `open`(恒为 true);密钥箱读取失败被显示成"密钥箱是空的";
`.note/.dim/.ov-h/.okbox/.err` 五个类**压根没有 CSS 规则**;`scan-secrets.ps1` 对自家更新私钥
结构性失明且命中时原样回显密钥;`verify-ui-0923.js` 点了一个不存在的 `#btnSort`,必然抛异常
且一条结果都不打印;`cdp-probe.js` 匹配不到就连到任意 Chromium 还 `exit 0`。

**顺带修掉的"宣称已修但没修完"**:金额千分位(P3-30)、紧凑条点「+N」会拖窗口(P3-32)、
`inject.rs` 里最后一处硬编码"不是 4SAPI"(P0-4 残留);另外 4 处 `filter_map(Result::ok)`
静默吞坏行改成 `eprintln!` 后丢弃。

**复测**:core `cargo test` **153 passed / 0 failed**(净增 20 项,其中 13 项经"临时还原后再跑"
证明在修复前确实失败)、0 告警;`cargo check --all-targets`(整个 Tauri 壳)零错误零告警;
8 个 `.js` 全过 `node --check`;7 个 `.ps1` 纯 ASCII 无 BOM;IPC 契约 37 命令 × 48 调用点全对齐;
`build/scan-secrets.ps1` 在真实仓库上仍 CLEAN(夹具仓库能全部命中,公开公钥不误报)。

**发版**:v0.1.33。**签名环境变量的坑这次换了条正路**——`cargo tauri` 只在
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 缺席时提示密码,而 PowerShell / Git Bash / .NET
**都无法把"存在但为空"的变量交给子进程**(实测:`$env:X=''` 是删除;Git Bash 的
`export X=''` 到不了 Windows 子进程;`ProcessStartInfo` 的空值也被丢掉)。真正的出路是
tauri-cli 自己的 `ci` 标志(`bundle.rs`:`or_else(|| if ci { Some("".into()) })`,
来源就是 `env = "CI"`):**未加密的密钥只要 `$env:CI = 'true'`,等价于给了空密码**,
纯 PowerShell 就能跑。`build/build.ps1` 的 bundle 守卫据此放宽(密码 或 CI 二选一),
`ci` 在打包器里只影响这一个分支。

**本次未修(记录在案)**:①签到只取第一份 Trae 凭据(`candidates().next()`,双装 Trae 且
首份 token 过期时每天报"凭据失效",取数那条路是会轮换的);②`appauth` 把"文件被占用/写到一半"
与"真的没登录"混成同一句话;③WorkBuddy 登录的三个状态位是进程级 `AtomicBool`、无会话号、
无超时(关窗后立刻重开可能停在"登录中");④每轮取数有约 9 次重叠的 O(n) 区间扫描(既有 O-1,
行数随 90 天保留期增长)。前三条是 P3、影响面窄,第四条是明确的优化项,都不阻塞本次发版。

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
- ~~待真机确认:真实密钥下的展示效果~~ 已确认:用户已配好真实密钥,
  面板/紧凑条/胶囊都在跑真实数据(含状态色与阈值染色)
