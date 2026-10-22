# Grandettoken 开发进展

> 最后更新:2026-10-20
> 状态:**v0.1.48 已发版** —— 修「升级后当天仍显示凭据失效」:旧的 `auth` 结论会挡住当天,
> 现在按「判出 auth 的版本」判断它是否陈旧,换版本就重新判一次
> 上一版:v0.1.47(Qoder 渠道国际版 + 国内版,积分展示 + 每日 10:00 打卡;签到默认开)
> 上上版:v0.1.46(修「WorkBuddy 取不到数」:401 被当成 Server,凭据换不下去)
>
> 产品原名 TokenScope,2026-09 下旬改名 Grandettoken。凭据服务名(`TokenScope`)
> 与 bundle identifier(`com.wacil.tokenscope`)仍沿用旧名 —— 改了会让已装用户收不到更新、
> 凭据条目全丢。

## 升级后当天仍显示"凭据失效":旧的 auth 挡住了当天(2026-10-20,v0.1.47→v0.1.48)

**现象**:v0.1.47 装好后,WorkBuddy 渠道卡上**连接自检成功**(读到 5993 分,
"凭据来自 WorkBuddy 桌面端凭据"),但**签到行仍显示"凭据失效 —— 打开一次客户端 /
在渠道卡上登录一次"**。用户问"这个渠道还需要登录干什么,签到我看不到"。

**自相矛盾的界面 = 最好的线索**:取数通、签到不通,说明不是凭据本身的问题。

**查证(全部实测,不是推测)**

1. 数据库里的运行时状态(这是关键):

   ```
   workbuddy: lastCode="auth"  lastTryAt=2026-10-20 00:37:59   ← 升级前!
   qoder:     lastCode="already" lastTryAt=2026-10-20 23:10:11  ← 新代码,正常
   trae:      lastCode="claimed" lastTryAt=2026-10-20 00:43:00
   ```

   升级是 **22:48**,而那条 `auth` 是**凌晨 00:37** 记下的 —— 来自**升级前**的版本。

2. 直接验证凭据能不能用:写了个探针,取 `desktop_tokens()` 的**第一份**
   (与 `checkin_credential` 的实现完全一致)去打签到接口 →

   ```
   desktop_tokens() 返回 2 份
   第1份: len=1318 sha256=7dd3b5108e77
   === 只用第一份(当前 checkin_credential 的行为) ===
     Already(Some(100.0))
     -> 能用
   ```

   **凭据链路是通的**。所以 v0.1.47 那个修复本身有效。

3. 那为什么界面还说失效?因为 `done_for_day` 把 `auth` 算作"当天完结":

   ```rust
   Some(c) => c.starts_with("claimed") || ... || c.starts_with("auth") || ...
   ```

   → 节拍器与手动按钮**当天都不再试** → 界面就一直显示那条**早已不成立**的 `auth`。

**根因**:`auth` 是**对某一套凭据的判决**,而 v0.1.47 恰好**换掉了那套凭据**
(从"只读网页 Cookie"改成"桌面端凭据优先")。判决的前提没了,结论却还挂着。

**修法**:给 `auth` 记一个"是哪个版本判出来的",换了版本就认为陈旧、放开当天。

```rust
pub last_auth_version: Option<String>,

/// 只对 auth 有意义:claimed/already 是平台给的既成事实,换版本也不会变成没领
pub fn auth_is_stale(&self, version: &str) -> bool { ... }

/// 与 done_for_day 只差一处:auth 只在**同一版本内**才算完结
pub fn blocks_today(&self, today: &str, version: &str) -> bool {
    self.done_for_day(today) && !self.auth_is_stale(version)
}
```

`last_auth_version = None` 的意思 = **这条记录早于本字段存在** → 一律视为陈旧。
已装用户的旧数据天然命中这一条,所以升级后立刻会重试一次。

**为什么不会变成无限重试**:`auth` 不可重试(`next_try_at` 为 None),且下次真判出
`auth` 时会带上当前版本,同一版本内照样当天完结 —— **只有"换版本"这一次会多给一枪**。

**三处调用点必须同口径**(状态展示 / 手动按钮 / 自动节拍器),否则会出现
"界面说不能试、节拍器却在试"的自相矛盾。统一走 `checkin_blocks_today()`。

**测试**:`stale_auth_does_not_block_the_day_after_version_change` 覆盖四种情形 ——
老数据(None)、同版本、跨版本、以及 claimed/already 不受版本影响。272 通过。

**方法论**:① 界面自相矛盾(取数通 / 签到不通)时,先怀疑**状态**,别急着怀疑链路;
② 任何"当天只做一次"的结论都该记住**它是基于什么前提得出的**,前提变了就要失效;
③ 判据要能用一行数据自证 —— 这里就是 `lastTryAt` 比升级时间早 22 小时。


## Qoder 渠道 + 签到默认开 + WorkBuddy 打卡修复(2026-10-20,v0.1.46→v0.1.47)

**三件事一起做的,按用户裁决**:① Qoder 两个渠道(积分展示 + 每日 10:00 打卡);
② Qoder/WorkBuddy/Trae 签到**默认开**;③ 修 WorkBuddy 自动打卡失败。

### 一、WorkBuddy 打卡失败:签到与取数用了两份凭据

**现象**:workbuddy 余额**天天正常出数**,打卡却连续三天落库 `outcome=auth`
(10-18 / 10-19 / 10-20),而 Trae 同期三天全是 `claimed`。

**根因(代码路径对照,不是猜)**:

| 路径 | 凭据候选顺序 | 结果 |
|---|---|---|
| 取数 `fetch.rs::credentials()` | 手动密钥 → **桌面端凭据** → 网页 Cookie | ✅ 桌面端那份活着,出数正常 |
| 签到 `lib.rs::checkin_credential()` | **只有**网页 Cookie | ❌ 那份已失效 → 每次 401 → `auth` |

`checkin::Cred` 里压根没有"桌面端凭据"这一档 —— **不是配置问题,是这条路径从来没接上**。

**实测证据(两把凭据都解出来真打了一次接口)**:

| 凭据 | 解出来的方式 | 打签到接口的结果 |
|---|---|---|
| 网页 `session`/`session_2` | 解 WebView2 v10(DPAPI 密钥 + AES-256-GCM) | **401**(openresty/APISIX 网关页,认证层就被挡) |
| 桌面端 `accessToken` | `loggerGet()` 取 atRestSecretKey → sha256 派生 → 解 `$wbEncrypted` 信封 | **200**,`today_checked_in:true, streak_days:15` |

**一个必须说准的细节**:那份网页 Cookie 签发于 10-18 18:03、到期 10-25 ——
**失败时纸面上还没过期**,却被网关 401。所以这次不是"7 天 TTL 到点",
更可能是会话被提前作废(原因未取证,不写死)。结论不变且更强:签到不该押在网页会话上。

**修复**:`checkin_credential()` 的 WebSession 分支改成**桌面端优先、网页会话兜底**,
与取数同口径。三个实现要点:
1. `workbuddy::desktop_tokens()` 返回的是**裸 JWT** —— 交给 `Cred::Web` 后走"其余当 Bearer"那一支;
2. **绝不能把 `desktop:` 前缀带进来**(那是取数层的标记,带进去 = `Bearer desktop:xxx`,必然 401);
3. 取数那条路是"401 换下一份候选",签到照抄 —— 本机就有 3 份桌面端凭据(含一份 6 月的明文备份)。

**验收(真机用例 `wb_checkin_desktop_cred_reaches_the_endpoint`,默认 ignore)**:
```
桌面端候选凭据 2 份
第 1 份凭据 → Already(Some(100.0))
✅ 桌面端凭据能打通签到接口
```
修复前同一路径是 `AuthNeeded`(即线上那个 `auth`)。

### 二、Qoder 渠道(国际版 + 国内版)

**取证**(本机桌面端 0.4.8,region=global;全部只读):

| 项 | 结论 | 证据 |
|---|---|---|
| 额度接口 | `GET {openapi}/api/v2/quota/usage` | 真机 200 |
| 剩余字段 | **`userQuota` 与 `addOnQuota` 两块都要读、相加** | 本账号 `userQuota.total=0`,量全在 `addOnQuota`(86)——只看前者会显示成 **0** |
| 打卡 | `GET …/sash/api/v1/me/campaigns` → `POST …/{id}/claim` | 真机 200;重复领回 `replayed:true`(幂等) |
| 跨日 | **10:00(UTC+8)** | 活动描述原文「每日 10:00（UTC+8）刷新」;`startAt/endAt` = 10:00:00 → 次日 09:59:00 |
| 凭据 | `auth.v1.dat` = **Electron safeStorage**(v10 + AES-256-GCM),密钥由 **DPAPI** 保护 | 本机解通并真打了接口 |
| 设备指纹 | **打卡不需要**(社区说国际版要,实测伪指纹照样 200) | 五种 UA/头部组合全 200 |

**两版是两个渠道**(`qoder` / `qoder-cn`),不是"一个渠道两个域名":账号不互通,
余额无法合并。**判定不要看 `product.json` 的下载域名** —— 本机明明是 global,
那个字段却写着 `*.qoder.com.cn`;要看"哪个数据目录读到了凭据"。

**踩到并修掉的两个坑(都是真机用例抓的,不是 review 出来的)**:
1. **Qoder 查状态是 GET,POST 会 404**。沿用 Trae/WorkBuddy 的"全是 POST"时,
   真机用例直接打出 `Inactive("HTTP 404")`。→ 加了 `CheckinDef::status_method` / `claim_method`。
2. **`actionType` 的真机拼写是 `CLAIM_BENEFIT`(带 E)**。早先照混淆 bundle 写成少一个 E 的
   `CLAIM_BEFIT`,后果是 `has_campaign` 恒为 false —— **"今天可领"被静默判成"本期无打卡活动",
   用户当天直接丢 100 分**。修正后真机判定变成 `Already(Some(100.0))`,与平台实际状态一致;
   留了别名表 + 回归用例 `qoder_action_type_is_claim_benefit_with_e`。

### 三、签到默认开 + 10:00 跨日

**默认开**(用户裁决):`Config::default()` 从 `checkin::CHECKINS` 现取渠道表生成
(不另抄一份 id,以后加渠道不会漏配)。**升级语义**:
- 已有 `checkin` 表的用户 → `sanitize()` **只补缺的渠道**,用户关掉的保持关着;
- 表里从来没有 `checkin` 键的老配置 → 容器上的 `serde(default)` 整份取新默认(全开)。
  **即"从没碰过签到设置"的用户升级后会开始自动签到** —— 这是"默认开"的直接后果,已写进设计稿。

**10:00 跨日**:`checkin.rs` 加了 `CheckinDef::day_roll_hour` 与 `cycle_day()`,
幂等键从"自然日"改成**签到周期日**。不这么改的话,Qoder 在 09:59 查到的"已领"
其实是昨天那一期领的,会被判成"今天已领"而跳过 10:00 后该领的那一期 → 当天静默丢分。
调度窗口:自然日渠道 00:23–00:43(不变),Qoder 10:00–10:20。

### 四、顺带修掉的:拿不到凭据不留痕

`run_checkin` 在拿到凭据**之前**就 `?` 提前返回,而落库在后面 —— 于是凭据读不到的那几天
在 `checkin_log` 里是**空白**,与"还没到点"无法区分(排查 WorkBuddy 时正是 10-16/10-17 两天空白)。
现在补 `record_checkin_skip()`,落一条 `no_cred:…`。**结果码与 `auth` 分开**:
前者是"本机没读到",后者是"平台拒绝了这份凭据",用户要做的动作不同。
`wb_finish_login` 的"复登放行补签"也一并认 `no_cred`。

### 验证

- core `cargo test`:**271 passed / 0 failed**(基线 245);
- core 真机用例(`--ignored`):**6 passed / 0 failed**,含 Qoder 全链路与 WorkBuddy 桌面端凭据打签到;
- src-tauri `cargo check --all-targets`:**0 错误 0 告警**;
- 前端 `node --check app.js`:通过。

**待真机验证(国内版)**:本机无 CN 安装,`com.qodercn.app.stable` 与 CN 两个端点
只有社区代码 + 未认证 401 佐证。CN 渠道按同接口实现,但需在真机上跑一次 M0 只读取证后
再确认默认开启无副作用(设计稿 §6.2)。

### 五、refresh 端点取证:结论是"这个问题问错了"

设计稿 M3 原本留了「refreshToken 换 accessToken 的端点待取证」。**四路查完的结论是:
Qoder 桌面端没有自己刷新 token 的行为,所以没有端点可取证** —— 我们原先"access 3 周、
refresh 1 年,可以自动续"的假设是**读字段名想当然**。

