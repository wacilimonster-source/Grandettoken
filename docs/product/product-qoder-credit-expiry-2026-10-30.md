# 产品设计 · Qoder 积分逐笔到期明细

> 2026-10-30 · 依据真机探测(桌面端凭据 + PAT + 公开 web bundle 0.0.743 实拆)
> 关联:0.1.71 签到中心;`providers.rs` extract_qoder 里那行 `"逐笔到期明细" → "接口未提供"`

## 0. 先说人话

Qoder 渠道的首页风险带现在只说"余额 986 分",**不说其中多少分哪天过期** ——
而 Qoder 的积分(签到 / 加量包)偏偏是**按包过期**的:过期了就是白丢。
详情卡片里那行「逐笔到期明细:接口未提供」是实情,也是缺口。

用户给了个人访问令牌(PAT),问能不能拉到过期明细。**排查结论:不能,且三条
链路里只有一条走得通** —— 下表是全部证据,然后是顺着那条路的设计。

## 1. 排查结论(2026-10-30 实测)

| 链路 | 实测结果 | 结论 |
|---|---|---|
| ① PAT(`pt-…`) | 两个网关(openapi.qoder.sh / .com.cn)都回 `TOKEN_EXPIRE`(token is not active);格式被识别但令牌未激活 | **此令牌已废**。且 PAT 是开发者 LLM API 的凭据 —— 桌面 Bearer(有效、能过 v2 网关)调 `/api/v1/*` 都 401,说明账务端点要 **web 会话**,PAT 大概率也不行 |
| ② 桌面端凭据(应用现有) | `/api/v2/quota/usage` 200,但**只有汇总**:`userQuota`+`addOnQuota` 的 total/used/remaining + 账户级 `expiresAt`(9999 = 不过期);v2 网关上 8 个过期/流水候选路径全部 404;`/api/v1/*` 5 个候选全 404 | **拿不到逐笔**。桌面 API 与网页 API 是两套后端 |
| ③ qoder.com web 会话 | 官方响应里 `addOnQuota.detailUrl` 亲自指向 `https://qoder.com/account/usage`;web SPA bundle(公开可下)里 **个人 Account/Usage 页的 AddOnCredits 组件渲染逐包过期** | **唯一通路**。数据存在,只是要网页会话 |

**③ 的证据(从公开 bundle 实拆,不是猜):**

- i18n key:`qoder.pages.Account.components.AddOnCredits.ExpiringSoon`(橙色标)、
  `ExpireInfo`,渲染模板原文 ——
  **`"Remaining {value} {unit}. Expires on {time}"`**;
- 每包字段:`limit_value` / `used_value` / `remaining_value` / `unit` /
  **`expires_at`(毫秒时间戳)** / `status` / `source`(PURCHASED / BONUS /
  REFUND…);组件还按"0~5 天内到期"自己标"即将过期";
- 同族端点(同 bundle 提取,会话后一次抓包即可钉死个人版用哪个):
  `GET /api/v2/me/usages/big_model_credits`、
  `GET /api/v1/me/usages/big_model_credits/histories`、
  `GET /api/v1/me/quotas/{quota_key}/histories`、
  `GET /api/v1/me/userplan`、组织版同款
  `GET /api/v1/organizations/{id}/dedicated-resource-packages/usage/{quota_key}`
  (响应就是 `summary` + `details[]{…, expiresAt, scheduledAt, isActive}`)。

**安全提示:用户贴在对话里的那个 PAT 建议立即去 qoder.com 设置里吊销重发**
(它已明文出现在聊天记录里;反正也处于未激活状态)。

## 2. 方案:web 会话链路 + 真实到期数据接入

### 2.1 凭据:复用 WorkBuddy weblogin 模式(应用已有全套机械)

- 打开独立登录窗加载 `https://qoder.com/account/usage`(不登录会被 302 到
  `/users/sign-in?oauth_callback=…`,判定规则照抄 `core::weblogin::is_post_login`
  的"落到目标路径即成功"),成功后抓**会话 Cookie**;
- Cookie 只进 Windows 凭据管理器,不进配置文件 / 日志 / 导出(与密钥同一红线);
- **会话只用于账务只读查询**。签到照旧走桌面 Bearer —— 两条链路职责分开,
  网页会话失效绝不影响签到,反之亦然;
- 入口文案:"积分明细需要网页登录 · 打开一次 qoder.com 即可"(放渠道卡;
  没登录就维持现状,不多画一个像素)。
- 备选(不做进 P0):桌面 webview 自带 openapi.qoder.sh 会话,其 Cookie 库
  (os_crypt,qoder.rs 的解密机械能读)也许有可用会话 —— 真机验证过再说不算数,
  不作为设计依据。

### 2.2 数据:一条抓包钉死 + 快照缓存

- 实现第一步:登录后 webview 里抓一次 usage 页的 XHR,确定个人逐包端点
  (候选已列在 §1;字段结构已由渲染代码确证,端点名二选一的事);
- 拉到的包列表落到**渠道快照的 `expiring[]`**(`{at, amount, label}` 形状,
  首页风险带 / 明细卡已消费这个结构,零改动复用):`at = expires_at`,
  `amount = remaining_value`,`label = source 的人话`(购买包 / 赠送 / 退款包);
- 取数节奏沿用渠道节拍;失败**沿用上次成功快照 + 标时间**(与余额同一原则,
  绝不渲染成"没有临期")。

### 2.3 界面:三层,全是对既有组件的填空

