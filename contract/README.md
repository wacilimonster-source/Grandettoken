# Grandettoken Sync Protocol v1

本目录是桌面端（Rust）与 Android 端（Kotlin）共用的同步协议契约。任何一端改字段、
改加密参数、改合并规则，都必须同时更新本文件与 `fixtures/sync-v1.json`，并让两端测试
读同一份向量。

状态：**M0/M1 已实现**（`app/core/src/sync/`）。R1 评审中尚未实现的部分在文末列出。

---

## 1. 模型

- 一个同步码 = 一个同步组。
- 每台电脑是写入者：只写自己的来源文件，也读取组内全部来源。
- 手机只读：读取全部来源、解密、按 `platformId` 合并。
- 手机不展示设备名、设备列表或设备切换。
- 多台电脑限定同一个 GitHub 账号；每台电脑单独取得授权，同步码不授予 Gist 写权限。

---

## 2. 同步码

文本格式：

```
GT1.<base64url(payload || crc32_be)>
```

`base64url` 无填充（RFC 4648 §5）。

`payload` 固定 51 字节：

| 偏移 | 长度 | 字段 |
|---|---|---|
| 0 | 1 | `version`，当前 1 |
| 1 | 1 | `relayKind`，1 = secret gist |
| 2 | 1 | `keyEpoch`，初始 0 |
| 3 | 16 | `relayId`：Gist ID 的 32 位十六进制折成 16 字节 |
| 19 | 32 | `masterKey` |
| 51 | 4 | CRC32（IEEE 802.3，覆盖前 51 字节，大端） |

共 55 字节，文本约 78 字符。解码必须严格校验前缀、长度、CRC、版本与 relayKind。

`relayId` 是 Gist ID 本身；Gist ID 必须匹配 `^[0-9a-f]{32}$`。

---

## 3. 加密

| 项 | 取值 |
|---|---|
| 主密钥 | 同步码里的 32 字节 |
| HKDF | HKDF-SHA256 |
| HKDF salt | `SHA-256(UTF-8("grandettoken-sync-v1/salt"))`，32 字节 |
| HKDF info | `UTF-8("grandettoken-sync-v1/source-key|<relayIdHex>|<source>|<keyEpoch>")` |
| 来源子密钥 | HKDF 输出 32 字节 |
| 算法 | AES-256-GCM |
| nonce | 12 字节 CSPRNG（fixtures 用固定值） |
| tag | 16 字节，附在 ciphertext 后 |
| AAD | `UTF-8("v1|<source>|<keyEpoch>")`；`v1` 是密文外壳版本，不是载荷版本 |
| 密文外壳 | `{"v":1,"n":"<base64url(nonce)>","c":"<base64url(ciphertext||tag)>"}` |

解密失败统一返回 `SYNC_DECRYPT_FAILED`。外壳缺字段、版本不符、nonce 长度不对、
密文短于 tag 返回 `SYNC_ENVELOPE_INVALID`。

`AAD` 中的 `source` 必须是 `s_<32 hex>` 小写形式。`relayId` 通过 HKDF info 绑定，
把密文挪到另一个中继会派生不同子密钥，解密失败。

---

## 4. 文件布局与命名

```
gt-relay-<随机>（Secret Gist）
├── gt_s_<32 hex>.json   # 写入者 A
├── gt_s_<32 hex>.json   # 写入者 B
└── gt_s_<32 hex>.json   # 写入者 C
```

- `source`：`s_` + 128 位随机值的 32 位十六进制。
- 文件名：`gt_` + `source` + `.json`，即 `gt_s_<32 hex>.json`。
- 只处理严格匹配该格式的文件；其他文件（包括建组占位文件 `gt_init.txt`）忽略。
- 复制安装目录/配置后必须重新生成 `source`，否则两个写入者会抢同一文件。
- 每台电脑只 PATCH 自己的来源文件；请求体只含这一个文件，不整份回写 `files`。

---

## 5. 明文载荷