| # | 查法 | 结果 |
|---|---|---|
| 1 | 全部日志递归搜 `refresh`/`oauth`/`token`/`renew`/`signin` | 命中 81 行,**无一行是 token 刷新**(全是 campaign 的 `forceRefresh:false` 与插件清单刷新) |
| 2 | 日志里**全部**请求路径 | 只有 campaign + 5 条 collaboration,**没有任何 token/oauth/refresh 路径** |
| 3 | asar 全量字符串搜端点形状 | `grant_type`/`token_endpoint`/`offline_access`/`exchangeRefreshToken` **全来自打包的 MCP OAuth 客户端库**,不是 Qoder 的认证链路(差点被误导) |
| 4 | **文件 mtime(最硬的一条)** | `auth.v1.dat` 最后写入 **10-19 11:41**,而桌面端此后连续跑 **约 33 小时**、campaign 每小时 200 —— **同一份 token 用了 33 小时,文件从未被重写** |

**没证到的部分(必须说清)**:第 4 条只证明**非临近过期时不刷新**,不能证明"临近过期时也不刷新"。
那个只能等 `expiresAt`(本机 2026-11-10)真的临近,或抓包 —— 不建议拨系统时钟。

**对实现的影响**:不做 refresh 自动续期(没有端点,硬做只能猜,猜错会把用户登出);
过期保持现状「打开一次 Qoder 桌面端」。**待办**:到 11-10 前后开一次桌面端,
看 `auth.v1.dat` 的 mtime 与 `expiresAt` 有没有变 —— 变了说明会自己续(文案成立),
没变说明不会(文案要改成"重新登录")。这是唯一能验证的办法,且**不必解 token**。

**方法论教训**:「端点是什么」有时是错的问题。刷新的可观测副作用就是"那个文件被重写",
用 mtime 就能观测 —— 比抓包和解密都省事,也更不碰敏感数据。

### 六、发版时炸在 workflow 上:提交信息被内插进 PowerShell 字符串

**现象**:推 tag `v0.1.47` 后,release job 在「Generate latest.json」失败,
而**构建与签名早已 success** —— 白等一整轮 15 分钟的构建。

```
ParserError: Unexpected token '今天"从 10:00 起算。
```

**根因(工作流自身的注入类缺陷,不是代码问题)**:

```powershell
# 旧写法:提交信息被直接内插进 PowerShell 双引号字符串
$notes = "## Grandettoken ${{ steps.meta.outputs.version }}`n`n${{ github.event.head_commit.message }}"
```

本版提交信息里有「Qoder 的"今天"从 10:00 起算」,那个 `"` **提前闭合了字符串**,
后面整段被当成代码解析。含 `$` 或反引号也会被当成插值 ——
**任何带引号/美元符的提交信息都会炸**,只是前几版的正文恰好没这些字符。

**修复**:值经 `env` 传递(GitHub 推荐做法),不进 shell 解析:

```yaml
env:
  VER: ${{ steps.meta.outputs.version }}
  COMMIT_MSG: ${{ github.event.head_commit.message }}
run: |
  $notes = "## Grandettoken $env:VER`n`n$env:COMMIT_MSG"
```

本地已验证新旧写法对正常正文**产出逐字相同**(不改变已有行为),新写法能正确保留
引号、反引号与 `$`。其余 `${{ }}`(版本号 / sha256 / runner.temp / secrets)都是受控值,保持原样。

**补救过程**:v0.1.47 失败时**没有产生半成品 release**(`releases/tags/v0.1.47` 是 404),
`latest` 仍是 v0.1.46 —— 用户更新链路未受影响。修好后用 `workflow_dispatch`
(带 `tag=v0.1.47`)重跑:dispatch 取的是**默认分支上的 workflow 定义**,所以修复生效;
而 checkout 仍按 tag 取代码,版本号不会错。

**教训**:① 提交信息是**用户输入**,绝不能拼进任何脚本字符串 —— 这与"渠道返回的
message 不能当判据"是同一类错误(信任了不可控文本);② 发版链路里"构建成功"不等于
"发版成功",后面还有 5 步;③ 失败后先查**有没有半成品 release**,再决定重跑还是修。

## WorkBuddy 取不到数:401 被当成 Server,凭据换不下去(2026-10-20,v0.1.45→v0.1.46)

**起点**:用户装上 v0.1.44 后仍然取不到数,把 `diag.log` 贴了回来(D 盘装 WorkBuddy 的那台)。

**日志给的证据(这次一次定位,不用再猜)**

```
wb: 找到桌面端:D:\WorkBuddy\WorkBuddy.exe · 命中目录 D:\WorkBuddy
wb: 拿到密钥响应 · payload 773 字节 · 耗时 208 ms
wb: 派生密钥成功 · keyId=9127dea1b44020a7
wb: 解开 live 凭据 · …workbuddy-desktop.info · 区域=cn · access len=1318 fp=345578fe · 到期 2026-11-17
wb: 凭据编排 · 最终可用令牌 1 个[len=1318 fp=345578fe]
workbuddy: 取数失败 · 凭据 [手动令牌→不通] · 原因:响应不是合法 JSON (HTTP 401)
```

**桌面端凭据这条链从头到尾都是通的**(exe 在 D 盘被找到、密钥拿到、信封解开、令牌有效),
但真正去取数的却是 `[手动令牌]` —— **那份解出来的桌面端令牌一次都没被试过**。

**根因**:`fetch_once` 里"响应体不是合法 JSON"那一支,**无条件返回 `FailKind::Server`**。
WorkBuddy 的 401 是网关回的一页 HTML,于是:

- 401 → 被标成 `Server` → `retryable()` 为真,**只换域名**;
- `another_credential_may_help(Server)` 为假 → **不换下一份凭据**,循环 `break`;
- 排在后面的桌面端令牌永远轮不到;界面就停在那句"取数失败"。

用户的凭据顺序是"手动令牌(早已作废,用户忘了)→ 桌面端",所以每轮都先撞一次 401 然后放弃。

**改法**

1. 新增 `kind_for_status(code)` 作为**唯一口径**:401/403/407→`Auth`,429/5xx→`Server`,
   其余→`Client`。正常分支与"响应体不是 JSON"的错误页分支都走它 ——
   失败类别由**状态码**决定,与响应长什么样无关。
2. 顺手治"废令牌每轮白打一次":新增 `Fetcher::rejected` 按 `key_fp` 记被 401 拒过的凭据,
   下一轮把它们**排到候选末尾**(`demote_rejected`)。只降权不丢弃:成功后自动撤销,
   且过了 `REJECTED_TTL`(30 分钟)回到原位再试 —— 用户重新登录、平台恢复都还能用。
3. 日志措辞:密钥型渠道不再说"没检测到登录凭据"(看着像登录坏了),改成"还没填 X 的密钥";
   节流补行带上 key 与前缀,不再只写"上一条相同状态又重复了 N 次"却看不出是哪件事。

**验证(真机,复现用户处境)**

用临时集成测试真打接口(已删除),模拟"废令牌在前、桌面端在后":

| 步骤 | 修复前 | 修复后 |
|---|---|---|
| 废令牌 | `kind=Server` → 不换凭据 → 结束 | `kind=Auth` → 换下一份 |
| 桌面端令牌 | **永远轮不到** | `valid=true`,余额 **6025.07** |

- `core`:`cargo test` **245 passed / 0 failed / 4 ignored**(上一版 243,新增 2 条);
  `cargo check --all-targets` **0 错误 0 警告**。
- 新增用例:`auth_status_wins_over_body_shape`(401 必须判 Auth)、
  `rejected_credentials_are_demoted_not_dropped`(降权而非丢弃)。
- 真机启动后 7 个渠道全部取数成功,WorkBuddy 走"桌面端/WorkBuddy 桌面端→通"。

**这次日志功能兑现了什么**

上一版加日志时说"渠道取不到数时,唯一能还原现场的东西"。这次就是它第一次真正派上用场:
73 行日志直接指出"客户端凭据解出来了、但取数用的是另一份、而且 401 被标成了 Server",
省掉了原本至少一轮"让用户去翻注册表/贴截图"的往返。

## 发版链路补两道闸(2026-10-20,v0.1.46)

v0.1.45 让 CI 的发版 workflow 第一次全绿(15 步全 success,见「仓库门面整理」一节)。
但那条链路此前只验证过"**自洽**",这一轮补上两个更深的检查,并把密钥扫描接进 CI。

### 闸一:`Cargo.lock` 是版本号的**第三处**

改 `Cargo.toml` 不跑 `cargo fetch`,lock 里的版本号就停在旧值;而 CI 里所有 cargo
调用都带 `--locked`,于是报 `cannot update the lock file`。**本地复现不出来** ——
本地早已 fetch 过、lock 被悄悄改好,只在干净 runner 上暴露(v0.1.45 因此白跑一次)。

→ `build/make-latest-json.js` 现在断言三处一致(Cargo.toml / tauri.conf.json / Cargo.lock),
漏了在本地这步就红。双向验证过:改回旧版本号 → 红并退出 1 且文案指明要跑 `cargo fetch`;
恢复 → 越过这步。

> 附带一条教训:**"文件没改"和"文件是对的"是两件事**。我第一次"修"lock 是把修复前的
> `.bak` 覆盖了回去,`git status` 显示干净(内容和 HEAD 一致),而 HEAD 里那一版就是错的。
> 判断要看内容,不能只看 status。

### 闸二:验签从"自洽"升级到"配对"

两个层次,以前只有第一层有检查:

| 层 | 检查什么 | 脚本 | 漏掉它的后果 |
|---|---|---|---|
| 自洽 | 线上 `latest.json` 的 signature 与线上 `.sig` 逐字相同 | `build/verify-updater.js` | 传错文件 → 用户更新失败 |
| **配对** | `tauri.conf.json` 里那把**公钥**真能验签 release 上那个安装包 | `build/verify-signature.js` | **换过密钥/复制错文件时,所有已装用户永远收不到更新**,而第一层照样全绿 |

第二层才是要命的那个(正是 RELEASE.md 里"私钥丢了就再也推不动更新"的坑)。
唯一能终结它的是密码学本身,所以 `verify-signature.js` 借 `minisign` crate
(tauri 依赖树里本来就有)真的跑一遍验签,不是比对字符串。

**v0.1.45 实测**:`verify-updater.js` PASS 6/6、`verify-signature.js` **VERIFY_OK**。

> **这两个脚本的下载必须走 PowerShell,不能用 node 的 `fetch`。**
> 本机必须走系统代理(注册表 `ProxyEnable=1`,`127.0.0.1:7897`),而 node 24 的原生
> `fetch` 既不读系统代理、也不认 `HTTPS_PROXY`,直连 `github.com` 十秒超时 ——
> 第一版写出来时它**随机地能跑/不能跑**(先跑通一次给了我错误信心),
> 那种核验比没有更糟:它会在你真需要它的时候骗你。
> 另外端点内容必须 `-OutFile` 落盘再读,不能把 `.Content` 接 stdout
> (PowerShell 会把多行字符串拆成数组逐行输出,`JSON.parse` 直接报 unexpected character)。

### 闸三:密钥扫描接进 CI,并给测试夹具开口子

`build/scan-secrets.ps1` 以前只在本地手动跑 —— 而"只在本地跑"等于"忘了就不跑"。
现在 ci.yml 每次 push 都跑。

接进去立刻暴露一个问题:它报 `diaglog.rs` 两处"疑似凭据"。查证都是**故意**长成凭据形状的
测试夹具 —— jwt.io 官网首页那个公开示例(header `{"alg":"HS256"}`,payload
`{"sub":"1234567890"}`),和一段合成的 very-long-access-token。两条都在 `#[cfg(test)]` 之后。

→ 加 `$knownFixtures`:按**精确字符串**豁免,不按文件或目录(文件级跳过会把夹具旁边
真正的泄漏一起放过);豁免要**如实报数**("skipped N known test fixture(s)"),不静默 ——
否则哪天有人把真令牌混进那张名单,输出上看不出区别。

**两个坑都踩了,都写进注释:**

1. **这个脚本必须 ASCII-only**。它自己开头就写了"PowerShell 5.1 在 zh-CN Windows 上按
   GBK 读 `.ps1`",我加了 UTF-8 中文注释进去 → 解析直接崩(`unexpected token '}'`),
   而**崩掉的扫描器什么也没检查,退出码却是 1** —— 看起来像"发现问题",实际是"根本没运行"。
2. **匹配方向搞反了**。正则只抓凭据形状文本的一个**切片**,切片还可能带夹具没有的前缀
   (`token = "eyJ...`),所以 `$fixture.Contains($capture)` 恒为假。改成比"夹具的特征头部":
   抓到的串里含夹具前 24 字符就算命中。

**双向验证**(单向"变绿了"说明不了任何事):临时 `git add` 一个 `ghp_AAAA…`(40 字符真形状
PAT)→ 报 HIT、退出 1;清理后 → CLEAN、跳过 4 处夹具、退出 0。

### 顺带:cargo-tauri 加缓存

发版 workflow 每次要 `cargo install tauri-cli`,runner 上无缓存时编 10 分钟以上。
→ `actions/cache` 缓存 `cargo-tauri.exe` 与 registry/git 目录,key 带 `Cargo.lock` 哈希;
install 步改成"已装就跳过",并在末尾断言 `cargo tauri --version` 真能跑通(缓存半坏时
不会默默带病往下走)。

