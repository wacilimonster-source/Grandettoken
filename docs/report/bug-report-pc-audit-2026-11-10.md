# Bug 报告 · PC 端全量代码排查（2026-11-10）

> 范围：PC 端全部代码 —— 前端 `app.js`（9051 行）/ `index.html` / `styles.css`，
> Rust 后端 `lib.rs` / `gateway.rs` / `sync_cmds.rs` / `download.rs`，
> 以及 `tauri.conf.json` / `capabilities` 权限配置。基线版本 v0.3.29。
> 方法：四路并行全量通读 + 关键发现逐条回读源码二次核实（关键项均已人工复核，非运行时复现）。
> 结论：**1 个高危、6 个中危、16 项低危**。高危是「改任意设置会把自定义渠道删掉」。
> 加密同步、更新链路、签到时区/幂等、密钥防泄漏经重点核查**未发现问题**。
>
> **修复状态（2026-11-10 更新）**：23 项已全部修复，随 v0.3.30 发布；
> 门禁全绿（core 421 / src-tauri 14 / jsdom 177 / CSS 形态门 PASS）。

## 一、问题总览

| # | 级别 | 问题 | 位置 |
|---|---|---|---|
| B1 | **高** | 改任意设置会把自定义渠道整份删掉/复活 | lib.rs:355 |
| B2 | 中 | 锁序违反 → 可能 AB-BA 死锁，挂件整体冻死 | lib.rs:4278/4292 |
| B3 | 中 | 自定义渠道的「复制/显示密钥」必报"未知渠道" | lib.rs:5475 |
| B4 | 中 | 签到失败路径整份回写陈旧配置，可回退用户设置 | lib.rs:4356-4376 等 |
| B5 | 中 | 底栏计数错位：预警算"正常"、告急标"预警" | app.js:2204-2218 |
| B6 | 中 | Qoder 签到「去登录」指去开 Trae | app.js:5597-5605 |
| B7 | 中 | 网关 /chat/completions 无读超时，上游挂起连接悬挂 | gateway.rs:284-297 |

低危 16 项见「四、低危一览」。

---

## 二、高危

### B1：改任意设置会把自定义渠道整份覆盖删除（或"复活"）

**现状（修复前）**
- 用户添加自定义渠道后，**任何一次**改设置（保存取数间隔 / 主题 / 快捷键 / 紧凑条 / 隐藏渠道……）都会把刚加的渠道定义删掉；密钥还留在 Windows 凭据管理器里成为孤儿。
- 反向同样成立：删除自定义渠道后，下一次改设置会把已删的渠道"复活"（密钥已清，显示"未配置"）。

**根因**
- `set_config`（lib.rs:272-365）对后端单方面推进的字段做了逐个保护（`skipped_version` / `win_*` / `checkin` 等），**唯独漏了 `custom_channels`**，第 355 行 `*c = next` 整份替换。
- 前端 `CFG = await invoke("get_config")` 只在启动时调一次（app.js:8731），`custom_add` / `custom_remove` 成功后**不刷新快照**；于是之后每次 `set_config { config: CFG }` 都把开机那一刻的旧渠道清单整份写回。
- 这正是本项目 P1-3 / P1-13 反复修过的同一类坑，`custom_channels` 是漏网的一个。

**修复方案**
- 后端把 `custom_channels` 也纳入保护：`custom_add` / `custom_remove` 是改它的唯一入口，前端永远不需要提交它，直接 `incoming.custom_channels = cur.custom_channels.clone()`（与 `diag_verbose` 同款写法，一行）。
- 可选加固：前端在 custom_add/remove 成功后重新 `get_config` 同步快照，防止未来再出现同类漏保护字段。

**修复后效果**
- 添加 / 删除自定义渠道不再被任何一次设置保存冲掉；凭据管理器不再产生孤儿密钥。

---

## 三、中危

### B2：`settle_arrivals` 锁序违反，与 `check_update` 构成 AB-BA 死锁

**现状（修复前）**
- 窗口窄但后果重：一旦命中，全部 IPC 与轮询永久阻塞，挂件无提示地整体冻死（release 是 `panic="abort"`，不会自愈）。