| 层 | 现状 | 接入后 |
|---|---|---|
| 首页风险带「临期」格 | Qoder 无数据(临期格写"——") | 真实"未来 30 天到期 N 分 · 最早 MM-DD",与其他渠道同构 |
| 首页行 toneOf | 积分型按"近 7 天到期占比"判,但 Qoder 恒为 0 → 恒绿 | 同一公式吃到真数据:临期占比 ≥10% 黄、≥30% 红 —— 不新增任何口径 |
| 详情卡「逐笔到期明细」 | 写死"接口未提供" | 逐包列表:剩余 N 分 · MM-DD 到期 · 来源;超过 3 条折叠 + "全部 X 包";每行带平台日期,**不再用估算** |

**口径三条(红线):**

1. `expires_at` 是平台事实,Qoder 的包**用真日期**;Trae 那套"签到积分 31 天
   有效期"的估算不动 —— 两渠道各自的口径都注明来源(平台 / 估算),
   明细卡写清"按平台返回"还是"按签到周期估算";
2. 到期数据**不参与余额渲染**:拉不到包 ≠ 余额归零,失败显示旧快照;
3. `remaining_value` 与 `addOnQuota.remaining` 对不上时**以包求和为准**并标
   "与渠道合计差 X 分"(平台两口径经常不同步,如实呈现,不替平台圆)。

### 2.4 不做什么

- 不做签到双链路(网页会话绝不碰签到,避免双签);
- 不做代购 / 续包之类的写操作(只读);
- 不把 PAT 做成输入项(排查已证伪;真要做也轮不到它);
- P0 不做组织版(企业 dedicated 包;字段结构相同,把组织 id 加进凭据即可,
  留给有企业账号的用户需求出现时再接)。

## 3. 分期

- **P0**:web 登录窗 + 会话存取 → 抓包钉死端点 → `expiring[]` 落快照 →
  三层 UI 填空。验收:渠道卡能看到逐包到期日,风险带临期格有数,断网/失效
  走旧快照。
- **P1**:临期变化提醒(某包 7 天内到期且剩余 > 阈值时,提示一次,
  与跌破红线通知同一套"只响一次"机制)。
- **P2**(视需求):组织版 dedicated 包;把 WorkBuddy 的"平台侧有效期"也换成
  同一套真实日期展示。

## 4. 真机复查:登录成功却取不到明细(2026-11-03)

**现象**(用户报「PC 端 Qoder 网页登录后成功了,但提示已登录、获取访问失败」):

- 网页登录那一跳是成功的,平台也认了这份会话 —— 诊断包里
  `/api/v2/me/usages/big_model_credits` 回 **200 + JSON**,不是 401;
- 但三个候选端点全部被判「不像包列表」,逐包明细一条都没出来;
- 渠道卡上写的又是「网页会话未登录 / 被平台拒绝」,把人往**重登**上引 ——
  重登解决不了任何问题,因为问题不在会话。

**实测到的响应结构**(诊断日志,脱敏后):

```text
/api/v1/me/dedicated-resource-packages/usage/big_model_credits  → 404
/api/v2/me/usages/big_model_credits → 200
  {user_id, quota_key, status, plan_quota, resource_package_quota,
   «32 字符的键», total_quota, lastResetAt, nextResetAt}   ← 全是汇总字段
/api/v1/me/userplan → 200(计划对象,无逐包字段)
```

也就是说:个人版的 v2 接口**顶层只有汇总**;逐个积分包(带 `expires_at`)要么在
更深的层级里,要么得换一条端点。前一轮实现只认「顶层 `details[]`」和「一层 `data`
信封」,层级再深就一律认不出。

**这一轮的修法(不猜数据,只改找法)**:

1. **找包不看层级**:`parse_packs` 增一条兜底 —— 在整棵树里找"成组的包"
   (`core::qoderweb::collect_packs`,深度上限 4)。判据没放宽,仍是
   「剩余量 + 到期时刻同时在场」;而且要求**成组**(数组,或每个值都是包的对象映射),
   避免把带账户级到期时刻的 `plan_quota` 这类汇总对象误当成一个包。
2. **三档失败分开**:`PacksErr` 新增 `Unrecognized` —— "会话有效、但响应里找不到逐包
   数据"。它与 `Unauthorized`(该去重登)在界面和登录轮询里都要说不同的话。
3. **登录成功的定义**:会话被平台接受就算登录成功(关窗、记 `qoder_web_session_at`),
   只是额外带一句「积分明细接口的返回结构还没识别(已记入诊断)」。
   绝不再把结构问题报成"被平台拒绝"。
4. **诊断能定位**:失败时日志里落一份**结构草图**(`shape_sketch`:键名 + 值类型,
   不含任何值)。键名截到 31 字符 —— 诊断日志的兜底脱敏会把「≥32 字符的连续串」
   当令牌掩成 `«secret len=…»`,真机那个 32 字符的顶层键正好被它吃掉。

**结果(2026-11-03 18:53,重装 0.3.7 后真机验证)**:重新登录网页版后
`fetch_packs` 直接成功 —— 诊断日志里再没有 `qoder / webpacks` 失败行,
`settings.qoder_web_session_at` 写于 18:52:59。也就是说逐包数据**确实在 v2 响应的
更深层级**,递归找包(§4 第 1 条)就是缺的那一块;`CANDIDATE_PATHS` 与解析判据都不用改。
P0 到此收口。

顺带确认的一件事:网页会话比想象中短命 —— 同一份会话在 18:28 还是 200,
18:3x~18:46 就变成 401(应用没重启、没改配置)。所以"渠道卡提示重新登录"不是噪音,
是这份会话的常态;失败分档后,用户看到的正好是"点登录网页版"这一条正确动作。