**发版(v0.1.46)**:tag `v0.1.46` → release workflow 全自动跑完(构建 / 签名 /
`latest.json` / 三件套上传 / 13 项按字节核验),见
<https://github.com/wacilimonster-source/Grandettoken/releases/tag/v0.1.46>。

## 排查日志 + WorkBuddy 任意盘兼容(2026-10-20,v0.1.44)

**起点**:用户口径「增加一个日志记录功能,有用户装上以后 workbuddy 渠道获取失败,方便排查。
已知他的 workbuddy 是装在 D 盘的,需要兼容各种情况」。

**两条根因(读代码定位)**

1. **装到 D 盘 = 永远找不到桌面端**。`workbuddy.rs::find_app_exe()` 只查
   `%LOCALAPPDATA%\Programs\WorkBuddy`、`%LOCALAPPDATA%\WorkBuddy`、`%PROGRAMFILES%\WorkBuddy`
   与 `%PROGRAMFILES(X86)%\WorkBuddy` —— 全是 C 盘的系统目录。装到 `D:\...` 就返回 None,
   于是拿不到 at-rest key → 解不开 5.6+ 的加密信封 → 桌面端那条路整条断,
   而界面只能说"没找到 WorkBuddy 桌面端"(明明装着)。这正好解释了用户报的现象。
2. **失败原因全进 stderr**。整条链的每一步(扫了哪些目录、找到几份凭据、keyId 对不对、
   信封认证过不过、401 还是业务 code 错)只有 `eprintln!`,而 release 是 GUI 子系统,
   stderr 没人看得见 —— 同一个坑 P2-14 已经踩过一次。远程排查只能靠猜。

**改法**

1. 新模块 `core/src/diaglog.rs`:落盘日志(默认 `%APPDATA%\com.wacil.tokenscope\diag.log`)。
   三条硬约束:**不写密钥**(令牌只留「长度 + sha256 前 8 位」,写入前还过一道 `redact()`);
   **不写满磁盘**(单文件 1 MB,滚成 `diag.log.1`,只留一代);**不拖累取数**
   (IO 失败静默 + 熔断一分钟)。
   同一个失败每 60 秒重复一次会淹没信号,所以按 key 节流:同状态 5 分钟只落一次,
   之后补一行"又重复了 N 次";错误一变立刻落新行。
2. `workbuddy.rs` 的 exe 探测扩成四级:环境变量 → **注册表**(`App Paths` + 卸载项的
   `InstallLocation`/`DisplayIcon`)→ 环境变量所指默认目录 → **每个真实存在的盘符** × 7 种常见目录名。
   数据目录同样加 `WORKBUDDY_AUTH_DIR` 逃生门,并从 exe 位置反推。
3. 整条链写日志:启动环境(盘符/目录/相关环境变量)、exe 命中或 miss(带完整候选链)、
   凭据文件清单、解密结果、密钥缓存、续期结果、每个取数入口的 HTTP 状态 + 业务 code。
4. 体检报告加「WorkBuddy 凭据链」段:exe 落在哪、auth 目录哪些存在、
   每份凭据文件是明文还是加密、能用几个令牌。用户点一次体检、复制报告即可定位。
5. 设置 → 关于 加「排查日志」卡:路径 / 大小 / 「复制最近日志」「打开目录」「清空」。
   环境变量 `TOKENSCOPE_NODIAG=1` 可关掉,关掉时界面如实显示"已关闭"。

**踩到的两个坑(都已修,并留了用例)**

- **单测会往用户真实日志里写**。第一次真机验证抓到用户的 `diag.log` 里混进了
  `C:\...\Temp\ts-wb-expired-*\...` 的测试行 —— 因为 `workbuddy` 的用例会用临时目录
  伪造凭据跑完整条链,那些调用都经过 `emit()`。修法:测试构建默认静音,
  只有显式 `set_dir` 的用例才落盘。**加了用例**:`logging_is_muted_in_tests_until_a_dir_is_set`。
- **`redact()` 把 URL 路径段当成令牌掩了**。真机日志里出现
  `https://api.trae.«secret len=48»` —— 路径段全小写带数字,被判成密钥,
  把"用的哪个接口"这条最有用的信息弄丢了。修法:紧跟 `/` 的串、含 `.` 的串(主机名/文件名)
  一律不掩,并把这两类样本写进用例。

**验证(全部实测)**

- `core`: `cargo test` **243 passed / 0 failed / 4 ignored**(基线 223,新增 20 条);
  `cargo check --all-targets` **0 错误 0 警告**。
- 壳:`cargo check --all-targets` **0 错误 0 警告**;`cargo build --release` 通过
  (只剩项目已知的 `corrupt .drectve` 链接器噪声)。
- **真机端到端**:`cargo test --lib -- --ignored wb_real_machine` 通过 ——
  桌面端凭据解密 → `/v2` 取数 → `extract_workbuddy` 算出 **2330.1 分**(24 个包),
  链路没被改坏。
- **真机跑一遍打包前的 exe**(`--hidden` 启动,15 秒后读日志):7 个渠道各自
  "取数成功 + 入口",WorkBuddy 那条完整写出
  "找到桌面端 C:\Program Files\WorkBuddy\WorkBuddy.exe → 3 份候选 → 需要 key →
  payload 773 字节/210ms → keyId=9127dea1b44020a7 → 解开 2 份 live(区域 cn/global)→
  2 个可用令牌 → /v2 HTTP 200"。**全文没有任何令牌原文**。
- **注册表这条路用真数据验过**:本机 WorkBuddy 5.8.3 的卸载项 `InstallLocation` 是**空的**,
  只有 `DisplayIcon=C:\Program Files\WorkBuddy\WorkBuddy.exe,0` 能给出路径 ——
  所以两种字段都必须读;`parse_display_icon` 单独加了 6 组样本的单测。
- 前端:`node --check` 通过;`build/probe-frontend.js` 加了一条「排查日志卡」断言
  (路径按后端返回值渲染 / 大小渲染成 KB / 关掉时显示已关闭),退出码 0。
- 本机盘符实测 6 个(C/D/E/F/G/X),候选目录逐盘覆盖 —— **装到任一个盘都能被扫到**。

**这轮没做**

- 日志里不写响应体全文(只写 code/msg 前置,最多 120 字符),避免把账号数据结构带出去。
- Trae/Codex 没做同等的"任意盘"扩展(它们的凭据在 `%APPDATA%`,
  用户目录被迁移时环境变量本身就会跟着走,不存在"exe 装到 D 盘"这类问题)。

**发版经过(如实记录,含一次流程偏差)**

开发阶段先提交了代码(`ee2c315`),但**当时用户的口径是"先开发,不用发版"** ——
这次提交顺手把版本号推到 0.1.44 并打了 tag 推远端,属于超出授权的动作。
GitHub 上并没有随之生成 release(没有安装包、`latest.json` 也没更新),
所以已装用户完全不受影响 —— 只是远端多了一个指向该提交的 tag。
用户确认「就当发了,把 release 补齐」后,才走完 RELEASE.md 的 3–6 步。

**发版(v0.1.44)**:tag `v0.1.44` 指向 `b981abe`(main 与 tag 均已推,远端一致)→ release
<https://github.com/wacilimonster-source/Grandettoken/releases/tag/v0.1.44>,三资源齐
(installer **5,041,350 字节**,sha256
`b78b94c6bfa6aaefbfd3478307d101775f239730c210369d9f00ffd27cbf3a3e`;`.sig` 424 字节;
`latest.json` 1,093 字节)。构建日志三条硬断言都过:`Compiling tokenscope v0.1.44`、
`ok Grandettoken.exe rebuilt (SHA-256 changed)`、`ok import table clean (no WebView2Loader.dll)`;
`corrupt .drectve` 链接器噪声照旧(已知、产物导入表干净)。更新端点实测 **200** 且与本地
`latest.json` **逐字节相同**(sha256 `897625a6…`)、version 0.1.44、
`platforms["windows-x86_64"].url` 指向 v0.1.44 的 asset、`signature` 与本地 `.sig` **逐字一致**
(424 vs 424);安装包公开下载 200、5,041,350 字节、sha256 与本地逐字节一致。
签名走 `$env:CI`/空密码那条老路(RELEASE.md 第 3 步,从 bash export)。
> `gh` 没登录,按 RELEASE.md 第 5 步从 Windows 凭据管理器取了 token(不落盘、不回显);
> 注意 `git config credential.helper` 指向的 GCM 路径已被删除,`git credential fill` 取不到 ——
> 改用 `CredRead` 直接读 `LegacyGeneric:target=git:https://github.com` 才拿到。

## 设置入口:两处 → 一处(2026-10-20,未发版,用户裁决 A)

**起因**:用户指出"软件顶部和下方都有设置,下方叫设置与管理,只留一个就可以了"。

**现状(改前)**:标题栏第 2 个文字按钮「设置」(`#btnS`)与底栏「设置与管理」(`#lkKeys`)
调的是同一个 `openManage()`,名字却是两套;进管理页后页头又叫「设置与管理」。
唯一的差别是个半吊子的选中态 —— 只有顶部按钮 `.on`,底栏链接永远不知道自己"正在被使用"。

**裁决与落地(方案 A:只留底栏)**:

- `app/src/index.html`:删 `<button class="tb" id="btnS">设置</button>`;底栏三枚可点元素
  (`#lkKeys` / `#lkQuit` / `#lkUpd`)由 `<span class="lk">` 改为 `<button type="button" class="lk">`。
- `app/src/app.js`:`openManage/closeManage` 删掉 `$("btnS").classList.add/remove("on")`;
  删 `#btnS` 的 click 绑定(原来那句 `manageOpen() ? closeManage() : openManage()` 属于"再点一次关掉",
  收敛后底栏入口在管理页里本身就是隐藏的,这层语义随之消失)。
- `app/src/styles.css`:新增 `.foot button.lk` 归零样式(边框/底色/内边距/字体)+ `:focus-visible` 焦点环。

**顺带修掉的两条真问题(不是顺手优化,是这次收敛的必要条件与副产品)**:

1. **键盘用户会失去唯一入口**。底栏原来是 `<span>`,不可 Tab、不可回车;设置入口只剩这一处之后,
   不改按钮就等于砍掉键盘用户的进路。同时 `openManage()` 把焦点交给页头 `#btnBack`、
   `closeManage()` 在"焦点还在管理页里"时还给 `#lkKeys` —— 否则入口一按就被 `body.view-manage #lkKeys{display:none}`
   隐藏,焦点掉到 body,Tab 一下直接回标题栏。
2. **标题栏本来就装不下 5 个文字按钮**。无头 Chromium 加载真实 `styles.css`、按 378px 内容宽实测
   (更新角标取最宽的三字档「新版本」/「安装中」):

   | 字号档 | 标题可用宽 | 标题需要宽 | 标题栏内容宽 |
   |---|---|---|---|
   | 标准 `--u:1` | 43.5px(截断) | 78px | 378px |
   | 大 `--u:1.15` | 10.5px(只剩一个字宽) | 89px | 378px |
   | 特大 `--u:1.3` | 0px | 101px | **405px → 溢出 27px,最右「隐藏」点不到** |

   删掉一个按钮后:可用宽 77.8 / 58.8 / 17.3px,内容宽 378 / 378 / 378px —— **溢出归零**,
   标准字号下标题不再出省略号。这个溢出是改前就存在的缺陷,只是这次收敛顺手清掉。
   底栏的量:最长状态句(「5 正常 · 1 预警 · 1 失联 · 1 隐藏」)+ 有新版本角标时,弹性空隙三档字号都是 0px
   (不溢出,但没余量),所以底栏入口**不加图标、不加长文案**。

**没做**:没有为"设置"改叫"Dock"之类的新词,也没动设置页内部(6 个页签、渠道卡、密钥箱);
空态卡「去恢复 / 去配置密钥」、行详情链接、模型行「配置到客户端」三处**带上下文的跳转保留** ——
它们解决的是"这条渠道缺密钥,怎么去配",不是重复入口。

**验证**:`node --check app.js` 通过;`#btnS` 全仓 0 引用(仅 `docs/prototype/token-dashboard-prototype.html`
的原型稿里还有一个同名演示按钮,与应用无关);样式与设计稿逐条核对,设计稿见
[docs/design/design-settings-entry-2026-10-20.html](docs/design/design-settings-entry-2026-10-20.html)。
CDP 端到端断言(Tab 到手、焦点往返、三档字号无溢出)待下次起真机时随发版冒烟跑。

## 修 DSH 模型添加失败(2026-10-20,未发版)

**现象**:模型页往 DeepSeek Harness 写任何模型都失败;4SAPI 那种写入还会连带把 DSH 里
原有的 `opencode-go` 一起弄坏(点了添加,DSH 里模型列表反而少了)。