```json
{
  "v": 1,
  "source": "s_0123456789abcdef0123456789abcdef",
  "seq": 7,
  "at": 1793500931,
  "app": "0.3.2",
  "channels": [
    {
      "platformId": "deepseek",
      "id": "deepseek",
      "name": "DeepSeek",
      "kind": "money",
      "unit": "CNY",
      "remaining": "88",
      "total": null,
      "percent": null,
      "windows": [],
      "expiring": [],
      "day": "1.2",
      "week": "8.4",
      "month": "23.6",
      "estimated": true,
      "stale": false,
      "errorCode": null,
      "updatedAt": 1793500850,
      "history": [
        {
          "periodStart": 1793414400,
          "periodEnd": 1793500800,
          "timeZone": "+08:00",
          "unit": "CNY",
          "balance": null,
          "spent": "1.2",
          "estimated": true,
          "complete": true,
          "revisedAt": 1793500850
        }
      ]
    }
  ],
  "checkin": {
    "date": "2026-11-02",
    "timeZone": "Asia/Shanghai",
    "items": [
      {
        "itemId": "deepseek:main",
        "platformId": "deepseek",
        "status": "done",
        "observedAt": 1793500800
      }
    ],
    "done": 1,
    "total": 2,
    "unsupported": 0,
    "unknown": 0
  },
  "totals": {
    "cny": "88",
    "others": [{ "unit": "USD", "amount": "10" }],
    "today": "1.2"
  }
}
```

### 5.1 时间语义

- `at`：发布时刻。心跳会变，**不**代表余额新鲜度。
- `updatedAt`：该渠道最后一次成功取数。合并只看它。
- `receivedAt`：读取方收到时刻，不进入载荷，不刷新 `updatedAt`。
- 304 Not Modified 不刷新任何观测时间。
- `stale`：上游失败或观测超时。手机网络失败是读取方的状态，不写进载荷。

### 5.2 数值口径

- 所有金额/积分/百分比都是**十进制定点字符串**，不是 JSON number。
- 允许可选的负号、最多 6 位小数、最多 24 字符；禁止指数、`NaN`、`Infinity`。
- 缺失一律 `null`，绝不补 0。
- `kind`：`money` / `quota` / `points`。
- `unit`：`CNY` / `USD` / `credits` / `%` 等，最多 16 字符；金额按 `unit` 分组。
- `percent` 只用于 `quota`；金额与百分比不能相加。
- 积分、配额不参与金额合计。

### 5.3 platformId

- 内置平台：协议枚举，等于 provider id（`deepseek` / `trae` / `workbuddy-ai` ...）。
- 自定义平台：`custom:<统一同步平台标签>`。标签小写、空白转 `-`、只留 `[a-z0-9.-]`，
  最多 40 字符；改显示名不改变身份。
- 没有标签时退回 `custom:<本机渠道 id>`（稳定但只在当前电脑有意义），UI 必须提示用户设置标签。
- 严禁把 URL、凭据指纹、邮箱或显示名直接当 `platformId`。

### 5.4 history

- 最近 7 个自然日，按电脑本地日切分；`timeZone` 是偏移字符串（如 `+08:00`）。
- 每条包含 `periodStart`、`periodEnd`、`unit`、`balance`、`spent`、`estimated`、
  `complete`、`revisedAt`。
- `balance` = 当天**最后一条成功快照**的余额；没有观测就是 `null`。
- `spent`：今天用电脑端的实时 `day`（今天到目前为止）；历史日从**明细快照**按
  与桌面 `day/week/month` 相同的余额差规则计算。缺口日给 `null`，不拿部分数据
  冒充全天，也不补 0。`daily_summary` 是 90 天归档后的长期底料，不能拿来算最近一周。
- `complete`：`false` = 当天采样有缺口；手机据此把曲线断开/标注。
  缺省 `true`，兼容旧载荷与旧 fixture。
- `revisedAt`：真实最后观测时间；没有观测时用当天零点，**绝不用读取时刻**，
  否则一条空历史会在跨来源合并时压过另一台电脑的真实记录。
- 按**当前 key 指纹**分段：同一渠道换 key 后只发布当前账户的历史，不把两个账户
  的余额差拼成消费。多本账但取不到当前指纹时整段历史为空，不猜。
- 合并时按 `(platformId, periodStart, periodEnd, unit)` 去重；同窗口取最大 `revisedAt`，
  同刻按 `source` 字典序；不跨来源求和或拼接成虚假连续记录。