**根因**
- lib.rs:1705-1706 明文约定全局锁序「**先 store 后 config**」；`check_update`（lib.rs:2550-2552）按 store→config 加锁。
- `settle_arrivals`（lib.rs:4277-4296）是唯一违反者：持 config 锁的循环体内逐渠道再拿 store 锁（4292 行，config→store）。它挂在每轮取数之后，只要存在"待落定的到账核对单"就会执行；`check_update` 是前端周期性命令，两者在 tokio 多 worker 上可真并发 → 互相等对方手里的锁。

**修复方案**
- `settle_arrivals` 在 config 锁内只克隆待核对清单 `Vec<(id, ArrivalInfo)>`，释放 config 锁后再逐个读 store；写回段（4307-4335）同样改为短临界区，不嵌套两把锁。

**修复后效果**
- 锁序约定全文件无违反者；理论死锁路径消除。

### B3：自定义渠道的「复制 / 显示密钥」按钮必然报"未知渠道"

**现状（修复前）**
- 自定义渠道卡上明明渲染了「复制 / 显示」按钮（app.js:3278-3281，条件 `hasKey && authSource==="keyring"` 对自定义渠道成立），点击却 toast「取密钥失败：未知渠道： custom-N」。功能 100% 复现。

**根因**
- `key_reveal`（lib.rs:5475）用 `providers::find(&id)` 校验，该函数只扫静态渠道表，`custom-N` 永远找不到 → 直接 Err；`key_copy`（5493）内部调 `key_reveal`，同样失败。
- 同文件的 `key_allowed`（410-419）专门为自定义渠道放了行，`set_key` / `delete_key` 都正确处理，唯独这两个命令漏了；`key_hint` 对自定义渠道返回 `Ok(None)`，尾号也回显不了。

**修复方案**
- `key_reveal` / `key_copy` 改用 `key_allowed` 同款判定：自定义渠道直接走 `secrets::channel_key(id)`；`key_hint` 对自定义 id 也做尾 4 位回显。

**修复后效果**
- 自定义渠道的密钥复制、显示（8 秒收回）、尾号回显与内置渠道行为一致。

### B4：签到失败路径整份回写陈旧配置，可把用户设置倒回旧值

**现状（修复前）**
- 节拍器每次签到失败（每小时都可能）调用 `record_checkin_skip`：克隆整份配置 → 持 store 锁写流水（期间 config 锁空闲）→ 第 4376 行 `*lock_rw(&state.config) = cfg` 把**陈旧克隆**整份写回内存和磁盘。此窗口内用户保存的设置（主题 / 间隔 / 隐藏渠道……）被静默回退，下次任何配置保存会把回退固化。
- `wb_finish_login`（862 行）同款；另有 5 处（`run_checkin` 收尾 / `settle_arrivals` 写段 / `inject_state` / `check_update`）在 `CFG_WRITE` 串行闸之外落盘，与闸内写者可乱序 —— 正是 P3-35 注释里描述过、修过一半的形态。

**根因**
- 项目为防"后改的先落库"建了 `CFG_WRITE` 闸（lib.rs:164-166），但只有命令路径走 `save_config_off_main`；后台路径各自为政，其中 `record_checkin_skip` / `wb_finish_login` 还多了"整份回写内存"这一步。

**修复方案**
- 全部改走 `save_config_off_main`；`record_checkin_skip` / `wb_finish_login` 照 `run_checkin` 收尾（4238-4248）已示范的写法：读 live 配置、只合并自己那条 entry，删除整份回写。

**修复后效果**
- 配置落盘全局串行、内存与磁盘一致；签到失败与用户改设置并发时，用户设置不再丢失。

### B5：底栏三段计数与行内状态语义错位

**现状（修复前）**
- 底栏「N 正常 · N 预警 · N 告急」：阈值黄色（预警）的渠道**不落在任何计数里，被归入"正常"**；跌破红线（行内红点"告急"）的渠道在底栏却被黄色点标成"预警"。只看底栏的用户会漏掉预警渠道，颜色梯度倒挂。