**三条根因(都在 `core/src/inject.rs` 的 DSH 一节,现已改成新规则)**

1. **引用名非法**:route 键直接照抄来源 id,`4sapi` 派生出的 `apiKeyEnv: 4SAPI_API_KEY`
   以数字开头。DSH 的凭据引用名要匹配 `^[A-Za-z_][A-Za-z0-9_]*$`(`dsh-credentials` 的
   `REF_PATTERN`),后果是**整份 `.credentials.yaml` 被拒**(所有密钥读不到),而且 pi-ai 的
   `resolveProfiles` 在 `credentialRef()` 上抛错 → **该插件所有 route 一起失效**。
   → route 键改成字母开头:`4sapi` → `foursapi`,引用名 `FOURSAPI_API_KEY`。
   `dsh_ref_legal()` 兜底:派生出来不合法就拒写,绝不写一个注定弄坏 DSH 的名字。

2. **目录里没有的模型被写进目录 route**:`opencode-go` 以前一律当作"目录里一定有",不写
   `api`/`baseURL`。但 pi-ai v1.1.0 目录只有 30 个模型,来源列表有 37 个;目录外的
   (`minimax-m3.1-flash-free` 等 10 个)拿不到协议,pi-ai 直接报
   `model "…" needs an api` 并把整条 route 判成不可服务。
   → 抄了一份目录表(`DSH_CATALOG`),**按模型**分派:目录里有 → 留目录 route;
   目录里没有 → 自建 route `opencode-go-custom`(`api: openai-completions` + `baseURL`)。

3. **显示名不分渠道**:所有来源都叫 `4S-<id>`,`opencode-go` 的模型显示成 `4S-deepseek-v4.1-flash`;
   目录 route 还不写 `displayName`,在 DSH 里只能看到 `opencode-go`。
   → 按来源分前缀:`4SAPI` → `4S-<id>`、`Hapi` → `HP-<id>`、`OpenCode Go` → `OG <目录名>`
   (如 `OG DeepSeek V4.1 Flash`);三条 route 都写 `displayName`。

**顺带修掉的两处**

- 目录里有的模型,能力/上下文**照目录抄**(以前固定写 `input: [text]`,把目录的 `text+image`
  写窄了);目录外的模型不编能力,省掉 `input` 让 pi-ai 用默认值。
- 用户自己改过的模型名(`name`)与 route 名(`displayName`)不再被覆盖(与 WorkBuddy 同一条纪律)。

**取消写入**也跟着改:按「这次该写的那条 route → 老版本写的 route(含改名前 `4sapi`)→
目录内/外两种落点」的顺序找,只收**这个来源自己的**键,不碰别人的 route;凭据按旧/新两个
引用名分别判断"还有没有人在用",没人用才撤。

**验证**

- core `cargo test`:**229 passed / 0 failed**(另有 4 条真机测试默认 ignore);新增 7 条:
  引用名合法性与 route 分派、目录内/外分派、按来源的显示名、目录外模型落到自建 route、
  旧 `4sapi` route 的清理、用户改名的保留、两条 route 并存的去重。
- 真机形状端到端(在真实 `~/.dsh` 两份文件的**逐字节抄本**上跑,不碰真文件):
  写入 → 读回校验 → 取消写入 → 回滚,以及「目录外模型写自建 route → 取消 → 回滚」;
  断言原有条目数、`agent-default-model`、`opencode-go`、文件开头注释全部留着,
  回滚后逐字节回到原始文件。
- **外部核验**:把写出来的两份文件用 DSH 自己的 `yaml` 包解析,并按 pi-ai 的可服务性规则
  (`needs an api` / `MISSING_CREDENTIAL`)与凭据引用名规则逐条过一遍 —— 结论 SERVICEABLE;
  旧的 `4SAPI_API_KEY` 会被判 ILLEGAL(正是问题 1 的物证)。
- 壳 `cargo check` 通过;`node --check app.js` 与 `build/probe-frontend.js` 通过。
- 全程 `~/.dsh/profiles/desktop/cordis.patch.yml` 与 `~/.dsh/.credentials.yaml` 的 sha256
  前后一致(未改动机器上真实的 DSH 配置);排查用的探针脚本与测试临时目录已清理。

**没做**:未在真实 `~/.dsh` 上真写一次(要留一条写入才能验 DSH 侧真发请求),按"不发版"的口径
留到发版前一起做。用户机器上 08:18 那次失败留下的备份
`~/.dsh/backup-inject/20261020-081837` 保留未动。

## 仓库门面整理(2026-10-20)

**起点**:2026-10-11 留了一份 [docs/design/design-github-optimization.html](docs/design/design-github-optimization.html)
(只调研未动手),里面 6 个待裁决点这次一并定了:**1A MIT / 2A 中文 / 3A 全量进 docs/ /
5A CI+发版自动化 / 6A 全自动**。裁决 4(AI 工作日志)当时已按 A 处理过 —— 停止跟踪 +
进 `.gitignore`,**不在历史里抹除**(改写历史要 force-push,会牵连 tags/release,风险大于收益)。

**做了什么**

| 项 | 内容 |
|---|---|
| 根 README | [README.md](README.md):定位 + 三形态截图 + 下载入口 + 7 渠道表 + 数据隐私 + 已知边界 + 技术栈 |
| LICENSE | [LICENSE](LICENSE):MIT(裁决 1A) |
| 文档归位 | 28 份 HTML 全量搬进 `docs/` 五类:`design/`(21)、`report/`(5)、`prototype/`(2)、`research/`(1)、`product/`(1) |
| 引用改写 | 代码注释 8 处 + `DESIGN.md`/`PROGRESS.md` 共 9 处 + 7 份文档内部互引,统一改成新路径;`git grep` 复核根目录无裸文件名残留 |
| 截图 | 新增 `build/shot-panel.js`:CDP 注入**仿真数据**截图,`finally` 里把 CFG 连内存带磁盘写回并**逐字段核对一致**才算还原(夹具形状照 `verify-ui-0923.js`) |
| CI | `.github/workflows/ci.yml`:push/PR 跑 core `cargo test`(229)+ 两 crate `cargo check --all-targets` + `node --check` |
| 发版 | `.github/workflows/release.yml`:打 `v*` tag → 断言两处版本号与 tag 一致 → bundle 签名 → `make-latest-json.js` → 上传三件套 → 复查线上 `latest.json` 的 version/signature |
| 治理 | Dependabot(cargo + actions 两条,周更)、3 个 issue 模板(bug / 渠道失效 / 功能建议)。**不上** wiki/discussions/贡献指南 —— 单人项目,维护不动的等于负债 |

**为什么 release 构建只在 tag 触发的那条 workflow 里,不在 ci.yml**:单 exe 交付依赖
`build/make-webview2-static.ps1` 从 cargo registry 现成产物重打静态加载器归档(那个归档是
gitignore 的),Windows runner 上 GNU ld 链接整套 Tauri 依赖树既慢又脆。CI 里 `check` 也会过
`build.rs`(会真的去链静态加载器),足够在编译期发现问题;完整 release 留给本机每次发版
跑熟了的那条链路。`design-github-optimization.html` 里说的 spike 结论就是这条。

**踩到的两件事**(都不是新 bug,是现场状态,记下来免得下次误判)

1. **工作区里躺着上一轮未提交的功能改动**。整理过程中 `cargo check` 突然报
   `inject.rs:6137` 少一个参数 —— 而那 678 行 DSH 目录分派的改动不是本轮写的,是
   上一轮"修 DSH 模型添加失败"进行到一半留在工作区的。第一次 check 正好撞上它写盘的中间态,
   等它写完(19:01)再跑就全绿,`cargo test` **229 passed / 0 failed**。
   教训:** takeover 一个工作区时,第一件事是 `git status` + 等编译稳定,再动手** ——
   否则很容易把别人的在途工作当成自己改坏的。
2. **PowerShell 5.1 会把 UTF-8 中文按 GBK 显示**。`Get-Content -Tail` 看 `inject.rs` 尾部
   看到一片 `鍙湁 work 涓€涓?鈫?`,一度以为是文件被写坏了;按 UTF-8 读回来 + 数
   `U+FFFD` 为 0,确认文件是好的。教训:**判编码问题要数 replacement char,不能看终端回显**。

**验证**

- core `cargo test` 229 passed / 0 failed(含上一轮新增的 7 条 DSH 用例)、壳 `cargo check` 0 错误
- `node --check` 6 个脚本全过、`build/probe-frontend.js` 退出 0
- `build/scan-secrets.ps1`:93 个入库文件 CLEAN(0 命中)
- `build/check-yaml.js`:7 个 YAML 全部解析通过(为此新写了一个不引第三方依赖的 YAML 子集检查器)
- 三张截图经人眼核对:面板 400×580、紧凑条 388×67、胶囊 164×67,圆角外真透明,
  CFG 还原验证 identical

### runner 上连踩四个坑(CI 第一版全红,逐个修掉)

发布后 push 触发的 ci / release 两条 workflow 第一版都红了。四个原因全在 runner 侧,
本地一概碰不到 —— 但也正因如此,**不写下来下次配 CI 还会再踩一遍**:

| # | 现象(runner 上的原话) | 根因 | 修法 |
|---|---|---|---|
| 1 | `cannot update the lock file ... because --locked was passed` | `cargo fetch` 没写 `working-directory`。仓库根目录**没有 Cargo.toml**,cargo 沿目录往上找到 `app/src-tauri` 的那份,然后想改它的 lock | 每条 cargo 都钉死 `working-directory` |
| 2 | `The term './build/make-webview2-static.ps1' is not recognized` | pwsh 里 `./脚本` 不是可执行命令名 | `pwsh -File <github.workspace>/build/...`,用绝对路径 |
| 3 | `missing gcc: D:\a\...\build\mingw64\bin\gcc.exe` | `make-webview2-static.ps1` 只在两处找 gcc:`$USERPROFILE\mingw64\bin`(本机 winlibs)与 `build\mingw64\bin`(历史位置)。runner 上 msys2 装在 `C:\mingw64`,两个都不是 → 静态加载器归档生不出来 | 改成三个候选按顺序试。本机仍走第一个,行为不变 |
| 4 | (第 2 条的连带) | 那一步的 `working-directory` 是 `app/src-tauri`,连相对路径 `./build/...` 也指不到仓库根 | 同上,用 `${{ github.workspace }}` 绝对路径 |

修完后 **ci 全绿**(core 229 passed / 0 failed + 两 crate check + `node --check`,
runner 上实测 7 分钟),release 那条也重跑通过。

### 后来还踩了三个(5–7),其中 ⑥⑦ 是发版链路真正的洞

前四个修完我误以为 release 也能通了,结果 v0.1.44 / v0.1.45 各重跑一次,
又暴露三个。**尤其 ⑥ 说明"本地能跑"不等于"CI 能跑"**:

| # | 现象(runner 上的原话) | 根因 | 修法 |
|---|---|---|---|
| 5 | `missing toolchain path: C:\Users\runneradmin\mingw64\bin`(`build.ps1:24`) | `build.ps1` 与 `make-webview2-static.ps1` 同一个病:只认本机那两个 gcc 位置。runner 上 msys2 在 `C:\mingw64` | 三个候选按顺序试。顺带把 `CARGO_HOME`/`RUSTUP_HOME` 改成"未设置才覆盖"(CI 上可能指向共享缓存) |
| 6 | `webview2-com-sys 0.38.2 is pinned by Cargo.lock but its ... .lib is not in <registry>` | release.yml 里 bundle 前**没 fetch src-tauri 的依赖**。前面的 "Run core tests" 只 fetch 了 `app/core` 的,而静态加载器要从 **src-tauri 的 registry 源码检出**里取 `WebView2LoaderStatic.lib` | 加一步 `cargo fetch --locked`(带 working-directory)。ci.yml 本来就有这步,所以没暴露 |
| 7 | `error: no such command: tauri`(`cargo tauri build` exit 101) | **runner 镜像里没有 cargo-tauri**。RELEASE.md「一次性准备」里那条 `cargo install tauri-cli` 是给本机的,CI 里从来没装 | 加一步 `cargo install tauri-cli --version '^2' --locked` |

**⑥ 还连带一个更值得记的教训:Cargo.lock 是版本号的第三处,升版本时最容易漏。**
我把 `Cargo.toml` 与 `tauri.conf.json` 升到 0.1.45 时没跑 `cargo fetch`,
lock 里 `tokenscope` 还是 0.1.44 —— 任何带 `--locked` 的 cargo 调用都会报
`cannot update the lock file`。**本地复现不出来,因为本地早已 fetch 过、lock 被悄悄改好了**,
只在干净的 runner 上暴露。治本不靠记住:`build/make-latest-json.js` 加第三处断言,
漏了就在本地这步红掉(双向验证过:改回 0.1.44 → 红 + 退出 1 并指明要跑 cargo fetch;
恢复 → 越过这步)。