### 5.5 checkin

- `date` 为 UTC+8 业务日；`timeZone` 固定 `Asia/Shanghai`。
- `itemId` 可跨来源去重。v1 每渠道一份凭据，使用 `<platformId>:main`；
  多账号计数口径待 M0 设计任务确认。
- `status`：`done` / `pending` / `unsupported` / `unknown`。
- `total = done + pending`；`unsupported` / `unknown` 不计入分母。
- **不得**拿任意一个来源的 `done`/`total` 冒充全组统计；手机必须按合并后的 items 重算。

---

## 6. 合并

1. 先按来源去重：同一 `source` 保留 `seq` 更大者；同 `seq` 同密文幂等；同 `seq` 不同密文
   返回 `SYNC_CONFLICT`。
2. 渠道按 `platformId` 分组，取 `updatedAt` 最大的一条完整记录。
3. 同刻按 `source` 字典序（小者胜）确定性选取；**不跨来源拼接字段**。
4. 最新一条是 `stale` 就带着 stale 标记展示，不退回更旧的观测。
5. `totals` 从合并后的渠道行重算，绝不累加各来源的 `totals`。
6. `history` 按窗口去重；`checkin` 按 `itemId` 去重并重算。
7. 解密失败或载荷不合法的来源跳过，其余正常合并；诊断里报告来源 ID 与错误码。

---

## 7. 序号与幂等

- 每个来源维护 `seq` 水位 `{seq, envelopeHash}`。
- `seq` 更小：忽略。
- `seq` 相同且密文哈希相同：幂等，忽略。
- `seq` 相同且密文哈希不同：`SYNC_CONFLICT`。
- 发布侧先把 `seq` 与本次密文持久化，再发送；超时重试必须复用原密文。
- 新安装没有历史水位，不能承诺识别所有回放。

---

## 8. 资源上限与截断

| 项 | 上限 |
|---|---|
| 来源文件数 | 32 |
| 单文件 | 256 KB |
| 响应总大小 | 4 MB（解析前 8 MB 硬上限） |
| 渠道数 | 64 |
| 每渠道 history | 7 |
| 每渠道 windows | 16 |
| 每渠道 expiring | 64 |
| 签到 items | 64 |
| 其他币种 | 16 |

- 必须检查 Gist 顶层 `truncated` 与文件级 `truncated`；任一为真 → `SYNC_TRUNCATED`。
- 超限 → `SYNC_TOO_LARGE`；不更新为“完整视图”。
- 只处理匹配命名规则的文件。
- 404 只表示不存在或不可访问，不能断言“已删除”。
- 错误信息不包含响应体、URL 查询串、Authorization 或账号信息。

---

## 9. GitHub 传输

- API：`https://api.github.com/gists/{id}`。
- 头：`Accept: application/vnd.github+json`、`X-GitHub-Api-Version: 2022-11-28`、
  `User-Agent: Grandettoken-Sync/1`。
- 条件读取：`If-None-Match: <etag>`；304 不刷新观测时间。
- 写：`PATCH /gists/{id}`，body `{"files":{"gt_s_<hex>.json":{"content":"<envelope>"}}}`。
- 建组：先 `POST /gists` 建一个只含 `gt_init.txt` 的 secret gist；拿到 ID 后派生来源子密钥，
  `PATCH` 真正的来源文件，再尽力删除占位文件。
- 状态映射：
  - 401 → `SYNC_AUTH`
  - 403 + `x-ratelimit-remaining: 0` 或 `Retry-After` → `SYNC_RATE_LIMITED`
  - 403 其他 → `SYNC_FORBIDDEN`
  - 404 → `SYNC_NOT_FOUND`
  - 409 → `SYNC_CONFLICT`
  - 422 → `SYNC_PAYLOAD_INVALID`
  - 429 → `SYNC_RATE_LIMITED`
  - 5xx → `SYNC_UPSTREAM`
- 遵守 `Retry-After`、`X-RateLimit-Reset`；403 不一律视为限流。

### 9.1 桌面 GitHub Device Flow（不手填 PAT）