**根因**
- app.js:2205 的 `warn` 过滤条件写成 `hvTone(c) === "bad"`（红线）而非 `"warn"`；行内同一套约定（`TONE_WORD`，app.js:938）里 "bad" 是红点告急、"warn" 是黄点预警，与函数注释"与行内同一套约定"自相矛盾。

**修复方案**
- 按行内同源口径改：`warn` 计数取 `hvTone(c)==="warn"`，红线与限流计入 `bad`（可与"取数失败"合并或分开，二选一），保持三段互斥（`normalN = 总数 - bad - warn`）。

**修复后效果**
- 底栏与行内状态一一对应：黄=预警、红=告急，预警渠道不再隐身。

### B6：Qoder 签到「去登录」按钮指去开 Trae

**现状（修复前）**
- Qoder / Qoder 国内版凭据失效时，签到卡给出「去登录」按钮，点击后提示「打开一次 Trae 即可」—— 对 Qoder 毫无作用。4 个可签到渠道里 2 个被误导。

**根因**
- app.js:5597-5605 的 `cklogin` 分支只写了 `workbuddy` 一个 case，其余全落到 Trae 文案；而后端 `checkin_credentials`（lib.rs:3320）明确 Qoder 凭据来自 Qoder 桌面端，自己的错误文案是「打开一次 Qoder 桌面端即可」。

**修复方案**
- 按 id 分支：`qoder` / `qoder-cn` → 「打开一次 Qoder 桌面端即可」；`workbuddy` → 保持 `workbuddy_login`；其余保持 Trae 文案。

**修复后效果**
- 签到失败路径的指引与后端错误文案一致，四个渠道各指各的路。

### B7：网关 `/chat/completions` 上游请求无读超时

**现状（修复前）**
- 上游完成握手后若不返回响应头或不结束流（半开代理、假死上游），网关 handler 永久悬挂，局域网调用方连接堆积；只能重启挂件恢复。

**根因**
- `chat()`（gateway.rs:284-297）的 `send().await` 与 `bytes_stream()` 无任何超时；代理客户端只设了 15s 连接超时（proxy.rs:74-76）。同文件 `gateway_publish` 显式加了 `.timeout(25s)`（483 行）——"该有超时"是项目共识，唯独这条生产主路径缺失。

**修复方案**
- 用 reqwest 的 `read_timeout`（空闲读超时，对流式 SSE 友好）构建客户端；`send()` 阶段再包一层 `tokio::time::timeout` 约束响应头到达。

**修复后效果**
- 上游挂起时网关在有限时间内返回 504/502，连接可释放，不再堆积。

---

## 四、低危一览