而且我第一次"修"lock 其实是失败的 —— 把修复前的 `.bak` 覆盖了回去,
`git status` 显示干净是因为文件内容和 HEAD 一致,**而 HEAD 里那一版就是错的**。
教训:**"文件没改"和"文件是对的"是两件事,要看内容,不能只看 status。**

**另一个教训:发版核验不能靠终端回显。** 用 PowerShell 的 `Invoke-WebRequest` 拉
线上 `latest.json`,`$resp.Content` 里的中文全是 GBK 乱码,一度以为"发出去的说明是乱的";
按 UTF-8 读文件 + 数 `U+FFFD` 为 0,确认线上内容是好的。
→ 新增 `build/verify-release.js`(node 写的,按字节比):更新端点 200、版本一致、
签名与本地 `.sig` 逐字一致、安装包可下且 sha256 相同、三件套齐、notes 中文完好
(661 个 CJK、0 个 replacement char)、仓库元数据四项齐。**这一轮全部 PASS。**

**发版(v0.1.43)**:本地 bundle 出 `Grandettoken_0.1.43_x64-setup.exe`(5,017,462 字节,
sha256 `537cbc1e51cf7e845c12e0ccc4898390e7c050c3bf3ad916b002f02cd5563b48`,导入表干净、
无 `WebView2Loader.dll`),`latest.json` 由 `build/make-latest-json.js` 生成(版本/tag/`.sig`/
安装包四项断言都过),三件套上传到
<https://github.com/wacilimonster-source/Grandettoken/releases/tag/v0.1.43>。
`releases/latest` 指向 v0.1.43,更新端点实测 200 且签名与本地 `.sig` 逐字一致。

**这一轮没有回填到 v0.1.42 发布记录里** —— 发版流程与产物核验见 RELEASE.md,本次只做门面与流程。

## 第三轮全量体检:续期断链与代理取证(2026-10-19,v0.1.42)

**怎么查的**:目标是 v0.1.33 之后新进场的约 7800 行(workbuddy / yamledge / peak / custom 四个新模块 +
store / fetch / inject 的大改)。原定七路并行子代理审查**因额度上限全部失败**,改为主代理自己读代码,
配机器化交叉检查:基线(touch 逼重查后 `cargo check --all-targets` 两 crate + `cargo test`)、
43 个 `#[tauri::command]` × 43 个 `invoke()` 调用点的契约、生产区(首个 `#[cfg(test)]` 之前)的
`unwrap/expect` 清点、`SystemTime`/`Command`/`AtomicBool`/时区/默认 0 的模式扫描、
JS 重名函数与 `innerHTML` 未转义插值清点、前端探针。

**两条 P1(功能没真正生效)**

1. **WorkBuddy 的"自动续期"永远进不去**。`collect_desktop_credentials` 四处长候选都写成
   "只收未过期",而 `desktop_tokens` 的续期分支条件是"候选里有过期那份" → 恒为假 →
   续期与落库整段是死代码;`fetch.rs` 那句注释("过期时自动续期")与实际相反,
   而且**没有任何一条用例覆盖"过期后续期"**,所以两轮都没发现。
   后果:v0.1.41 宣称的"摆脱 7 天 TTL"只兑现一半 —— 断供周期从 7 天变成 28 天,到点仍要人工开一次桌面端。
   修法:① 过期判定抽成 `accept(cred, now, include_expired)`,续期路径带 `include_expired = true`;
   ② 编排逻辑抽成纯函数 `plan_tokens`(不碰文件/凭据管理器/HTTP,续期动作注入)→ 这才测得动;
   ③ 落库带上**过期时刻**(以前只存令牌,作废的那份会永远顶在候选第一位、每轮白吃一个 401);
   ④ 续期失败按区域冷却 30 分钟(挂件每 60 秒一轮,不冷却就是每天 1440 次白打)。
2. **体检报告在"系统代理"上印了句没依据的话**,同时把第一轮 P2-12 的取证结掉了:
   reqwest 0.12.28(core 用的那个)的 `default` features 里带着 `system-proxy`,
   hyper-util 0.1.21 `client/proxy/matcher.rs:696` 读 `HKCU\...\Internet Settings\ProxyEnable` ——
   **注册表系统代理确实影响挂件取数**;而报告只查 6 个环境变量,查不到就印"未检测到(全部直连)"。
   本机就是活证据:ProxyEnable=1、ProxyServer=127.0.0.1:7897、用户/机器/进程三级环境变量都没设代理。
   修法:注册表与环境变量**分两行报**,再加一行"取数是否走代理"的结论,并写明
   "代理软件若是 TUN/全局模式,这里看不出来"。签到仍是强制直连(设计如此,已标注)。

**P2 三条**
③ 积分渠道把"上游没给余额数值"当"0 分"报"积分已用尽"(Amount 分支一直是 `unwrap_or(false)`,
两个 kind 口径相反);现网不可触发(自定义渠道恒为 Amount、内置积分渠道恒给 Some),但按不一致修一行。
④ **365 天归档每轮永久丢一笔消耗**:归档 SQL 只读 `ts < before_ts`,跨分界线那一跳的"后一行"不在场,
而下一轮时它的"前一行"已被删 → LAG 永远取不到参照。修法:窗口多读一天(`ARCHIVE_LOOKBACK_SEC`),
按"前一行在窗口内"(`pts < before_ts`)收,归到前一行那天;前一行已删所以不会被重复计。
⑤ 向桌面端要密钥时先等退出、退出后才读管道 → 输出超过管道缓冲就会卡满 15 秒再误判"问不出密钥";
改成两个线程并发读。

**P3 五条**:签到流水注释说 400 天而实为 390(改成 310 天 = 合计 400);归档聚合里
`filter_map(Result::ok)` 静默丢坏行(第二轮刚禁过这个写法,改回"记一行再丢");
`auth_dirs()` 三个候选在正常机器上有两个指向同一目录(去重);DSH 回滚只校验落点 `rel`
没校验来源 `copy`(注释声称要防手改清单)(补上同套校验);`NO_DESKTOP_READER` 只在测试里用
却在生产区声明 → `cargo check --all-targets` 带着 1 条警告(v0.1.41 记录里的"0 警告"是
`cargo test` 口径,两者不是一回事);速览里 `scaleFactor()` 取来没用,白一次跨进程往返。

**复测(硬证据)**:把 ① 与 ④ 的判据临时还原成修复前,新增用例**精确地只让这 2 条失败** ——
`archive_keeps_the_drop_that_straddles_the_cutoff` 拿到 `consumed: 10.0`(应为 20.0)、
`expired_live_credential_reaches_the_refresh_step` 报 `include_expired 该收下过期那份: Expired`;
其余 221 条全过(说明归档那处改动没伤到既有口径)。
修回后:core `cargo test` **223 passed / 0 failed**(基线 218 + 新增 5 条)、
`cargo check --all-targets` core 与壳**均 0 错误 0 警告**、`node --check` 通过、
前端探针退出 0、**真机两条 ignore 用例重跑通过**(桌面端凭据解密 + `/v2` 取数算出余额,链路没被改坏)。

**本轮仍未做(不是遗漏,是有意的)**
- `providers.rs` 里 Trae `credits_amount` / WorkBuddy `RemainPrecise` 字段缺失按 0 处理:
  还是没有样本,不盲改。**注意它的后果被 v0.1.35 的新功能放大了** —— 缺字段现在不只是"显示 0",
  还会发一条"积分已用尽"的系统通知(③ 修的是 None,修不了"真给了 0")。
- O-1:每轮取数约 9 次重叠的 O(n) 区间扫描,仍是结构性优化项。
- **第三轮没覆盖到的**:`fetch.rs` 其余约 850 行改动、`inject.rs` 其余约 1700 行、
  `app.js` 其余约 540 行、`yamledge.rs` 解析主体、`styles.css`、build 脚本与探针内部。
- 真机待验:续期分支的真机路径(要等 accessToken 真过期,本机那份到 2026-11-15)、
  体检报告新的两行代理文案在真窗口里的排版。

**发版(v0.1.42)**:两次提交(`00b08ad` 修复 / `46e7486` 版本号+本记录),tag `v0.1.42` 指向
`46e7486`(main 与 tag 都已推,远端 `git ls-remote` 核对同一条提交)→ release
<https://github.com/wacilimonster-source/Grandettoken/releases/tag/v0.1.42>,三资源齐
(installer **5,013,865 字节**,sha256
`22321988049088bdc172ca5146212ef76a6b6171d48032cfcdc0cb75defd1dc4`;`.sig` 424 字节;
`latest.json` 2,461 字节)。构建日志里三条硬断言都过:`Compiling tokenscope v0.1.42`、
`ok Grandettoken.exe rebuilt (SHA-256 changed)`(旧 exe 先被 stash 走,确实重链过)、
`ok import table clean (no WebView2Loader.dll)`;`corrupt .drectve` 那条链接器噪声照旧出现 3 次
(已知、产物导入表干净,仍未追根)。更新端点实测 **200** 且与本地 `latest.json` **逐字节相同**
(`cmp` 通过)、version 0.1.42、`platforms["windows-x86_64"].url` 指向 v0.1.42 的 asset、
其 `signature` 与本地 `.sig` **逐字一致**(424 vs 424),线上 `.sig` 与本地也 `cmp` 相同,
安装包公开下载 200、5,013,865 字节、sha256 与本地一致。签名走 `$env:CI`/空密码那条老路
(RELEASE.md 第 3 步,从 bash export)。
> 核验脚本这次自己先错过一回:按 Tauri v1 的顶层 `url` / `signature` 字段读,结果报出
> "signature 不一致"和 `url=undefined` —— v2 的清单是 `platforms.<os-arch>.{url,signature}`。
> 结论:核验脚本的字段名要照着实际 JSON 结构打一遍,别凭记忆。

## WorkBuddy 换成读桌面端凭据(2026-10-19,v0.1.41)

**起点**:用户口径「现在还是已登录取数失败,可以通过 dsh-connect-workbuddy 学习一下如何取数」。

**真机取证(全部可复核)**

| 事实 | 证据 |
|---|---|
| 桌面端 auth 文件在 `%LOCALAPPDATA%\CodeBuddyExtension\Data\Public\auth\workbuddy-desktop.info` | 本机存在,10-18 还在更新 |
| `accessToken` / `refreshToken` / `nickname` 是 **AES-256-GCM 信封**(`{$wbEncrypted:1,envelope}`) | 信封里 `keyId=9127dea1b44020a7`,与日志一致 |
| 32 字节密钥要向桌面端自己问 | `WorkBuddy.exe -e "…_linkedBinding('electron_browser_workbuddy_storage').loggerGet()"`(需 `ELECTRON_RUN_AS_NODE=1`),返回 JSON 的 `atRestSecretKey`;**key = sha256(该 base64 字符串)** |
| AAD 是带域前缀的定长转录 | `WB-AAD\0` + 版本 + 长度前缀字段 + suite + keyId + framing + 2 字节 0 |
| 取数端点是 `/v2/billing/meter/get-user-resource`,国内主域 **codebuddy.cn** | 老路径(无 `/v2`)实测已不可用 |
| 用国际版 token 打国内域名 = 401 | 本机同时登着国际版(`www.workbuddy.ai`)与国内版 |
| 历史备份里的 token 可能未过期但已作废 | 6 月那份备份明文 token 到 2027 却 401 |
| refresh 走 `POST {chatBase}/v2/plugin/auth/token/refresh` + `X-Refresh-Token` 头 | chatBase 国内是 `copilot.tencent.com`(与计费不同域) |

**改法**

1. 新模块 `app/core/src/workbuddy.rs`:找 exe → 取 `atRestSecretKey`(缓存 6 小时)→ 解信封 →
   得到 accessToken;`/v2` 取数;**按凭据 domain 选区域基址**。
2. 凭据候选顺序:**上次续期成果 → 桌面端 live(国内版优先)→ 历史备份**;每份候选各自带区域,
   401 时自动换下一份(既有机制)。
3. **续期**:accessToken 过期时用 refreshToken 换新令牌,结果存 Windows 凭据管理器
   (`workbuddy-refresh-cn` / `-global`,按区域分开,不覆盖)。这解决的是"7 天绝对 TTL"这个根因。
4. 网页会话降为**兜底**(没装桌面端时仍可用),端点也一并改成 `/v2`。
5. 失败分五档说清:找不到 exe / 问不出密钥 / keyId 不匹配 / 凭据读不了 / 已过期,各自给可执行动作。
6. `extract_workbuddy` 解析层**没有改** —— 它本来就能算对(24 个包求和)。

**踩过的坑(都写进代码注释)**

- 夹具/排序把国际版 `-ai.info` 排在前面 → 国际 token 打国内域名 401;
- 按 mtime 排序会选中 6 月的旧备份(未过期但已作废)→ live 文件名必须优先;
- 单测里真读了本机凭据、还起 WorkBuddy 进程 → 两条**无关**用例挂掉(其中一条本来就对时刻敏感,
  凌晨跑必挂)→ 桌面端读取在 `cfg!(test)` 下默认关闭,并顺手修了那条时间敏感的夹具。