- 独立桌面 OAuth App，启用 Device Flow，scope 仅 `gist`；程序只内置公开 `client_id`，
  不含 `client_secret`。client_id 通过编译期 `GRANDETTOKEN_DESKTOP_CLIENT_ID`
  或配置 `syncOauthClientId` 提供。
- `POST https://github.com/login/device/code`（form：`client_id`、`scope=gist`）返回
  `device_code`、`user_code`、`verification_uri`、`expires_in`、`interval`。
  `device_code` 只留 Rust 内存，前端只拿到 `user_code` 与 `verification_uri`。
- 用户点击后复制 `user_code`，通过系统浏览器打开 `https://github.com/login/device`；
  程序不收 GitHub 密码、不用内嵌网页。
- 按 `interval` 轮询 token endpoint；`authorization_pending` 继续，`slow_down` 降速，
  `access_denied` / `expired_token` / `device_flow_disabled` 停止并给重试入口。
- 成功后校验实际 scope **恰好为 gist**；`GET /user` 取 `id`、`login`；
  token 存 Windows 凭据管理器 `TokenScope:sync`（账号 `oauth`），
  `login`/`id` 存配置用于展示。任何 token / device_code 不进前端状态、日志或诊断。
- 创建/加入前必须已授权；加入时校验 Gist `owner.id` 等于当前授权账号 ID，
  不同账号拒绝加入，不另建空组冒充成功。
- 401 停止自动发布，保留组与缓存，提示重新授权；403 区分权限与限流，429 退避。
- 断开连接：删除本机 OAuth token、清除登录状态、暂停发布，**保留组与主密钥**；
  单独的“退出同步组”才删除本机组密钥。远端撤销走 GitHub 官方授权管理页。
- 桌面同步设置单独保存在 SQLite 的 `sync:profile:v1`，升级时从旧配置迁移一次；
  通用设置或后台任务写入旧配置不会覆盖它。同步主密钥仍只存在 Windows 凭据管理器，
  同步码由主密钥、中继 ID 和密钥代数在本机还原。读取错误不等于未创建同步组。
- 找回旧组时先验证当前 GitHub 账号和至少一份密文，确认后切换并暂停自动发布，
  不删除原组。仅在本机发布水位与远端密文能唯一对应时恢复原发布来源；不能确认时
  保留同步码但阻止发布。自动搜索覆盖本机保留的 v1 初始代数，其他代数可输入旧码。
- 打开浏览器只允许 `https://github.com/login/device`（`open_url` 白名单）。

---

## 10. 手机可选 GitHub 登录（未实现）

- 匿名：前台 5 分钟一次；后台 15 / 30 / 60 分钟尽力调度。Android 端 v0.1 只实现匿名模式。
- 登录：Device Flow，单独注册手机专用 OAuth App，APK 只含公开 `client_id`；
  不请求 gist/repo/user/email scope，只读公开可访问的 Secret Gist。
- 登录后前台默认 60 秒；401 停止使用 token、保留缓存、降为匿名策略。
- token 用 Android Keystore 包装密钥加密后存应用私有目录；退出登录删 token、
  取消旧任务并重新调度匿名任务，不退出同步组。
- 必须先扫描同步码并验证至少一份有效密文，才原子保存新组；空组不能证明密钥正确。

---

## 11. 轮换

- 创建新 Gist + 新主密钥，先发布并验证，再切换本机。
- 所有其他电脑和手机必须重新加入；失败保留原配置。
- 本机转而写入新中继；其他电脑若尚未迁移，仍可能继续写旧中继。删除旧中继单独确认。
- 无法远程清除旧手机缓存，也无法保证旧手机知道新组存在。
- 新组创建后若其他电脑仍写旧组，必须提示“迁移未完成”。

---

## 12. 错误码