| # | 问题 | 位置 | 修法（→ 效果） |
|---|---|---|---|
| L1 | 详情弹层后台重取「最近变动」失败时清空已有缓存，正在展示的台账被换成错误态 | app.js:4349-4361 | 失败时保留缓存仅标 error（→ 刷新失败不再丢旧数据） |
| L2 | 模型「N 个可用」：usableCount=0 被 `||` 当成未知，误报成全部模型数 | app.js:1562-1565 | 与 8177 行对齐，0 照写（→ 不再虚报可用数） |
| L3 | 配额窗口百分比未套「用掉还是剩余」翻转口径，与主数字并排自相矛盾 | app.js:1102 | 输出走 `dispPct` 或固定加「剩」字样 |
| L4 | 临期 7 柱图窗口横跨 8 个自然日，第 8 日的到期漏柱、图与合计不符；「最先到期」今天到期显示"剩 1 天" | app.js:2004-2008 / 2067 | 柱与摘要统一为自然日口径（→ 图与数字一致、同事实同一文案） |
| L5 | 签到徽标「10:00 后可签」用本地时区小时对比北京时间的刷新点 | app.js:7487-7492 | 换北京小时判断（→ 非 UTC+8 机器提示时刻正确） |
| L6 | Esc 双监听：一次按键同时关优惠弹层又把面板收成胶囊 | app.js:8978 + 6818 | promo 处理器 `stopImmediatePropagation`（→ 一次 Esc 一个动作） |
| L7 | `checkin_state` 读失败静默清空签到状态（徽标/提示条整体消失）；历史读失败永远停在"正在读取" | app.js:7555 / 7715 | 照 loadVault F3 纪律：失败保留旧值 + 错误态 + 重试入口 |
| L8 | `channels-updated` / `crit-alert` 等监听挂在首轮 refresh 之后，首检预警音可能丢 | app.js:8849-8896 | 移到 `await refresh()` 之前（与 checkin-* 一致） |
| L9 | 凭据管理器读错误被吞成"还没填密钥"，P2-15 回归点（3 处） | lib.rs:2117 / 2234 / 4795 | 区分 Err 与 Ok(None)（→ 凭据库故障如实报错） |
| L10 | 节拍器对"正在签到/开关没开"等互斥拒绝也记 `no_cred` 流水并推进退避 | lib.rs:4691-4698 | 套用手动入口同款 gate_like 过滤 |
| L11 | 昨日遗留的 `activity_state` 绕过当天门控，多打一次请求且 `last_credits` 展示丢失 | lib.rs:61 / 4445 / 4676 vs 3429 | 三处按 `period == today` 门控（与 checkin_state_of 一致） |
| L12 | `custom_add` 的 id 分配在锁外 push，并发可分到重复 id | lib.rs:2032-2055 | id 分配与 push 收进同一临界区 |
| L13 | 诊断体检在配置损坏回落默认值时，对"隐藏=不发请求"的渠道真打请求 | lib.rs:2177-2180 | 体检改读 `state.config`，CONFIG_BROKEN 时跳过或明示 |
| L14 | 同步命令三处不一致：手动发布缺配置损坏闸；断开 GitHub 先删后存、失败留不一致；无 token 匿名请求报误导 404 | sync_cmds.rs:984 / 1229 / 997 | 补 `refuse_if_config_broken`、调换顺序、改 `github_token()?` |
| L15 | 网关丢弃上游 4xx/5xx 错误体与 Retry-After，错误不可诊断；无鉴权本机模式可被 DNS rebinding 读 /v1/models | gateway.rs:293 / 163 | 透传截断后的上游 body 与 Retry-After；校验 Host 头 |
| L16 | `Gateway::new` 的 `expect` 在 TLS 初始化失败时启动期 panic 且不写 startup-error.log | gateway.rs:86 | 改 Result / 懒初始化，失败进 startup_error |

---

## 五、排查过、确认无问题的方面

- **前后端接口**：前端全部 84 处 `invoke` 的参数名/类型与 Rust `#[tauri::command]` 签名逐一匹配；事件名与 payload 字段一致。
- **加密同步**：每次 seal 独立随机 nonce，无复用路径；HKDF 用 RFC 5869 官方向量锁定；AES-256-GCM 带 AAD，错 key/错 source 均有测试；明文不进 gist。
- **更新链路**：下载 URL 严格白名单 + minisign 公钥 + `latest.json` 端点配置正确，无路径注入。
- **签到时区/幂等**：统一固定 UTC+8；重复领取有"回查证据 + ready 才发 claim + 互斥"三重防护，未发现真实重复签到路径。
- **密钥防泄漏**：`diag_export` 过 `redact`；日志与流水不含凭据；明文回前端仅 `key_reveal` / `vault_reveal` 两条显式路径。
- **配置落库**：单行 SQLite UPSERT（WAL），断电/崩溃不产生半份配置。
- **前端壳**：index.html 无重复 id；CSP 无 `unsafe-inline` 脚本；capabilities 无过宽权限。

## 六、修复优先级建议

1. **下个版本必须修**：B1（用户可感知的数据丢失）。
2. **同批修复**：B2 / B3 / B4（后端正确性，改动集中在 lib.rs）。
3. **随后一批**：B5 / B6 / B7 + 低危 L1-L8（前端交互与网关健壮性）。
4. **低危批量清理**：L9-L16。

> 备注：B1 / B2 建议修复后做真机复现验证（B1：加自定义渠道 → 改主题 → 重启确认仍在；B2：并发触发 check_update 与带核对单的取数，确认无冻结）。