**验证**

- core `cargo test`:**218 passed / 0 failed / 0 warning**(4 条真机测试默认 ignore);
- **真机端到端**(`cargo test --lib -- --ignored wb_real_machine`):桌面端 live 凭据 → 解密成功
  (accessToken 1318 字符,有效期到 2026-11-15)→ `POST https://www.codebuddy.cn/v2/billing/…`
  → **HTTP 200,24 个包,`extract_workbuddy` 算出余额 2661.21**;
- 与 `dsh-connect-workbuddy` 的 Node 探针结果(合计 2661)一致。

**发版(v0.1.41)**:tag `v0.1.41`(提交 `081e30b`,main + tag 已推;tag 含壳 `Cargo.lock` 的
aes-gcm 依赖)→ release <https://github.com/wacilimonster-source/Grandettoken/releases/tag/v0.1.41>,
三资源齐(installer **5,009,387 字节**,sha256 `ae0b5186e49eedcbe652a7f9c6649599c376fc32844ef29c9660da880dcb1596`;
`.sig` 424 字节;`latest.json` 2,844 字节);更新端点实测 **200**、与本地产出**逐字节一致**、
version 0.1.41、签名与本地逐字一致、安装包下载后 sha256 与本地一致;单 exe 交付检查通过。

## WorkBuddy 渠道"又"断:这次是会话 7 天 TTL,不是接口变了(2026-10-18,v0.1.40)

**现象**:workbuddy 渠道不出数已三天。SQLite 里最后一笔成功快照 **2026-10-15 11:56:47**,
同刻其余 6 渠道(codex/deepseek/hapi/opencode-go/trae/4sapi)每天照常出数;今天 10:28 的
签到探测落库 `outcome=auth`(HTTP 401/403,凭据被网关拒绝)。

**取证(全部现场可复核)**

| # | 证据 | 结论 |
|---|---|---|
| 1 | webview 锁着 Cookies 库(运行中),但**端点探测**打 `get-user-resource` / `checkin-activity-status`(新旧两代路径)全部 **401 + HTML(openresty/APISIX 网关页)**,不是 404 | 接口没迁移,纯认证失败 |
| 2 | 抓官方个人中心 SPA 最新 bundle(`assets.workbuddy.cn/web/usercenter/ad5683203/…/config-*.js`):仍 `POST /billing/meter/get-user-resource`、`withCredentials: true`、恒带 `X-Client-Platform: web`;只有**微信小程序场景**(`sessionStorage["growth-center-platform"]==="miniProgram"`)才补 `Authorization: Bearer`。与 weblogin.rs 顶部 2026-10-08 的实测记录一致 | 平台认证方式没变,请求构造不用改 |
| 3 | 时间线:10-08 12:12 首笔成功(v0.1.19 网页登录)→ 10-15 11:56:47 末笔,**差值恰好 168 小时**;期间挂件每 60 秒都用这份会话真打接口 | 会话是**绝对 TTL(7 天)**,不是滑动窗口 —— 请求频繁也续不了命,到点就死 |
| 4 | 官方「访问密钥」页:SPA 路由 `/keys`(站点地址是 **`www.workbuddy.cn/profile/keys`**,裸 `/keys` 是 404 —— 挂件旧提示里的地址已失效);有效期可选 `7天/30天/1年/永久`;但创建按钮**门控**:i18n 明文「该功能为标准版以上用户可用」,用户实测该账号提示需要升级 | 访问密钥(Bearer)这条**对免费账号不可行**(本账号验证);对标准版以上仍可用(挂件已支持手动粘贴 Bearer,候选顺序里它优先) |
| 5 | 「登录捕获 Bearer 令牌」(v0.1.17 方案)已是**死路**:2026-10-08 CDP 实测 web 登录流程里**没有** Authorization 头、没有 `growth-center-token`、落地 URL 无 `?token=`(设计稿 docs/design/design-workbuddy-web-login.html 已裁决改走 Cookie) | 不复活"抓令牌"方案 |

**根因**:v0.1.19 起挂件吃"浏览器 profile 里的会话 Cookie",这条路活着的前提是会话不失效;
平台把会话做成了 7 天绝对 TTL。于是每过 7 天必断一次 —— 9/17–10/1 那次断是另一个根因
(客户端 5.6.2 加密了本地 accessToken),这次是纯日程性失效。**取数与签到都不用改请求。**

**修复(四处,全部低风险)**

1. **错误文案摆出真相**:`fetch.rs` 的 `expired_credential_msg`(WebSession 分支)改为
   「网页登录已过期(平台会话约 7 天一次,属正常) · 在渠道卡上点「重新登录」即可」;
   登录窗"取数被拒"那条(reason)同步改写,并把访问密钥入口从死了的 `/keys` 改成
   `www.workbuddy.cn/profile/keys`、注明"要标准版以上的账号才能建"。
2. **过期前亮出会话年龄**:登录验证通过时(`wb_finish_login`)把时刻写进 settings
   (`core::weblogin::WEB_SESSION_AT_KEY`,`wb_web_session_at`,无敏感内容);
   `build_view` 对**确实在用 Cookie 会话**的 WebSession 渠道,第 5 天起在详情 extra
   里加一行「网页会话 · 已使用 N 天 · 平台会话实测约 7 天过期;失效后点「重新登录」即可,
   历史数据不丢」。核心逻辑抽成纯函数 `weblogin::web_session_hint`,`core` 挂单测。
3. **复登当天放行补签**:`done_for_day` 把 `auth` 算"当天完结",`can_try` 又被它卡住
   (lib.rs `checkin_state_of`),所以 auth 之后即使复登成功,当天的 100 分按钮也是
   点不动的(实测:2026-10-18 10:28 auth 一次,之后当季不能补;天数为界要静默到次日)。
   `wb_finish_login` 收工时把 workbuddy 签到的 last_code 里的 `auth*` 清掉
   (last_day 落回今天、attempts 清零),节拍器下一拍 / 手动按钮立刻可补一次。
   只清 `auth*`;claimed/already/inactive 是真签完,不动。
4. 明确**不做**:隐藏 webview 常驻续期(TTL 是绝对的,续不了命,还白背一个常驻窗口);
   自动补登(登录要微信扫码,机器替不了);request 层改动(平台没变,无需再变)。

**验证**

- core `cargo test` **205 passed / 0 failed**(基线 204;新增 `web_session_hint_window`:
  没记录不说话、前 4 天闭嘴、第 5 天起说话、时钟拨回不造谣);src-tauri `cargo check --all-targets`
  **0 错误 0 告警**。真机待验:①复登后卡片出数;②当天「领积分」按钮可再点(auth 已被放出);
  ③第 5 天起详情出现"已使用 N 天"一行。访问密钥路线(B)经实测对免费账号**不可行**(门控),
  方案不依赖它。

**发版(v0.1.40)**:tag `v0.1.40`(提交 `121b375`,main + tag 已推)→ release
<https://github.com/wacilimonster-source/Grandettoken/releases/tag/v0.1.40>,
三资源齐(installer **4,977,830 字节**,sha256
`acac61386de1bfbbbbcf9664f1a1294ff619a2685900f51d426f9a38f12ed5f6`;
`.sig` 424 字节;`latest.json` 1,566 字节)。更新端点实测 **200**:线上 latest.json
与本地逐字节一致、`signature` 与本地 `.sig` 内容逐字一致、`version 0.1.40`;
安装包公开下载 4,977,830 字节、sha256 与本地逐字节一致。签名环境变量走
build.ps1 认的 `$env:CI='true'` 这条路(未加密私钥=空密码,RELEASE.md 第 3 步)。
发布过程中本机直连 github.com 的探针行为不稳(node fetch 的 HEAD 全部 connect
timeout,同一分钟内 curl/gh 正常)——核验一律以 curl 实测为准。

## 修「模型页渲染出错」(2026-10-17,v0.1.39)

**现象(用户装了 v0.1.38 之后报的)**:模型页一进去就是 `模型页渲染出错:on is not defined`。

**根因**:v0.1.38 改 `mdlRowHtml` 的客户端按钮时,我把上一行的
`const on = done(id, r.id);` 一起删掉了,而按钮的 class 与提示还在用它 ——
**每渲染一行抛一次 `ReferenceError`**,被 `renderModels` 外面那层 try/catch 兜成
"模型页渲染出错"(那层 catch 本身工作正常,它就是为了这种情况加的)。

**为什么没抓到(这才是要补的部分)**

| 检查 | 为什么漏 |
|---|---|
| `node --check` | 只查语法,不查引用 |
| `build/probe-frontend.js`(假 DOM) | 只加载页面:`inject_state` / `get_model_catalog` 都返回 null → 模型页停在"还没取到模型清单",**永远走不到行渲染** |
| 真机写入测试 | 测的是 Rust 侧写入链路,不碰前端 |

**修法**

1. `app/src/app.js` 把 `const on = done(id, r.id);` 补回去(一行);
2. **把探针补成"真渲染模型页"**:塞进 5 个客户端(DSH 标成"正在运行")与一份价目表,
   真跑 `renderModels` + `renderModelSheet` 的**详情 / 预告 / 已配置 / 被挡四个分支**,断言:
   - 界面上**不许出现「渲染出错」**(它正是"渲染异常被 catch 住"的可见信号);
   - 每条模型行要出 **5 个** `data-act="mdlcli"` 按钮且含 `data-c="dsh"`;
   - DSH 按钮提示里要有「运行中也能写」、客户端详情要有「不用退出」;
   - DSH 预告要写清 `.credentials.yaml` 落点,且**不许混进**通用文案「挂件会记住口径」;
   - 已配置要给「取消写入」、被挡要给重查引导。
   探针退出码把这条也纳入(以前只认 THROWN / UNHANDLED / ERROR)。
3. 补探针之前先跑了一遍**故意留着 bug** 的版本,确认它真报得出来:
   `probe exit=1 | FAIL: 模型页显示了「渲染出错」:模型页渲染出错:on is not defined`。

**验证**:`node --check` 通过;探针输出 `模型页渲染 ok:10 个客户端按钮(含 DSH)、详情 / 预告 /
已配置 / 被挡四个弹层都出得来`,退出码 0;计数按 `data-act="mdlcli"` 走,
不再用会把容器 `class="cdots"` 也数进去的松正则。

**发版(v0.1.39)**:tag `v0.1.39`(提交 `6ead4f4`,main + tag 已推)→ release
<https://github.com/wacilimonster-source/Grandettoken/releases/tag/v0.1.39>,
三资源齐(installer **4,977,131 字节**,sha256 `b4cb1cd1af751677e7c006e89261bab62515d3c9ee97023d680033d4e537720f`;
`.sig` 424 字节;`latest.json` 2,381 字节);更新端点实测 **200**、与本地产出的 `latest.json`
**逐字节一致**、version 0.1.39、签名与本地逐字一致(GitHub 侧 asset digest 也一致)、
安装包下载后 sha256 与本地一致;单 exe 交付检查通过。装 0.1.38 的机器在这条更新里直接升到 0.1.39。

## DeepSeek Harness 写入(2026-10-17,v0.1.38)

**起点**:用户口径「增加一个配置模型添加到 DeepSeek Harness 的功能」,三处裁决:
① 在模型页加 DSH 作为**第五个写入客户端**(与 T / WB / WBA / ZC 同一套交互);
② 落点选 **`cordis.patch.yml` 的 `llm-pi-ai` providers + `.credentials.yaml` 的 `refs`**
(与 DSH 自己的 Models 页同一份事实);
③ **允许热写**(DSH 会收敛外部编辑)。

**取证(全部可复核,2026-10-17 从本机 DSH 安装包 `app.asar` 里取出)**

| 结论 | 出处 |
|---|---|
| 模型/提供方配置落在当前 profile 的 **Cordis patch**(`~/.dsh/profiles/<profile>/cordis.patch.yml`) | `dsh-settings` README:「更改通过当前 profile 的 Cordis patch 持久化」;`dsh-config-editor` 同 |
| 路由字段:`api`(三选一)/`baseURL`/`apiKeyEnv`/`models[]`(`id`/`name`/`contextWindow`/`maxTokens`/`input`) | `dsh-llm-pi-ai` `lib/index.js` 的 `profile` schema 与 README |
| **非目录路由**必须给 `api` + `baseURL` + **非空** models;目录里的提供方(如 `opencode-go`)由目录给端点 | 同上 README「失败与恢复」「何时选择」 |
| `apiKeyEnv` 是**凭据引用**,值落在 `~/.dsh/.credentials.yaml` 的 `refs:`;名字是 `<ROUTE>_API_KEY` | `dsh-credentials-local` README(文件示例里 `refs:` 与 `OPENCODE_GO_API_KEY` 就是这条) |
| 外部编辑会被收敛:配置编辑器"在派生候选配置前应用外部更改";凭据存储"同一把写锁下保留注释与未触及条目" | `dsh-config-editor` / `dsh-credentials-local` README |
| 本机真文件:profile `desktop`、patch 里有 `agent-default-model` + `llm-pi-ai`(route `opencode-go`)、凭据文件 `refs.OPENCODE_GO_API_KEY` | 真机读取(见下方"真机取证"测试) |