| 码 | 含义 |
|---|---|
| `SYNC_CODE_INVALID` | 同步码格式/CRC/长度错误 |
| `SYNC_ENVELOPE_INVALID` | 密文外壳非法 |
| `SYNC_DECRYPT_FAILED` | 密钥/AAD/内容校验失败 |
| `SYNC_PAYLOAD_INVALID` | 载荷字段或 JSON 不合法 |
| `SYNC_UNSUPPORTED_VERSION` | 主版本不支持 |
| `SYNC_TRUNCATED` | Gist 或文件被截断 |
| `SYNC_TOO_LARGE` | 超过资源上限 |
| `SYNC_RATE_LIMITED` | GitHub 限流 |
| `SYNC_AUTH` | 401 |
| `SYNC_FORBIDDEN` | 403（非额度） |
| `SYNC_NOT_FOUND` | 404 |
| `SYNC_CONFLICT` | seq 冲突 / 并发写 |
| `SYNC_NETWORK` | 网络/超时/连接失败 |
| `SYNC_UPSTREAM` | GitHub 5xx 或其他状态 |
| `SYNC_NOT_CONFIGURED` | 未建组/未启用/OAuth App 未配置 |
| `SYNC_NO_KEY` | 缺少主密钥或 GitHub 授权 |
| `SYNC_ACCOUNT_MISMATCH` | 加入的 Gist 不属于当前 GitHub 账号 |
| `SYNC_OAUTH_EXPIRED` | Device Flow 授权码过期 |
| `SYNC_OAUTH_DENIED` | 用户拒绝授权 |
| `SYNC_SCOPE_INVALID` | 授权范围不是仅 gist |
| `SYNC_KEY_INVALID` | Android Keystore 包装密钥失效，需要重新配对 |
| `SYNC_GIST_ID_INVALID` | Gist ID 格式错误 |
| `SYNC_SOURCE_INVALID` | source 格式错误 |
| `SYNC_STORE` | 本地状态/凭据读写失败 |

---

## 13. Fixtures

`fixtures/sync-v1.json` 由：

```bash
cd app/core
cargo run --example gen_sync_fixture > ../../contract/fixtures/sync-v1.json
```

生成。包含固定 masterKey、relayId、source、nonce、AAD、derivedKey、plaintext、
envelope、syncCode 与预期失败码。

Rust 测试：`app/core/tests/sync_fixtures.rs`。
Android 测试必须读取同一文件，至少验证：

1. HKDF 派生 key 与 `derivedKeyHex` 一致；
2. AAD 字节与 `aadHex` 一致；
3. AES-GCM 能解出 `plaintextJson`；
4. 同步码解码回 masterKey / relayId / keyEpoch；
5. 错误 key、错误 AAD、篡改、截断分别得到预期错误码；
6. 用同一 nonce 重新加密得到同一 envelope。

---

## 14. R1 中尚未实现的部分

- 手机 GitHub Device Flow 与 token 生命周期（Android v0.1 仅匿名）。
- 桌面 Device Flow 的**真实 OAuth App 注册与线上实测**（代码与 UI 已实现；
  client_id 未配置时界面明确显示“OAuth App 未配置”，不会假装已连接）。
- 被隐藏渠道的历史清理确认与旧缓存缺失提示。
- 多账号签到的最终计数口径（当前每渠道 `:main` 一项）。
- Gist `raw_url` 回退（当前遇到 `truncated` 直接报错，不回退）。
- Android 检查更新：设计见设计文档第 14 节；签名清单、长期 APK 签名密钥、
  下载/校验/安装引导尚未实现。

---

## 15. Android 检查更新（设计要点）

- 通过本项目 GitHub Releases 分发 APK；默认每天自动检查一次，只提示，不自动下载。
- 仅 `versionCode` 更大且满足系统版本、ABI、稳定渠道的版本可推荐；不降级。
- 先下载独立签名的更新清单（内置 Android 清单验签公钥），再下载 APK；
  SHA-256 只证明文件与清单一致，不能替代 APK 签名与清单验签。
- 下载由用户点击触发；移动数据需确认；显示进度与取消；校验包名、证书、版本、ABI。
- 安装前检查 `canRequestPackageInstalls()`，引导用户到系统“允许来自此来源的应用”，
  返回后重查，不循环拉起权限页。
- 交给系统安装器不等于安装成功；下次启动核对实际 `versionCode`，清理旧 APK，
  保留同步组、密钥、GitHub 登录与通知设置。
- 检查失败不得报告“已是最新”；保留最近可信状态；诊断只含时间、阶段、版本与错误码。
- 读取 GitHub 资源时校验官方资源域名白名单，不把 OAuth token、同步码或
  `Authorization` 传给下载/重定向链路。