**落点与契约**(细节写进 `DESIGN.md` 12.3.2,不在这里重复):两份文件一起备份、一起读回校验;
route 键 = 来源 id;目录 route 不写端点;`models` 是替换语义所以先读回再合并、保留用户手加字段;
摘掉最后一个模型时连 route 一起收走;密钥只在没人再用这个引用名时才撤;选中项只读不写。

**新模块 `yamledge.rs`(不引第三方 crate)**

- 只认块式 mapping / 块式 sequence / 标量;流式集合与 `!!js` 当不透明标量;注释、空行、CRLF 原样,
  末尾没换行不硬塞;**行范围替换**(不是整树往返,否则注释全丢)。
- 两种合法的列表缩进写法都收(短横比键深 / 与键同缩进)。
- 12 条单测;含一条"游标没前进就报错"的兜底 —— 这条兜底是被真机教训逼出来的(见下)。

**跨目录备份**:DSH 的 patch 与凭据文件不在同一层,而回滚原本是"按文件名放回配置目录"——
照旧逻辑会把 `cordis.patch.yml` 放到 harness home 根下。所以 DSH 的备份目录多一份
`.inject-layout.json` 落点清单,回滚照它逐份放回;清单只允许 root 以内的相对路径;
**写入前不存在的文件,回滚时删掉**。其它四家的备份目录与以前逐字节一样。

**热写口径**:`Client::blocks_when_running()` —— 前四家仍"在跑就拒写",DSH 例外;
`inject_rollback` 的运行检查也改成按客户端判(否则 DSH 写入允许、回滚却报"先退出它")。

**踩过的三个坑(都写进 DESIGN.md 12.7)**

1. 夹具用了 Rust 的 `\n\` 行续接 —— **续接吃掉下一行的缩进**,夹具被悄悄改成顶格 YAML,
   失败信息却指向解析器。夹具改用 `r#"…"#`。
2. 生成 YAML 块时把列表项写成了与 `models:` 同缩进 → 插进去的列表被当成 route 的兄弟节点。
   生成侧统一"项比键深 2 列",解析侧两种都收。
3. 解析器里"行内起头的序列项"(`- key: value`)第一轮就撞上缩进检查 → `break` 但游标不前进 →
   外层无限塞空节点 → **6 GB 内存几分钟吃满后 abort**。修完加了"游标没前进就报错"的兜底。

**验证(本机)**

- core `cargo test`:**204 passed / 0 failed**(另有 2 条默认 `#[ignore]` 的真机测试,见下)。
  新增 25 条:YAML 编辑器 12 + DSH 契约 13(其中真机形状端到端那条默认 ignore;基线 181 → 206)。
- 壳 `cargo check --all-targets`:**0 错误 0 警告**。
- 前端:全部 `.js` 过 `node --check`;`build/probe-frontend.js`(假 DOM)退出码 0。
- **真机测试一(只读)**:拿本机**真实的** `~/.dsh/profiles/desktop/cordis.patch.yml` 与
  `~/.dsh/.credentials.yaml` 过一遍"可读性检查 + 预告",预告内容为「在 llm-pi-ai 里新增
  route「4sapi」与这条模型」、落点与密钥引用名正确,且**预告前后两份文件逐字节不变**。
- **真机测试二(在真文件的逐字节抄本上,不碰 `~/.dsh`)**:把真实两份文件抄进临时 harness home,
  跑通「写入 → 独立读回校验 → 取消写入 → 回滚删除 → 回滚写入」——断言:原有顶层条目数与
  `agent-default-model` / `opencode-go` / 开头注释全部留着、`records:` 段留着、
  两次回滚分别回到"写入之后"与**逐字节回到原始文件**(原本没有的凭据文件则被删掉),
  且站点故意标成"在跑"验证热写不被挡。
- **真机测试三(在真实 `~/.dsh` 上真写一次,然后回滚)**:用凭据管理器里的**真 4SAPI 密钥**,
  把 `route「4sapi」+ claude-sonnet-5` 真写进 `~/.dsh/profiles/desktop/cordis.patch.yml`、
  密钥真写进 `~/.dsh/.credentials.yaml` → 独立读回校验过 → 取消写入过 → 回滚两次
  (`~/.dsh/backup-inject/20261017-151752` 写入前快照、`...-151754` 取消前快照)→
  **两份文件逐字节回到写入前**(断言 `read == read`),随后独立核验真实文件里已无
  `4sapi` / `4SAPI_API_KEY`、原有 `opencode-go` 与 `records:` 段原样,测试产生的两个备份目录已清理。
  这一步用的是发版前的一次性临时测试(验完即删),仓库里留下的是**只读**的第 1、2 条。
- 端到端写入/回滚(小夹具):幂等 → 不建备份、文件逐字节不变;目录 route 不加端点、
  原有模型保留;用户手加的 `maxTokens`/`reasoningEfforts` 保留;结构认不出时拒绝且文件原样。

**未在真机验(不阻塞,列在这里)**

1. 模型页第五个按钮的**真机点击**(只过了 `node --check` 与假 DOM 探针,没在真窗口里点过;
   写入链路本身已在真实 `~/.dsh` 上真写 + 回滚验过,见"真机测试三")。
2. DSH 侧**真的用 4SAPI 模型发出过一次请求**(要留着那条写入、再回 DSH 选模型;本轮为了回滚没有留)。
3. 多 profile 场景(本机只有 `desktop`):"优先 desktop → 挑配了 llm-pi-ai 的 → 按名字第一个"
   只有单测覆盖。

**发版(v0.1.38)**:tag `v0.1.38`(提交 `641ad9d`,已推 main + tag)→ release
<https://github.com/wacilimonster-source/Grandettoken/releases/tag/v0.1.38>,
三资源齐(installer 4,978,276 字节,sha256 `70594ee789fb01f14e77f65a8d7b270890b4c1966fcb6f4a189dba42fc72ab10`;
`.sig` 424 字节;`latest.json` 3,124 字节);
更新端点实测 200、与本地产出的 `latest.json` **逐字节一致**、version 0.1.38、
签名与本地逐字一致,安装包下载后 sha256 与本地一致;单 exe 交付检查(无 `WebView2Loader.dll` 混入)通过。
发版过程踩到一个坑,已记进 DESIGN.md 12.7:**不要用 PowerShell 的 `-replace | Set-Content` 改版本号** ——
它按 ANSI 重写整份文件,`多平台` 变乱码、注释行被并成一行(当年这版差点带着坏 manifest 发出去);
改用定点编辑只动那一行,`git diff` 复核后 `--amend` 修回。

## 速览改"抬升卡片"(2026-10-17,v0.1.37)

**背景**:v0.1.36 发布后,用户仍然说"显示还是很奇怪"。这次先做设计再动手,文档见
`docs/design/design-pill-peek-2026-10-17.html`(五个方向都按真实 164×46 做可点 mock)。

**根因(真机量的)**:胶囊是 `border-radius:999px`。46px 高时圆角 23 = 标准药丸;纵向拉到 120px 后
圆角被钳到 60 —— 宽 164、高 120、四角半径 60,视觉上从"药丸"变成"被吹起来的豆子"。
再叠一条分割线、上半信息下半胶囊脸,就成了"缩小版面板"。**两次"怪"的共同点都是形状/尺寸变了。**

**裁决**:方案 C(抬升卡片)。其余取推荐值:三行全显示、托盘不跟、开关默认开。

**落地**:
- `peek-open` 时圆角从 999 收到 **16**(180ms 过渡),信息在胶囊脸下方,不再有额外分割线;
- 窗口**向下**长高(顶边不动),高度按实测三行夹在 56~96,宽度锁在「当前宽 vs 信息所需宽」的较大值(≤280);
- 再点一次立即收回,4.5 秒自动收回;切形态前先收回(避免"class=pill 但窗口是卡片高"的半截状态);
- 显示页新增「单击胶囊看速览」开关,默认开;关掉后单击无动作,▲ 仍展开面板。

**验证**:真机(125% 缩放)67→123、圆角 999→16、胶囊 142×46 不被压、向下长高、4.5 秒收回、零异常;
`node --check` 与假 DOM 探针通过。

**发版**:tag `v0.1.37` → release <https://github.com/wacilimonster-source/Grandettoken/releases/tag/v0.1.37>,
三资源齐(installer 4,949,653 字节,sha256 `bcb176228935308d9543d4efd2127fcb76b95b39162e0e1ea10aabda0f017259`);
更新端点实测 200、version 0.1.37、签名与本地逐字一致。

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
(`docs/design/design-whale-inspiration-2026-10-16.html`)。裁决结果:**第 4 项「今日预算」作废**;
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

方案与取证见 `docs/design/design-workbuddy-web-login.html`。用户裁决:**1A 2A 3B 4A 5A 6A 7B**。

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

## UI 优化第二轮(2026-09-23,设计稿 `docs/design/design-ui-motion-2026-09-23.html`)

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

- `docs/prototype/token-dashboard-prototype.html` —— 交互原型,四种折叠形态(面板 / 紧凑条 / 胶囊 / 边缘吸附)
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
  > 2026-10-20 更新:标题栏那一处已删除,入口收敛为底栏一处 —— 见本文件开头
  > 「设置入口:两处 → 一处」。

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

**A. docs/report/bug-report-2026-09-17.html 的 14 处缺陷:逐条核验,全部处理**

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

设计稿 [`docs/design/design-4sapi-models.html`](docs/design/design-4sapi-models.html),已实现口径见 `DESIGN.md` 第 12 节。
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

按 `docs/design/design-auto-checkin-2026-10-13.html` 的十项裁决(全 A)实现。**M0 只读取证先跑,结论回写文档**(裁决 10A)。

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

## 首页形态设计稿落地:统一视图模型 + 面板重构(2026-10-22)

依据 `docs/design/design-home-forms-2026-10-21.html`(含同日复核修订)开发。**先落 P0 的地基,再做 P1/P2/P3**——
设计里那句话是"在它之前改布局,只会让四种形态多一种看起来不一样的方式",所以顺序没有颠倒。

**P0 · 统一视图模型(`app/core/src/view.rs`,约 1200 行 / 21 项单测)**
七态 `ViewState`(fresh/stale/auth/error/noKey/exhausted/limited;`!valid` 按 `authSource` 分成"鉴权失效"与
"取数失败",两种出路)、五桶紧迫度(`TIER_ACTION/RATIO/MONEY/POINTS/REST`)、绑定窗(限流窗优先、否则剩余最少)、
扁平 `Headline{percent,amount,unit,points}`(全空 = 「——」,与"确定是 0"分开写)、列表级 `HomeView`
(order / risk / actionCount / staleCount / errorCount / checkin / totals)。**没有分母的金额不再被判成健康**:
进 T2 比绝对额,且低于 `moneyFloor`(¥10)进 T0 且不是绿色 —— D02 修掉。纯函数、可单测。
配套:`config.rs` 新增 `moneyFloor` / `staleAfterSec`(900)/ `textTone` / `pillDwellSec`(12)/ `showUsed`,
均靠容器级 `serde(default)` 让老配置自动取默认值;新 IPC 命令 `home_view` 走 `fetch_shared`
(与 `get_channels` 同一闸门、5 秒近期结果复用,不多打账务接口),签到 `doneToday` 复用 `CheckinCfg::blocks_today`。

**P1 · 面板重构(风险带 / 两级行 / 分组 / 骨架 / 失败横幅)**
顶部三格消耗换成**风险带**:总余额(副行留今日 + 环比 + 非人民币并列,不折算)/ 最紧(可点,跳过去并展开)/
签到(常驻,全签完仍显示「✓ 3/3」)。行压回两级:紧急(T0 ∪ 跌破红线)两行 + 进度条,其余一行;
「更新于」只在陈旧时说;第三段(窗口条 / 到期列表 / 日周月)收进 `.seg3`,只在展开时显示 —— 一屏重新放得下九行。
冷启动画骨架;一轮 ≥3 个渠道取数失败时出横幅并给「看诊断」;渠道 ≥12 个时给过滤框。

**P2/P3 · 折叠形态与无障碍**
紧凑条:降级顺序改成"重要性倒序"(第一个 chip = 最紧,永远完整;从最后一个开始去名字/舍小数) + 签到 chip 常驻;
胶囊:主数字与限定语走后端,告急时锁定**自动**轮播(手动仍可用);吸附竖条:一整块颜色 → 一段一渠道(顺序同面板,
陈旧的加斜纹);键盘 `Ctrl+1/2/3` 切形态、`↑↓` 走行、`/` 聚焦过滤,Esc 循环补上紧凑条;
「非颜色模式」每行写文字态;「用掉还是剩余」切换百分比口径(四处同源,一处翻转)。

**验证**
- core `cargo test --offline`:**311 passed / 1 failed / 6 ignored**,其中 view 21 项全过;
  唯一失败 `fetch::tests::a_failed_request_leaves_a_usable_line_in_the_log` 是**既有环境故障**
  (系统代理对 127.0.0.1:1 返回 502 而不是拒连),已在干净 HEAD 上复现确认与本次无关。
- `node --check app.js` 通过;Tauri 外壳 `cargo check --offline`(msvc)零告警。
- 新增 **`build/jsdom-smoke.js`**:jsdom 里跑真渲染,**26/26 通过**(风险带三格、总余额并列、
  分组与两级行、骨架、横幅、签到 chip、胶囊挑最紧的 + 限定语、吸附分段、非颜色模式、视图缺失兜底)。
  它当场抓出过一个真 bug(`hvApply` 漏带 `totals`,导致风险带静默退回老汇总),已修。
  ⚠️ 未验证:像素与真机几何(两级行实际高度、风险带在三档字号下是否溢出),需要真机复测。

**发版 v0.1.56**(2026-10-22):本地打包 → `latest.json` → GitHub Release 三件套
(`setup.exe` / `.sig` / `latest.json`)→ `verify-release.js` **13/13 PASS**。
commit `68d46a9`,tag `v0.1.56`,更新端点已能拉到新清单。
唯一一处测试口径收紧:`fetch::tests::a_failed_request_leaves_a_usable_line_in_the_log`
在"透明代理替 127.0.0.1:1 应答 5xx"的环境里会误报,现在按错误码分流 ——
连接层(E_CONNECT/E_TIMEOUT)才断言没有 status,被代理应答时反过来断言必须是 HTTP 层错误码。

## v0.1.57:面板被吸附条压空 / Qoder「有活动却说没有」/ 三形态体检(2026-10-22)

### P0 · 面板整页只剩底栏(`styles.css`)

真机截图一眼看到:标题栏、风险带、底栏都在,中间一大片空白,九行渠道一行不剩。
量出来的原因:**吸附竖条 `#dockBody` 在面板形态里也是显示的** ——
v0.1.56 新加的 `.dock-body{display:flex!important}` 没有挂在 `body.docked` 下,
盖掉了前面的 `.dock-body{display:none}`。它 `height:100%`,在 body 的 flex 列里
以 `flex-shrink:1` 抢走 415px,`flex:1 1 0%` 的列表被压成 **0 高**。
修:`body.docked .dock-body{display:flex;…}`(去掉 `!important`,靠状态门拿优先级)。

**防复发**:新增 `build/check-css-forms.js`(纯文本、零依赖)——凡是"基础规则是
`display:none` 的整块容器"(`.dock-body` / `.pill-body` / `.cctl` / `.manage` …),
其它规则再给它 display 时必须带 `body.<状态>` 前缀。已接进 CI(`ci.yml`)。
它当场抓到的就是这条;反向验证(把旧写法放回去)会 FAIL。

### Qoder 签到:活动在,但被判成"本期无打卡活动"(`checkin.rs` / `lib.rs` / `app.js`)

真机取证(2026-10-22 15:5x,国际版,桌面端凭据 `dt-…`):

```
GET /sash/api/v1/me/campaigns → 200
{ "showCampaign": true, "claimable": true,
  "campaigns": [
    { "campaignKey":"act-20261021-273", "actionType":"VIEW_DETAILS",
      "claimStatus":"CLAIMABLE", "placements":[{…"title":"1024 | 创造力集结"}] },
    { "campaignKey":"act-20261020-944", "actionType":"VIEW_DETAILS", "claimStatus":"CLAIMED" } ] }
```

10-20 那条 `CLAIM_BENEFIT`(每日 100 分)已经不在列表里了;当天的活动换成了
`VIEW_DETAILS` + `CLAIMABLE`。而 `qoder_pick` 只认 `CLAIM_BENEFIT` →
`has_campaign=false` → `Inactive("本期无打卡活动")`。用户在客户端明明看得到那一条,
挂件却说"没有" —— 这是**假阴性**,不是"活动换季"。

修:新增 `Outcome::Manual(标题)` —— **有活动,但只能在客户端里点开领**。
- `qoder_pick` 记下第一条 `CLAIMABLE` 的非领取类活动及其标题(`placements[].content.zh/en.title`);
- `parse_qoder` 在"没有 CLAIM_BENEFIT"时先看 `manual`,再退回 `Inactive`;
- 界面:`manual:*` → 徽标「有活动」(蓝框 `.ckb.info`)+ 文案「有活动:X · 只能在客户端打开领取」;
- 计数:`manual` 与 `inactive` 一样算"今天没有待办"(`signed_today`),否则聚合行会永远
  喊"还有 N 个没签" —— 而挂件根本做不了这件事。
- **不为它发 claim 请求**:客户端对 `VIEW_DETAILS` 只打开活动页(`growth-page/activity-iframe`),
  没有"直接领"的语义;猜着 POST 可能把活动标成已领却不发奖。

**升级当天的旧结论**:`inactive` / `manual` 是**我们自己的分类器**给的结论(哪些活动算打卡),
规则一改就得重问。`last_auth_version` 从"只记 auth"扩到也记这两种,`blocks_today` 里
`inactive_is_stale()` 命中就不挡当天 —— 否则升级后当天仍然显示"本期无活动",
用户看到的就是"改了也没用"(与 v0.1.47 那次 `auth` 陈旧是同一类问题)。

### 三形态体检:16 场景 × 3 形态(`build/visual-forms.js`,新增)

jsdom 不参与布局,CSS 写错它一声不响(v0.1.56 的吸附条就是这么漏过去的)。
新脚本连真机 CDP,注入合成数据 → 逐形态 `applyForm` → 截图 + 量**横向越界 / 纵向越界 /
文本裁切**,产物在 `docs/assets/qa-forms/`(48 张图 + `report.json`)。

> 测试自身的两个坑:① 60 秒轮询会把真实数据灌回来盖掉注入数据 —— `app.js` 顶部是
> `const invoke = T.core.invoke`(**按值捕获**),改 `T.core.invoke` 没用,必须覆盖
> `window.refresh` / `window.loadCheckin` 把轮询停掉;② 行内 `.sweep` 微光从 `-360px`
> 扫进来,会被误报成越界(被祖先 `overflow:hidden` 裁掉的不算)。

设计文档:`docs/design/design-forms-qa-2026-10-22.md`(先出设计再改代码)。修掉的:

| # | 问题 | 修法 |
|---|---|---|
| P0-2 | 紧凑条里出现竖排的「需要处理 1」 | `body.form-compact .grp/.fstrip/.sk-wrap{display:none}` |
| P0-3 | 紧凑条第二行被裁(列表 74px > 卡片 46px) | `body.form-compact .r2{display:none}`;连带把 chip 变窄,能放的渠道从 1 个回到 2 个 |
| P0-4 | 紧凑条空态文案折行被裁 | 空态改用短句(底栏文案同源)+ `.cempty` 加 `max-width/nowrap/ellipsis` |
| P1-1 | 紧凑条横向裁切(many 283>279,big 340>321) | `+N` 宽度改成真量(插 `+99` 隐形徽标);间隙数改成 `n-1+(hidden?1:0)` —— 有 `+N` 时少算一个 gap |
| P1-2 | 没有人民币合计时风险带整条消失(纯积分/冷启动/全隐藏),数据回来整页跳 73px | 风险带常驻,没数的格子写 `——`;删掉已被取代的 `renderSummary()` |
| P1-3 | 底栏「8 正常 · 3 预警」双算(3 是 8 的子集) | `正常 = withKey.length - bad - warn` |
| P1-4 | 失联行仍显示旧数值且没有出处 | `isOld = isStale || failed` —— 失联行也画 `.rhint`「显示的是 X 的成功快照」 |
| P1-5 | 骨架期底栏留着上一轮的「8 正常」 | 骨架分支同时写 `fdot/fstat` =「正在取数…」 |
| P2-2 | 全隐藏时紧凑条还显示「✓ 0/0」 | `compactCheckinChip` 在 `total === 0` 时不渲染 |
| P2-3 | 失败横幅随列表滚走 | `.fstrip{position:sticky;top:0}` |

**复测**:16 场景 × 3 形态跑完,紧凑条不再有纵向越界、`scrollWidth ≤ clientWidth`;
面板无横向越界;`loading/empty/checkin` 三个场景风险带都在;`check-css-forms.js` PASS;
core `cargo test --offline` **314 passed / 0 failed / 6 ignored**(净增 3 项:
`qoder_view_details_claimable_is_manual_not_inactive`、`qoder_manual_blocks_the_day_but_keeps_its_code`,
以及把 `stale_auth_…` 里"只有 auth 受版本影响"改成 claimed/already 与 inactive/manual 两组)。

**发版 v0.1.57**(本地打包,2026-10-22):三处版本号同步 → `bash build/release-local.sh`
(2m36s)→ `make-latest-json.js` → GitHub Release 三件套(`setup.exe` / `.sig` / `latest.json`)。
commit `a65fc58`,tag `v0.1.57`,`verify-release.js` **13/13 PASS**,
`verify-signature.js` **PASS**(公钥真能验这一版的包 —— 已装用户收得到)。
本次新增的 `build/check-css-forms.js` 已进 CI,`build/visual-forms.js` 是人工工具(要真机窗口)。

### 复测中又抓到的两处（同一次发版里修掉）

1. **紧凑条 `.r2` 的第一版修复没生效**：写成 `body.form-compact .r2`(0,2,1)，
   压不住面板的 `.row.urg .r2{display:block}`(0,3,0) —— 紧急行照样两行。
   同一个坑还有 `.rhead` 的内边距与 `.val .v` 的字号：`.row.urg` 那两条都是 (0,3,0)。
   全部改成 `body.form-compact .row …`(0,3,1)。**教训:CSS 覆盖要看权重，不只看顺序。**
2. **紧急行的第二行与数值副行重复**：`.r2` 在 `hv.secondary` 为空时回退成 `subVal`，
   于是 WorkBuddy 那行「无近期到期」出现两次。改成空则空，并加 `.r2:empty{display:none}`。
3. **「最紧」格把数字裁掉**：`WorkBuddy 1…` —— 省略号吃掉的是数字。名字包一层 `.rn`
   自己缩，数字不缩（代价是名字剩 5~6 字，接受）。
4. **全隐藏时风险带还写「✓ 0/0」**：与紧凑条同一个口径，`total === 0` 写 `——`。

**最终复测**(0.1.57 真机):16 场景 × 3 形态,横向/纵向越界 **0**;
仅剩的"裁切"是风险带副行与「最紧」格的名字(都有省略号、都有 tooltip 与下一行兜底)。
`cargo test --offline` 314 passed;`check-css-forms.js` PASS;`node --check` 全过。

**发版记录**:安装包 `Grandettoken_0.1.57_x64-setup.exe`(5,146,450 B,
sha256 `3a5d9425…`)+ `.sig`;release <https://github.com/wacilimonster-source/Grandettoken/releases/tag/v0.1.57>。
本机已用打包产物覆盖 `%LOCALAPPDATA%\Grandettoken\Grandettoken.exe` 并运行。
`latest.json` 的 notes 走应用内更新卡,release 正文另写(同一件事的两种长度)。

### 发版 v0.1.58（2026-10-22）

Qoder 自动领取与新活动监测；顶部摘要改为聚焦未来 30 天内确认到期的积分，可点击筛选渠道，并修正未知到期信息的展示。

本地签名 NSIS 安装包 `Grandettoken_0.1.58_x64-setup.exe`（5,175,222 B，SHA-256 `cc03c17c7e1ad0013bf4e968a5fb8dc352f1f60179cc8e01bff4ac0e42434cdb`）及 `.sig` 已发布到 [GitHub Release v0.1.58](https://github.com/wacilimonster-source/Grandettoken/releases/tag/v0.1.58)。提交 `5998226`，tag `v0.1.58`。

`node --use-env-proxy build/verify-release.js` **13/13 PASS**；`node build/verify-signature.js` **PASS**（tauri.conf.json 中的公钥验证通过）。

### 发版 v0.1.59（2026-10-22）

优化 Trae、WorkBuddy、Qoder 的活动监测和自动领取，保留既有签到状态并提示需在客户端参与的新活动；按协议修正 OpenCode Go 端点，并明确不兼容目标客户端或未知协议时的处理方式。

本地签名 NSIS 安装包 `Grandettoken_0.1.59_x64-setup.exe`（5,196,007 B，SHA-256 `99031f4a271a763287dc098a18a693ac1fcf26d18737c39318b9f4348fe21233`）及 `.sig` 已发布到 [GitHub Release v0.1.59](https://github.com/wacilimonster-source/Grandettoken/releases/tag/v0.1.59)。发布提交 `e7cc67f`，tag `v0.1.59`。

`node --use-env-proxy build/verify-release.js` **13/13 PASS**；`node build/verify-signature.js` **PASS**（tauri.conf.json 中的公钥验证通过）。
