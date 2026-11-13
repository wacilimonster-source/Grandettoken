# Codex 重置顾问 · 详细设计方案

> 2026-11-13 · 状态：**已评审并修正**，算例与边界裁决见 `docs/report/codex-reset-review-2026-11-13.md`。
> 关联代码：`app/core/src/codex.rs`（重置卡只读元数据）、`app/core/src/providers.rs::extract_codex`（周窗模型）、`app/src/app.js::coreCodexResets`（重置卡区块）、`docs/product/product-codex-weekly-reset-2026-11-03.md`（周窗倒计时）
> 关联外部口径：OpenAI Help Center（见 §12）

---

## 0. 结论先行

### 0.1 用户当前这一手该怎么打

| 项 | 值 |
|---|---|
| 现在 | 2026-11-13 12:41 CST |
| 周额度剩余 | 0.0%（已限流） |
| 下次自然重置 | 约 2026-11-13 17:56（接口快照；按 6 小时估算约 18:40） |
| 订阅到期 | 2026-11-29（按当日 23:59 计，约 16.5 天） |
| 重置卡 | 3 张 |
| 之后的自然重置 | 11-20、11-27（各 +7 天） |

**今晚不要用卡。** 现在用一张卡，只能把「本来 6 小时后免费给你的满额」提前 6 小时；按本周期平均消耗 14.8%/天算，这 6 小时大约值 **3.7% 个周窗**，一张卡约 96% 的价值被烧掉。

**3 张卡不是按次数平均用，而是按真实速度用。** 本周期平均只有 14.8%/天（100% 撑了 6.75 天），这个速度下到 11-29 只差约 **11% 个周窗**：省着用是 **0 张**，想全程不断也只要 **1 张**，而且这一张只换回 7%–11%，并不划算。如果你实际会按 **20%/天** 推进（一周 5 天打满），才需要 **3 张**；到 **30%/天**，3 张也不够（约差 94%）。所以第一周先按「省卡」跑，用真实速度校准，再决定动几张。

### 0.2 合算的用法（四条）

1. **距自然重置 < 24 小时：等。** 这时用卡买到的只是不到一天的使用时间。
2. **周额度耗尽、且距自然重置 ≥ 2 天：可以用 1 张。** 按 14.8%/天，2 天约换回 30% 个周窗，刚好过「值一张卡」的 25% 门槛；3 天则约 44%。
3. **周额度满额（≥99%）：默认绝对不要点重置。** 官方口径是「只把低于 100% 的额度恢复到 100%，已经是 100% 的那一项不会重新开始计时」；此时点重置可能只刷新 5 小时窗，却照样消耗一张卡。**唯一的例外**：你正被 5 小时窗限流、且马上要干活——那是 5h 窗的取舍，本顾问不替它做决定，文案会把两种满额态分开说。
4. **11-27 之后只剩约 2.2 天：只在 11-28–29 有截止任务时用。** 否则卡会随订阅一起作废——但「不用」不亏，强行用只会换回一两天。

> 注：用卡会**改写每周重置日**（官方：刷新 5 小时与每周周期，并更改每周重置日期）。重置后的准确锚点以 Codex「设置 → 用量」为准；本工具在下次刷新时以服务端 `resetsAt` 重算，不缓存旧日期。

> 数据校准：本机快照 `resetsAt=2026-11-13T09:56:52Z`，即 **17:56:52 CST（约 5.3 小时）**；正文按用户提问时的「6 小时」取整。按 5.3h 重算 gain 为 **3.25%**，「约 96% 价值被烧掉」仍成立（96.75%）。工具不做取整，直接用实时 `resetsAt`。

---

## 1. 为什么要做这个功能

现有 Codex 详情卡已经显示：周窗剩余、下次重置、重置卡数量与到期日。但它只回答「是什么」，不回答「现在该不该用」。用户面对的是三个相互牵制的量：

- 剩余额度（用了多少、还能撑多久）
- 下次自然重置（等多久可以免费满额）
- 订阅到期（卡的有效生命还剩多长）

任何两个都算不出结论，三个一起才能。现有规划里只有「重置前 N 小时的一次性提醒」（`product-codex-weekly-reset` §7 P2），那是**通知**，不是**决策**；本方案补的是决策，且明确不做通知。

### 1.1 目标

1. 在 Codex 详情卡里，用一句话回答「现在要不要用重置卡」。
2. 给出可核对的数字：用卡能换回多少、自然重置还剩多久、到订阅到期还差几张。
3. 允许用户用一次点击声明「现在要用 / 24 小时内要用 / 能等到下次重置 / 暂时不用」，建议随答案变化。
4. 数据缺失、旧快照、刚用过卡时**不撒谎**：给「无法判断」，不给假精确。

### 1.2 非目标

- **不自动兑换、不调用兑换接口。** `codex.rs` 的既有边界是只读；兑换是不可逆的平台侧动作，产品定位也明确「不代替用户执行平台侧外部动作」（DESIGN §1.4）。
- **不看 5 小时窗做主判断。** 用户明确只看周额度；5 小时窗只在「满额误点」警告里提一句。
- **不做通知/声音/托盘提醒。** 与「不打扰」冲突；已有的红线告警不扩展。
- **不建第二套重置卡数据。** 数量、明细、最近用卡时间全部复用 `ResetCredits`。
- **不预测平台侧未来政策。** 套餐窗口结构会变（2026-07 曾取消又恢复 5 小时窗），代码按服务端字段走。

---

## 2. 官方机制（决定算法形状的四条）

| # | 口径 | 对算法的含义 |
|---|---|---|
| 1 | 一次完整的储存重置会刷新 5 小时和每周用量周期，并**更改每周重置日期**（Plus / Business Standard；Pro 100/200/500 只重置周窗；Go 是月窗） | 用卡不是「加 100%」，而是**把未来的自然重置提前到现在**；卡的价值 = 它提前打开的这段时间里你能消耗的额度 |
| 2 | 只有低于 100% 的额度会恢复到 100%；已经是 100% 的那一项不会重新开始计时 | **满额保护是最高优先级**；周窗满额时点重置可能只刷新 5 小时窗却消耗卡 |
| 3 | 至少恢复了一项额度，这次重置才算用掉 | 判断「用没用掉」不能只看周窗数字；`lastRedeemedAt` 是更可靠的信号 |
| 4 | 付费重置的下一周重置时间 = 重置生效后首次请求的 7 天后 | 卡后锚点不是简单 `now+7d`；工具以服务端下次刷新的 `resetsAt` 为准，不用本地推算锁死 |

> 这四条推出一个反直觉但重要的结论：**卡的「面值」不是 100% 个周窗，而是「你本来会被卡住的那段时间」× 你的消耗速度。** 距自然重置越近，面值越低；满额时面值为 0。

---

## 3. 用户当前账本（算例）

### 3.1 时间轴

| 时刻 | 事件 | 可用周窗 |
|---|---|---|
| 11-13 12:41 | 现在，周窗 0% | 0 |
| 11-13 17:56 | 自然重置 #1（接口快照） | 1.00 |
| 11-20 17:56 | 自然重置 #2 | 1.00 |
| 11-27 17:56 | 自然重置 #3 | 0.32（距到期只剩约 2.2 天） |
| 11-29 23:59 | 订阅到期 | — |

自然供给粗算：`0 + 1 + 1 + 0.32 = 2.32` 个周窗。这是「每 7 天一个窗」的量级估算，用来理解供需；精确的缺口与用卡数见 §6 的时间轴模拟（结果约 11%，不是 0.12 个整窗）。

### 3.2 消耗速度

本周期用了 100%，窗口已经跑了 `7 天 − 6 小时 = 6.75 天`：

```
burn ≈ 100 / 6.75 = 14.8% / 天
```

这是**平均速度**，也是下限：如果用户其实在第 4 天就打满、后面 3 天没再用，真实需求速度更高。所以界面必须标注「按本周期平均速度估算」，并在计划里给区间，不给单点。

### 3.3 现在用卡值多少

```
等到自然重置的空窗 = 6 小时
能换回的额度 = 14.8% / 天 × 0.25 天 = 3.7% 个周窗
```

一张卡的面值是「一个周窗」，换回 3.7% → 价值比 3.7%，**约 96% 被烧掉**。按接口快照的 5.3h 重算是 3.25%（约 96.75% 被烧掉）；这里用用户提问时的 6h 取整，结论不变。

### 3.4 到订阅到期的缺口

```
需求 = 14.8% / 天 × 16.5 天 = 2.44 个周窗
供给 = 2.32 个周窗（不含卡）
缺口 ≈ 0.11 个周窗 → 省卡策略 0 张；想全程不断 1 张
```

按精确模拟（§6），缺口是 10.9%，不是 0.12 个整窗；1 张卡能补上，但每张只换回 7%–11% 个周窗。若订阅在 11-29 00:00 结束（少 1 天），缺口降到约 3%，1 张更没必要。

---

## 4. 决策模型

### 4.1 输入

| 字段 | 来源 | 说明 |
|---|---|---|
| `weekly.remainPercent` | `ChannelView.windows[key="weekly"]` | 只看周窗；没有周窗 → 无法判断 |
| `weekly.percent` | 同上 | 已用百分比，用来估速度 |
| `weekly.resetsAt` | 同上（RFC3339） | 服务端绝对时刻，不在前端按窗长推算 |
| `fiveHour.remainPercent` | 同上（可选） | **只用于满额文案**：周窗满时区分「只刷新 5h」与「两项都满」；不参与主判断 |
| `resetCredits.availableCount` | `ChannelView.resetCredits`（详情卡实时数据） | `null` = 未知，不是 0；有明细时可推导 |
| `resetCredits.credits[].expiresAt` | 同上 | 最近到期卡；`null` = 接口未提供 |
| `resetCredits.lastRedeemedAt` | 同上 | 刚用过卡 → 旧额度数据不可信 |
| `resetCredits.updatedAt` | 同上 | 与 `lastRedeemedAt` 比，判断数据是否在重置后 |
| `view.updatedAt` | `ChannelView` | 兜底的数据时刻；`lastRedeemedAt > 它` = 数据在重置前 |
| `subEndAt` | **新增，手动设置** | `wham/usage` 不含订阅到期日；按凭据 scope 存 |
| `need` | **新增，交互输入** | 4 档：现在 / 24 小时内 / 能等下次重置 / 不用；可选用量 |
| `scope` | 可选；否则快照 scope | 多账号隔离；不一致时 warning(scope_mismatch) |
| `now` | 后端 | Unix 秒 |

> 订阅到期日为什么必须手动：`wham/usage` 只给 `plan_type` 和窗口，不给「订阅还剩几天」。这不是可以猜的字段——猜错会让整个 horizon 计划失真，所以宁可让用户填一次。

**有效截止 = `min(subEndAt, 最近一张卡的 expiresAt)`。** 本机 2026-11-13 实测：3 张卡的到期分别是 **12-01 03:10、12-02 04:55、12-13 05:40（CST）**，全部晚于订阅到期 11-29。所以本例的有效截止就是 **11-29**；卡到期日只影响「如果续订，这张卡还能用到哪天」，不改变「不续订、11-29 之后都用不了」的结论。

### 4.2 派生量

```
gap_h        = (resetsAt - now) / 3600          // 距自然重置的小时数；resetsAt 缺失则为 null
days_left    = (subEndAt - now) / 86400         // 距订阅到期天数；未填则为 null
elapsed_days = clamp(7 - gap_h/24, 0.25, 7)     // 本周期已跑天数（仅 weekly）
burn         = used / elapsed_days              // %/天；elapsed < 0.5 天或 used < 5% 时标记“不可信”
runs_out_h   = burn > 0 ? remain / burn * 24 : ∞
idle_h       = max(0, gap_h - runs_out_h)        // 自然重置前会被卡住多久
gain         = burn 可信 ? min(100 - remain, burn * idle_h / 24) : null
```

`gain` 是核心：它回答「现在用卡，能在自然重置前多干多少活」。`remain=0` 时 `gain = burn × gap_h / 24`；`remain` 还多时，先扣掉能撑到重置的部分。**burn 不可信时 `gain` 必须是 `null`，不能写成 0**——0 会被读成「用卡毫无价值」，而实际是「算不出来」。

再补四个决定分支的派生量：

```
cards            = availableCount ?? 从 credits 推导（status=available 且未过期）
cards_source     = "api" | "derived" | "unknown"
can_wait         = resetsAt <= subEndAt        // subEnd 未填时为 null，按 true 算但加 warning
earliest_expiry  = min(credits.expiresAt > now) // 无到期日则为 null
expiring_before_reset = count(credits.expiresAt < resetsAt)
all_expire_before_reset = cards > 0
                          && 所有已知到期日 < resetsAt
                          && 没有“到期日未知”的卡
```

`can_wait` 是这次优化轮补的关键量：**自然重置发生在订阅到期之后，就等于等不到。** 没有它，`patient` 会在「等到 12-05 的自然重置」时给出 `WAIT`，而用户 11-29 就已经没订阅了。

`all_expire_before_reset` 处理另一种等不到：**卡在自然重置之前过期。** 用户是 `patient` 时最多损失一张缓冲卡；用户是 `soon/urgent` 且当前额度不够时，等到重置等于把卡等没了，必须先用手上这张。

### 4.3 阈值

| 常量 | 值 | 为什么是这个数 |
|---|---|---|
| `FULL_WINDOW` | 99% | 官方：100% 不重新计时；留 1% 容差给上游四舍五入 |
| `WAIT_GAP_H` | 24h | 24 小时内用卡，最多换 `24h × burn`；按典型 14.8%/天只有 14.8%，低于「值一张卡」的直觉 |
| `MIN_GAIN` | 25% | 一张卡的机会成本约一个周窗；换回不到 1/4 个窗，不如等。用于即时文案的价值标注与 §6 模拟的用卡门槛，不单独决定 `verdict` |
| `SUNK_MIN_GAIN` | 5% | 卡会在下次重置前过期、且当前确实有闲置空窗；不降门槛就会把卡等没。只用于模拟，不改即时 verdict |
| `SUB_WARN_DAYS` | 3d | 距到期 <3 天，卡最多用出 3/7≈43%，必须提示「用不完整」 |
| `NEED_TTL_H` | 12h | 需求会变；过期回到保守建议，不拿昨天的答案指导今天 |
| `STALE_MIN` | 15min | 快照超过 15 分钟且已跨过重置点 → 不判断，等刷新 |

阈值不写死在 UI 文案里；文案由 `reasonCode` 映射，改阈值只改常量与测试。

### 4.4 判定顺序（v2，2026-11-13 优化轮）

先算 §4.2 的派生量，再按下面的门走。**顺序本身就是安全策略**：越靠前的门越不能算错；能靠“满额保护”直接驳回的，不要先算需求。

```
0. 硬门（先排除“算了也没用”）
   - subEndAt != null && subEndAt <= now
                              → HOLD(sub_expired)
   - 没有 weekly 窗            → UNKNOWN(no_weekly)
   - remain >= 99%             → HOLD(full_window)
        · fiveHour.remain < 99 → HOLD(full_window_five_hour)
        · fiveHour.remain >= 99 → HOLD(full_window_both)
   - |remain + used - 100| > 1 → UNKNOWN(data_inconsistent)
   - resetsAt 缺失             → UNKNOWN(reset_unknown)
   - gap_h <= 0                → UNKNOWN(stale_past_reset)   // 已过重置点，等刷新
   - gap_h > 168               → UNKNOWN(reset_unknown)      // 超过 7 天，窗口异常
   - lastRedeemedAt > dataUpdatedAt
                              → UNKNOWN(just_redeemed)

1. 先问“等得到吗”（can_wait）
   - can_wait == false（resetsAt > subEndAt，自然重置在订阅结束后）：
       urgent/soon: amount > remain → NOW(deadline_before_sub_end)
                    amount <= remain → HOLD(remaining_covers)
       patient    → HOLD(no_reset_before_sub_end)
       idle       → HOLD(no_need)
   - 卡在自然重置前过期（earliest_expiry < resetsAt）：
       need ∈ {urgent, soon} 且 amount > remain
                    → NOW(card_expires_before_reset)  // 等到重置，卡先没了
       need ∈ {patient, idle}
                    → 继续走第 2 步，但加 warning(card_expiry_loss)
       all_expire_before_reset == true
                    → 文案必须写明“等到重置，所有卡都过期了”

2. 需求判定
   - urgent:
       amount > remain  → NOW(deadline_before_reset)
       amount <= remain → HOLD(remaining_covers)
   - soon:
       can_wait && gap_h <= 24 → WAIT(gap_too_short)
       amount > remain          → NOW(deadline_before_reset)
       amount <= remain         → HOLD(remaining_covers)
   - patient:
       can_wait && gap_h <= 24 → WAIT(gap_too_short)
       can_wait && gap_h > 24  → WAIT(wait_for_reset)
       !can_wait               → HOLD(no_reset_before_sub_end)
   - idle:
       can_wait && gap_h <= 24 → WAIT(gap_too_short)
       else                    → HOLD(no_need)

3. 卡数校验（只覆盖 NOW；WAIT/HOLD 不因卡数改变结论）
   - cards == 0    → HOLD(no_cards)
   - cards == null → UNKNOWN(cards_unknown)
   - cards > 0     → NOW 保留

4. 质量标注（不改 verdict，只进 warnings / 文案）
   - burn 不可信或 gain 为 null → warning(burn_unknown)
   - NOW 且 gain < 25%          → warning(low_value_now)
   - cards 由明细推导           → warning(cards_derived)
   - count 与明细不一致         → warning(count_mismatch)
   - 有卡但到期日未知           → warning(card_expiry_unknown)
   - dataUpdatedAt 缺失         → warning(data_time_unknown) // 刚用卡可能识别不到
   - 订阅剩余 < 24h             → warning(sub_ends_soon)
   - 订阅剩余 < 3d              → warning(sub_short)       // 卡最多用出 3/7
   - 订阅到期日未填             → warning(plan_unavailable)
```

判定表（基础分支；卡过期/满额/数据门是覆盖规则，优先级见上面的顺序）：

| need | `can_wait` 且 gap ≤ 24h | `can_wait` 且 gap > 24h | `!can_wait`（重置在订阅后） |
|---|---|---|---|
| `urgent` | `NOW`（amount > remain）/ `HOLD` | `NOW`（amount > remain）/ `HOLD` | `NOW`（amount > remain）/ `HOLD` |
| `soon` | `WAIT` | `NOW`（amount > remain）/ `HOLD` | `NOW`（amount > remain）/ `HOLD` |
| `patient` | `WAIT(gap_too_short)` | `WAIT(wait_for_reset)` | `HOLD(no_reset_before_sub_end)` |
| `idle` | `WAIT` | `HOLD` | `HOLD(no_need)` |

> `HOLD` 与 `WAIT` 的区别：`WAIT` 前面有一个确定的免费自然重置；`HOLD` 没有，只是「现在别用，留着」。`can_wait == false` 时不允许返回 `WAIT`——等不到的东西不能建议用户等。

**四个横切规则（检查判定树是否漏项用；具体优先级以 §4.4 的 0–4 为准）：**

1. **前置门**：订阅已过期、没有周窗 → 直接 `HOLD/UNKNOWN`，不要先算需求。
2. **满额保护**：`remain ≥ 99%` 时，不管 need 是什么，都不建议用卡；`fiveHour` 只用来把文案说准（周窗满、5h 也满 vs 只刷新 5h），不改变 verdict。
3. **数据门**：数据不自洽、重置时间缺失/已过点/超 7 天、刚用过卡 → `UNKNOWN`，不硬算。
4. **卡过期/卡数**：卡在自然重置前过期，且用户 `urgent/soon` 且额度不够 → `NOW`；只有 `NOW` 会被卡数降级，`WAIT/HOLD` 不因为没卡改判。

**边界取整规则：**

- `gap_h == 24` 算 `≤ 24h`（等）；`gap_h == 168` 算合法；`> 168` 才算异常。
- `remain == 99` 算满额保护；`98.9` 不算，但会触发 `low_value_now` 文案。
- `amount == remain` 算“剩余够”，返回 `HOLD(remaining_covers)`；只有严格 `>` 才 `NOW`。
- `amount` 缺失、≤0、非数字：`urgent/soon` 按 100% 保守估；`patient/idle` 忽略。
- `amount > 100`：夹到 100；`percent/remain` 超出 `[0,100]` 超过 1 个百分点 → `data_inconsistent`。

### 4.5 为什么不用「余额够不够撑到周末」这种单一规则

因为三个量会互相抵消：

- 剩余 60%、但 5 天后才重置、每天用 20% → 3 天耗尽，空窗 2 天，用卡能换 40%；
- 剩余 0%、但 6 小时后重置 → 用卡只换 3.7%，等；
- 剩余 0%、距重置 3 天、订阅只剩 4 天 → 用卡换 3 天，但卡本来也快作废，值不值取决于有没有截止任务。

单一阈值必然在其中一种情况下给错答案。`gain + 截止` 两个维度才能覆盖。

v2 又补了两个维度：`can_wait`（自然重置是否发生在订阅到期之前）和 `card_expiry`（卡是否会在重置前过期）。它们不改变“现在值不值”的计算，但会把“等”从正确建议变成错误建议——**等不到的重置、等不到的重置前就过期的卡，都不能建议用户等。**

---

## 5. 交互设计

### 5.1 位置

Codex 详情卡（`detailBody`）里，**「重置卡」区块之上**新增一个 `重置顾问` 区块。不新开页面、不弹窗、不进首页摘要。

> **2026-11-13 实现回写**：按 PC 原型改为「重置卡区块整块可点，在现有 `detailBody` 内切换为重置顾问子视图」，不再只是内联展开。仍然是同一个详情弹层单例：窄窗 480 全宽子视图、宽窗 860 内同宽子视图，不新开第二个窗口；Esc / 顶部返回都回到 Codex 详情。

```
┌ 配额窗口 ───────────────────────────────┐
│ 本周   0.0%    下次重置 11-13 17:56      │
│ 5 小时 100%    17:53 重置                │
│ 周窗到期即清零，不滚存；5 小时窗按滚动恢复 │
├ 重置顾问 ───────────────────────────────┤
│ 建议：等约 5 小时后的自然重置               │
│ 现在用卡只能换回约 3% 个周窗；            │
│ 17:56 自然重置免费给满额。                │
│ [现在就要用] [24 小时内要用]               │
│ [能等到下次重置] [暂时不用]                │
│ ───────────────────────────────────── │
│ 到 11-29 还有 16 天 · 自然重置 3 次       │
│ 按 14.8%/天：省卡 0 张，到到期差 11%      │
│ 严格补满所有空窗要 3 张；每张只换回约 3.3% │
│ 不建议为了补满去烧卡；等自然重置更划算      │
│ 小字：用卡会改写每周重置日；满额时别点。    │
├ 重置卡 ────────────────────────────────┤
│ 3 张可用                                 │
│ 额度重置卡   最早 12-01 03:10 到期        │
│              （订阅 11-29 更早）           │
└─────────────────────────────────────────┘
```

> 上图是用户 2026-11-13 的实机样例：服务端快照 `resetsAt=17:56:52`，正文按用户提问时的「约 6 小时」取整，工具实际用实时 `resetsAt` 计算。三张卡的真实到期是 12-01 03:10、12-02 04:55、12-13 05:40；**最早一张也晚于订阅到期 11-29**，所以卡片区块要写「订阅更早」，不能只写卡到期日。

### 5.2 什么时候出现

| 条件 | 行为 |
|---|---|
| 周窗剩余 > 25% 且无卡临期 | 区块折叠成一行 `重置顾问：本周还够用，暂不需要用卡`，不占高度 |
| 周窗剩余 ≤ 10% 或已限流 | 自动展开，显示完整建议 |
| 有卡 3 天内到期 | 展开，并在标题右侧加 `1 张卡 3 天内到期`（纯文字，不染红） |
| 没有周窗（非 weekly 套餐） | 整块隐藏，不留「不可用」占位 |

### 5.3 四个按钮 = 四种需求

| 按钮 | `need.kind` | 含义 | 默认用量 |
|---|---|---|---|
| 现在就要用 | `urgent` | 有必须在几小时内完成的事 | 未填时按 100% 保守估 |
| 24 小时内要用 | `soon` | 24 小时内要用，但可以等几小时 | 未填时按 100% 保守估 |
| 能等到下次重置 | `patient` | 这周会用，但可以等自然重置 | 0（不参与判定） |
| 暂时不用 | `idle` | 没有明确需求 | 0 |

默认选中 `patient`（保守：不假设用户有紧急需求），并在标题下注明「默认按能等到下次重置估算，点按钮可改」。选中 `urgent` / `soon` 后，展开一行「这次大概要用多少？」三个小 chip：`不到 25%` / `约 50%` / `一周量`。不弹第二个弹窗。

### 5.4 结果文案（按 reasonCode）

| reasonCode | 标题 | 正文（必须带数字） |
|---|---|---|
| `deadline_before_reset` | 现在重置 | 「你选的是现在就要用；自然重置在 17:56（约 5 小时后）。用这张卡能拿到满额，但会改写每周重置日；如果不急，等免费满额更划算。」 |
| `gap_too_short` | 等自然重置 | 「只差约 5 小时；现在用卡约换回 3.3% 个周窗，等免费满额更划算。」 |
| `wait_for_reset` | 等自然重置 | 「你选的是能等；X 天后自然满额，这张卡留到下次卡住。」 |
| `full_window` | 现在别用 | 「周额度还是满的。官方只恢复低于 100% 的窗口；现在点可能只刷新 5 小时窗，却消耗一张卡。」 |
| `full_window_five_hour` | 现在别用 | 「周窗是满的，点了只会刷新 5h 窗。如果你正被 5h 限流且马上要干活，这是 5h 窗的取舍，不在本顾问的周窗判断里；否则别点。」 |
| `full_window_both` | 现在别用 | 「周窗和 5h 窗都满；官方只恢复低于 100% 的项，点了可能不生效，别赌。」 |
| `remaining_covers` | 不用卡 | 「还剩 62%，按 14.8%/天能撑到自然重置；这张卡留到下次卡住。」 |
| `no_need` | 先留着 | 「没有明确需求就别用；真要开工时约能换回 X% 个周窗。」 |
| `no_reset_before_sub_end` | 等不到重置 | 「下一次自然重置在 11-29 之后；等到那时订阅已经结束。要用就现在用，不用就留着作废。」 |
| `deadline_before_sub_end` | 现在重置 | 「你选的是现在就要用，而自然重置在订阅之后；剩余额度撑不到，只能用卡。」 |
| `card_expires_before_reset` | 现在重置 | 「这张卡会在下次自然重置之前过期；等到重置等于把卡等没了，先用它。」 |
| `data_inconsistent` | 无法判断 | 「周窗的已用与剩余加起来不等于 100%；数据可能正在刷新，等下一轮再算。」 |
| `no_cards` | 没有可用卡 | 「接口显示 0 张可用；先确认卡是否已用完或过期。」 |
| `no_weekly` | 无法判断 | 「当前套餐没有周额度窗口，本建议只看周窗。」 |
| `reset_unknown` | 无法判断 | 「接口没给下次重置时间；只能提示：满额时别点。」 |
| `stale_past_reset` | 待刷新 | 「数据停在重置点之前，刷新后再判断。」 |
| `just_redeemed` | 待刷新 | 「刚用过一张卡；每周重置日已改写，等下一次取数后重算。」 |
| `cards_unknown` | 无法判断 | 「重置卡数量没取到，先刷新再判断。」 |
| `sub_expired` | 订阅已到期 | 「订阅已到期，重置卡没有使用价值。」 |

**Warning codes（不改 verdict，只进 `warnings[]`）：**

| code | 触发 | 文案 |
|---|---|---|
| `burn_unknown` | `elapsed < 0.5 天` 或 `used < 5%`，或 `gain` 为 null | 「本周期样本太少，换回值只能粗估；先按即时建议。」 |
| `low_value_now` | `NOW` 且 `gain < 25%` | 「你选了现在用，但只换回 X%；如果截止能挪，等更划算。」 |
| `cards_derived` | `availableCount` 为 null，由明细推导 | 「卡数由明细推导；以接口数量为准，刷新后复核。」 |
| `count_mismatch` | `availableCount` 与未过期明细数量不一致 | 「接口数量与明细不一致；按数量算，明细只用到到期日。」 |
| `card_expiry_unknown` | 有卡但 `expiresAt` 为 null | 「有卡没有到期日；不假设它临期，也不假设它永久有效。」 |
| `data_time_unknown` | `dataUpdatedAt` 缺失 | 「数据时刻缺失；刚用过卡可能识别不到，刷新后再看。」 |
| `card_expiry_loss` | 卡在自然重置前过期，但 need 是 patient/idle | 「等到重置，这张卡会先过期；你不需要它就没关系。」 |
| `sub_ends_soon` | `0 < days_left < 1` | 「订阅 1 天内到期；等到重置也用不了几天。」 |
| `sub_short` | `1 <= days_left < 3` | 「订阅只剩 X 天；一张卡最多用出 X/7，别按整周估。」 |
| `plan_unavailable` | `subEndAt` 未填或 `burn` 不可信 | 「计划区算不了；先填订阅到期日，或等本周期样本够了再看。」 |
| `scope_mismatch` | 传入 `scope` 与快照 scope 不一致 | 「账号 scope 不一致，订阅日期可能不是这个账号的；请复核。」 |
| `amount_defaulted` | `urgent/soon` 未填用量 | 「按 100% 保守估；填了实际用量会更准。」 |
| `need_defaulted` | `need.kind` 非法 | 「需求选项无法识别，先按能等到下次重置估算。」 |

### 5.5 状态与持久化

- `subEndAt`：按 `scope` 存 `codex_reset_profile_v1:<scope>`，长期有效；写失败 → 回滚日期 + `showToast`（写操作范式）。
- `need`：同 key，带 `needAt`；12 小时 TTL，过期后回到默认 `patient`。应用一直开着时，60 秒 ticker 也要检查 `needAt`，不能只在下次点击时才过期。
- 换账号（snapshot `scope` 变化）：旧 profile 不读，避免拿 A 的到期日指导 B。
- `lastRedeemedAt` 变化：立即把建议置为 `just_redeemed`，等下一轮刷新，不拿重置前的额度算。
- `dataUpdatedAt`：前端必须把 `ChannelView.updatedAt`（或 `resetCredits.updatedAt`）一起传给命令；没有它，`lastRedeemedAt > dataUpdatedAt` 判不出来。缺失时加 warning，不默认等于 `now`。
- **请求乱序**：用户连点四个按钮时，晚到的旧响应不得覆盖新结论。`codex-reset.js` 维护 `adviseSeq`，只接受最后一次请求的响应；jsdom 冒烟要测「先发的慢响应后到」。
- **profile 读失败**：不阻塞建议；按默认 `patient` + `plan_unavailable` 降级，写回时再提示。
- `amountPercent` 跟 `need` 一起存；`urgent/soon` 未填时按 100 估，并加 `amount_defaulted` warning。


### 5.6 不做的事

- 不做「一键重置」按钮。产品不执行平台侧动作；用户自己去 Codex「个人资料 → 用量摘要」点。
- 不做自动弹窗。周额度耗尽时最多在详情卡内展开，不抢焦点。
- 不因为「卡快到期」就建议用。卡的到期只影响「最晚什么时候用」，不影响「该不该用」。

---

## 6. 到订阅到期的用卡计划（Horizon）

### 6.1 为什么不是简单加法

`自然重置 + 卡` 不能直接相加：卡会**改写**下一次自然重置的时间。一张卡用在下一次自然重置之前，是把那次自然重置提前，不是额外加一个窗。简单加法会把「3 次自然 + 3 张卡 = 6 个窗」算成供给，实际可能只有 4 个。所以用 7 天步进的确定性模拟。

### 6.2 模拟规则（v2，按卡到期日）

```
t = now; quota = remain; next = resetsAt
card_expiries = credits 按 expiresAt 升序（null 排最后，视为“到期日未知”）
known_cards   = len(card_expiries)
cards         = availableCount
unknown_expiry = max(0, cards - known_cards)
consumed = 0; idle = 0; used = 0; iter = 0

while t < subEnd && iter < 1000:
    iter++
    t_exhaust = burn > 0 ? t + quota / burn * 24h : INF
    t_event   = min(next, subEnd)

    if t_exhaust <= t_event:
        consumed += quota; t = t_exhaust; quota = 0
        if t >= subEnd: break
        gap  = next - t
        gain = min(100, burn * gap / 24h)
        usable = card_expiries 中 expiresAt > t 的卡 + unknown_expiry
        // 最早可用的卡会在下次重置前过期 → 这张是“沉没卡”，门槛降到 5%
        earliest = min(usable 的 expiresAt > t)
        sunk = earliest != null && earliest < next
        threshold = sunk && idle > 0 ? SUNK_MIN_GAIN : MIN_GAIN
        if usable > 0 && gain >= threshold:
            用最早到期的那张可用卡
            cards--; used++; quota = 100; next = t + 7d
        else:
            idle += next - t; t = next; quota = 100; next += 7d
    else:
        consumed += min(quota, burn * (t_event - t)/24h)
        quota -= burn * (t_event - t)/24h; t = t_event
        if t >= subEnd: break
        quota = 100; next += 7d
```

`coverCards` 定义为：**从 0 到 `availableCount` 逐个跑同一份模拟，取第一个 `shortfall == 0` 的最小卡数**；都不为 0 就返回 `null`（“现有卡也不够”）。`plannedCards` 是“省卡”贪心策略实际用掉的张数。两者不是一回事：14.8%/天时 `plannedCards=0`、`coverCards=1`，因为省卡策略宁愿差 10.9% 也不花一张只换回 10.9% 的卡；如果用户要求“全程不断”，才需要 1 张。

**为什么不是简单加法：** 卡会**改写**下一次自然重置的时间。一张卡用在下一次自然重置之前，是把那次自然重置提前，不是额外加一个窗。简单加法会把「3 次自然 + 3 张卡 = 6 个窗」算成供给，实际可能只有 4 个。所以用 7 天步进的确定性模拟。

**卡到期日必须进模拟：** 如果一张卡在 11-20 到期，而模拟在 11-27 还想用它，供给就会凭空多出一个窗。v2 在每次用卡前只从“当时还没过期”的卡里取，且优先用最早到期的；到期日未知的卡不假设永久有效，单独计数并提示。

### 6.3 用户账本模拟结果

| 实际速度 | 不用卡到到期差 | 省卡策略用几张 | 用后还差 | 每张换回 | 结论 |
|---|---|---|---|---|---|
| 10%/天 | 2.5% | 0 | 2.5% | — | 自然够 |
| **14.8%/天（本周期平均）** | **10.9%** | **0** | **10.9%** | 1 张可补，7%–11% | **0–1 张，补不补都行** |
| 16%/天 | 28.0% | 0 | 28.0% | 12%（3 张可补） | 能补但很亏 |
| 18%/天 | 56.5% | 2 | 4.5% | 26% | 2 张（仍差 4.5%，第 3 张可补） |
| 20%/天 | 85.0% | 3 | 5.0% | 40% | 3 张（仍差 5.0%，现有卡也不够补满） |
| 25%/天 | 156.3% | 3 | 11.8% | 75% | 3 张仍略差 |
| 30%/天 | 227.5% | 3 | 94.1% | 100% | 3 张远远不够 |

> 这张表就是「3 次机会怎么用合算」的答案：不是按次数平均分配，而是按**你实际能消耗的速度**决定。14.8%/天时卡几乎用不上；20%/天时 3 张刚好；30%/天时 3 张也不够。第一周先按「省卡」跑，用真实速度校准，再决定动几张。

### 6.4 输出

计划区只显示三行，不画甘特图：

1. `自然重置 3 次 · 到 11-29 还剩 16 天`
2. `按 14.8%/天：省卡 0 张；到到期差 11% 个周窗`
3. `想全程不断：1 张可补，但每张只换回 7%–11%，不划算；11-24 后若速度上到 20%/天，再按 3 张规划。`

如果 `coverCards == null`（现有卡也不够补满）：第 2 行改成 `按当前速度，现有 3 张也不够补满，仍差 X% 个周窗`，并给两个选项文案：「降速 / 把部分工作排到订阅外」——不推荐买卡（官方卡不可买，只有促销赠送）。

`plannedCards == 0` 时，第 3 行改成：`自然重置就够；3 张卡是缓冲，不用为了「不用浪费」强行用`。`coverCards != null && coverCards > plannedCards` 时，第 3 行写清「补满要 X 张，但每张只换回 Y%，补不补都行」，把选择权还给用户。

---

## 7. 架构与落点

### 7.1 模块划分（按仓库的规模棘轮）

| 文件 | 改动 | 行数预算 |
|---|---|---|
| `app/core/src/codex_reset.rs` | **新增**：纯函数 `advise()` + 类型 + 单测 | 新增文件，<1500 |
| `app/core/src/lib.rs` | `pub mod codex_reset;` | +1 |
| `app/src-tauri/src/codex_reset_cmd.rs` | **新增**：3 个 Tauri 命令，读写 profile，取快照 | 新增文件，<1500 |
| `app/src-tauri/src/lib.rs` | `mod codex_reset_cmd;` + 3 个 handler 名 | +5（预算余 68） |
| `app/src/codex-reset.js` | **新增**：渲染区块、按钮事件、invoke、失败提示 | 新增文件 |
| `app/src/codex-reset.css` | **新增**：区块样式，复用 V2 令牌 | 新增文件 |
| `app/src/index.html` | 1 个 `<link>` + 1 个 `<script>`（app.js 之后，确保复用其全局格式化函数） | +2（预算余 143） |
| `app/src/app.js` | `coreCodexResets()` 里插入 1 行渲染调用 + 1 个事件分支 | +15（预算余 84） |

**不往 `app.js` / `lib.rs` / `fetch.rs` 里塞逻辑**：`codex_reset.rs` 是纯计算，命令层只做 IO 胶水，前端新文件独立。这样 `check-file-size.js` 不需要上调任何预算。

### 7.2 数据流

```
ChannelView（详情卡正在渲染的这一份，实时）
  ├─ windows[weekly]
  └─ resetCredits（数量 + 明细 + 到期日 + lastRedeemedAt）
        │
        ├─→ codex_reset_cmd::advise（只校验/夹取范围，不读快照）
        │
codex_reset_profile_v1:<scope>（新增，subEnd + need）
        │
  codex_reset::advise(input)  ← 纯函数，无 IO
        │
  Advice (JSON) → codex-reset.js 渲染
```

**为什么不用 `quota_snapshot_v1:codex`：** 2026-11-13 在本机实测，该快照里的 `resetCredits` 是 `null`，而同一时刻 `rate-limit-reset-credits` 返回了 3 张完整明细（含 `expires_at`）。顾问如果读快照，会出现「详情卡有卡、顾问说未知」的自相矛盾。因此顾问以详情卡同一份实时 `ChannelView` 为准；快照只用来取 `scope`（给 profile key），不用来取卡数据。

### 7.3 命令契约

```rust
#[tauri::command]
async fn codex_reset_advice(
    state: State<'_, AppState>,
    id: String,
    input: codex_reset::AdviceInput,   // weekly 窗口 + resetCredits + need
) -> Result<codex_reset::Advice, String>;

#[tauri::command]
fn codex_reset_profile_get(state: State<'_, AppState>, id: String)
    -> Result<codex_reset::Profile, String>;

#[tauri::command]
fn codex_reset_profile_set(
    state: State<'_, AppState>,
    id: String,
    profile: codex_reset::ProfilePatch,
) -> Result<codex_reset::Profile, String>;  // 写失败必须 Err，前端 toast
```

`AdviceInput` 由前端从当前 `ChannelView` 直接构造：`weekly` 只取 `key==="weekly"` 的窗口，`fiveHour` 取 `key==="5h"`（可缺），`resetCredits` 原样传入，`dataUpdatedAt` 传 `ChannelView.updatedAt`（缺失时用 `resetCredits.updatedAt`），`subEndAt` 从 profile 读。命令层只做三件事：范围校验（`0≤percent≤100`、时间戳>0）、按 `scope`/`id` 读 profile、调用纯函数。**命令不读快照里的卡数据，也不发起网络请求。** 这样既与详情卡同源，又不增加账务接口调用。

`Advice` 序列化（camelCase）：

```json
{
  "verdict": "wait",
  "reasonCode": "gap_too_short",
  "headline": "等约 5 小时后的自然重置",
  "detail": "现在用卡只能换回约 3.3% 个周窗；17:56 自然重置免费给满额。",
  "metrics": {
    "weeklyRemainPercent": 0.0,
    "weeklyUsedPercent": 100.0,
    "nextResetAt": 1794563812,
    "gapHours": 5.26,
    "cardsAvailable": 3,
    "cardsUnknown": false,
    "cardsSource": "api",
    "canWait": true,
    "fiveHourRemainPercent": 100.0,
    "earliestCardExpiry": 1796065830,
    "cardsExpiringBeforeReset": 0,
    "allCardsExpireBeforeReset": false,
    "burnPercentPerDay": 14.8,
    "burnEstimated": true,
    "cardGainPercent": 3.25,
    "subEndAt": 1795967940,
    "daysToSubEnd": 16.47,
    "dataUpdatedAt": 1794544860,
    "planAvailable": true,
    "plannedCards": 0,
    "coverCards": 1,
    "shortfallPercent": 10.9
  },
  "planLines": [
    "自然重置 3 次 · 到 11-29 还剩 16 天",
    "按 14.8%/天：省卡 0 张；到到期差 11% 个周窗",
    "想全程不断：1 张可补，但每张只换回 7%–11%，不划算"
  ],
  "warnings": ["用卡会改写每周重置日；满额时别点。", "按本周期平均速度估算，样本 6.75 天。"],
  "computedAt": 1794544860
}
```

- `verdict`：`now | wait | hold | unknown` 四值，前端不自己推导。
- `reasonCode`：稳定枚举，文案映射在前端；后端不返回中文长句以外的判断逻辑。
- `planLines`：最多 3 行，防止详情卡被计划撑爆。
- 所有时间戳为 Unix 秒，前端按本地时区格式化（复用 `exactLocalTime`）。

### 7.4 存储

```json
// key: codex_reset_profile_v1:<scope>
{
  "subEndAt": 1795967940,
  "need": { "kind": "patient", "amountPercent": 0.0, "at": 1794544907 },
  "updatedAt": 1794544907
}
```

- `scope` 来自 `quota_snapshot_v1:codex` 的 `scope`；没有 scope 就不持久化，只在本轮内存里算。
- `subEndAt` 存 epoch 秒，界面按本地日期显示；用户改日期后立即重算。
- profile 很小，不新增表；沿用 `Store::get_setting/set_setting`。

---

## 8. 异常与诚实口径

| 情况 | 判定 | 界面 | 绝不做什么 |
|---|---|---|---|
| 订阅已过期 | `hold/sub_expired` | 「订阅已到期，卡无使用价值」 | 不建议用卡 |
| 没有周窗 | `unknown/no_weekly` | 「当前套餐没有周额度窗口」 | 不拿主窗（可能是月窗）冒充周窗 |
| 周窗满 + 5h 也满 | `hold/full_window_both` | 「两项都满，点了可能不生效」 | 不把“点了没用”说成“能刷新” |
| 周窗满 + 5h 未满 | `hold/full_window_five_hour` | 「只会刷新 5h 窗；5h 的取舍另说」 | 不因为 5h 没满就建议用卡 |
| 周窗已用 + 剩余 ≠ 100% | `unknown/data_inconsistent` | 「数据正在刷新，等下一轮」 | 不拿两个矛盾的数硬算 |
| `resetsAt` 缺失 | `unknown/reset_unknown` | 「重置时间未知」+ 满额提示 | 不按 7 天自己推算 |
| 快照已跨过重置点（≤15min） | `unknown/stale_past_reset` | 「数据停在重置前，刷新后再判断」 | 不拿旧百分比说「还有 6 小时」 |
| 重置点已过 >15min | `unknown/reset_unknown` | 「重置时间异常，等刷新」 | 不把过期时间当下一刻 |
| `resetsAt` 距现在 >7 天 | `unknown/reset_unknown` | 「重置时间异常，等刷新」 | 不把月窗/错误字段当周窗 |
| 自然重置在订阅到期之后 | `hold/no_reset_before_sub_end`（patient/idle）；urgent/soon 见 §4.4 | 「等不到那次重置」 | 不返回 `WAIT` |
| 卡在自然重置前过期 | `now/card_expires_before_reset`（urgent/soon 且额度不够）；否则 warning | 「等到重置，卡先没了」 | 不在 patient 场景硬劝用卡 |
| 所有卡都在重置前过期 | 同上 + `card_expiry_loss` | 「等到重置，所有卡都过期」 | 不把过期卡算进供给 |
| 卡数量 `null`，但有明细 | 由明细推导，`cards_source=derived` | 「卡数由明细推导」 | 不把未知写成 0 张，也不把明细长度无条件当权威 |
| 卡数量与明细不一致 | 数量权威，warning(`count_mismatch`) | 「按接口数量算，明细只用到期日」 | 不用明细长度覆盖 count |
| 卡明细 `null` 但数量有 | 照常算数量 | 「接口仅返回数量」 | 不编造到期日 |
| 快照 `resetCredits` 为 `null`，详情卡有卡 | 以详情卡实时数据为准 | 正常出建议 | 不因快照缺失判 `unknown`，也不拿 0 张代替 |
| 某张卡无到期日 | 不排序假设，warning(`card_expiry_unknown`) | 「接口未提供到期日」 | 不说「即将到期」，也不假设永久有效 |
| `lastRedeemedAt > dataUpdatedAt` | `unknown/just_redeemed` | 「刚用过卡，等刷新」 | 不拿重置前的额度算 gain |
| 订阅到期日未填 | 只算即时 verdict；计划区不可用 | 「填订阅到期日」入口 + warning | 不默认「还有很久」 |
| 订阅 1 天内到期 | 按 `can_wait` 判；warning(`sub_ends_soon`) | 「等到重置也用不了几天」 | 不把 1 天当成 1 周 |
| 本周期刚开始（elapsed < 0.5 天） | `burnEstimated=false`，`gain=null` | 「速度还看不出来，先按即时建议」 | 不拿 1 小时的斜率外推 16 天，不把 gain 写成 0 |
| `NOW` 但 `gain < 25%` | `now` + warning(`low_value_now`) | 「你选了现在用，但只换回 X%」 | 不因为用户急就把低价值说成高价值 |
| 传入 scope 与快照 scope 不一致 | 正常算 + warning(`scope_mismatch`) | 「订阅日期可能不是这个账号的」 | 不静默用错 scope |
| profile 读失败 | 默认 `patient` + warning(`plan_unavailable`) | 「先按默认估算」 | 不阻塞建议，不静默 |

---

## 9. 测试计划

### 9.1 纯函数表驱动（`cargo test -p tokenscope-core`）

| # | 输入（r / gap / need / 其他） | 期望 verdict | 期望 reason |
|---|---|---|---|
| 1 | 0 / 6h / patient / 3 张 / sub 16.5d | `wait` | `gap_too_short` |
| 2 | 0 / 6h / urgent(100) | `now` | `deadline_before_reset` |
| 3 | 0 / 30h / soon(50) | `now` | `deadline_before_reset` |
| 4 | 0 / 6h / soon(50) | `wait` | `gap_too_short` |
| 5 | 60 / 30h / soon(30) | `hold` | `remaining_covers` |
| 6 | 100 / 5d / urgent(100) | `hold` | `full_window` |
| 7 | 0 / 5d / patient | `wait` | `wait_for_reset` |
| 8 | 0 / 5d / idle | `hold` | `no_need` |
| 9 | 20 / 6h / idle | `wait` | `gap_too_short` |
| 10 | 0 / 3d / patient / 卡数 null | `wait` + warning | `wait_for_reset` |
| 11 | 无 weekly 窗 | `unknown` | `no_weekly` |
| 12 | 快照跨重置点 | `unknown` | `stale_past_reset` |
| 13 | lastRedeemed > snapshot | `unknown` | `just_redeemed` |
| 14 | 0 / 3d / urgent(100) / 卡 0 张 | `hold` | `no_cards` |
| 15 | 0 / 3d / urgent(100) / 卡数 null | `unknown` | `cards_unknown` |
| 16 | 0 / 6h / urgent(100) / 卡 0 张 | `hold` | `no_cards` |
| 17 | 0 / 5d / patient / sub 2d（重置在订阅后） | `hold` | `no_reset_before_sub_end` |
| 18 | 0 / 5d / urgent(100) / sub 2d / 3 张 | `now` | `deadline_before_sub_end` |
| 19 | 0 / 6h / soon(100) / 最早卡 2h 后到期 | `now` | `card_expires_before_reset` |
| 20 | 0 / 5d / patient / 最早卡 3d 后到期 | `wait` + warning | `wait_for_reset` |
| 21 | 100 / 0 / 5h=100 / urgent | `hold` | `full_window_both` |
| 22 | 99 / 1 / 5h=0 / urgent | `hold` | `full_window_five_hour` |
| 23 | 50 / 30（相加 80） | `unknown` | `data_inconsistent` |
| 24 | 0 / 6h / patient / count=null、3 条可用明细 | `wait` + warning | `wait_for_reset`（卡数推导，不 unknown） |
| 25 | 0 / 3d / urgent(100) / 卡 0 张 | `hold` | `no_cards` |
| 26 | 0 / 3d / urgent(100) / count=null、无明细 | `unknown` | `cards_unknown` |
| 27 | 0 / 6h / patient / subEnd 未填 | `wait` + warning | `wait_for_reset` |
| 28 | 0 / 6h / urgent(100) / subEnd 未填 | `now` + warning | `deadline_before_reset` |
| 29 | 0 / 6h / patient / sub 2h 后到期 | `hold` | `no_reset_before_sub_end` |
| 30 | 0 / 24h / soon(50) | `wait` | `gap_too_short`（边界算 ≤24h） |
| 31 | 0 / 168h / patient | `wait` | `wait_for_reset`（边界合法） |
| 32 | 0 / 169h / patient | `unknown` | `reset_unknown` |
| 33 | 0 / 6h / urgent(100) / gain=3.7 | `now` + warning | `deadline_before_reset`（`low_value_now`） |
| 34 | 0 / 5d / patient / elapsed=0.2d、used=2% | `wait` + warning | `wait_for_reset`（`burn_unknown`） |
| 35 | 0 / 5d / patient / 3 张全在 3d 后到期 | `wait` + warning | `wait_for_reset`（`card_expiry_loss`，全过期） |
| 36 | 100 / 100（相加 200） | `hold` | `full_window`（满额保护优先，附 warning） |
| 37 | 订阅已过期 + 无 weekly | `hold` | `sub_expired`（订阅优先于窗口缺失） |
| 38 | 订阅已过期 + r=100 | `hold` | `sub_expired`（订阅优先于满额） |
| 39 | 0 / 6h / urgent(0) | `now` | `deadline_before_reset`（用量按 100 估） |
| 40 | 0 / 6h / urgent(150) | `now` | `deadline_before_reset`（用量夹到 100） |

### 9.2 双向验证

- 断言 `r=100` 的样本绝不返回 `now`（防「满额也建议用」）。
- 断言 `gap=6h`、`patient` 的样本绝不返回 `now`（防「短空窗也烧卡」）。
- 断言 `can_wait=false`、`patient` 的样本绝不返回 `wait`（防「等到订阅结束」）。
- 断言 `cards=0` 的样本绝不返回 `now`（防「没卡还劝用」）。
- 断言 `cards=null` 且无明细的 `NOW` 样本一定降级为 `unknown`，但有明细时必须推导出数量、不得偷懒判 unknown。
- `reasonCode` 到文案的映射必须有「未知 code」兜底，不能显示空白。
- 计划模拟单独测：
  - 16.5 天 / 14.8% / 3 张 → `plannedCards=0`、`shortfallPercent≈10.9`、`coverCards=1`；
  - 16%/天 → `plannedCards=0`、`shortfall≈28`、`coverCards=3`；
  - 18%/天 → `plannedCards=2`、`shortfall≈4.5`、`coverCards=3`；
  - 20%/天 → `plannedCards=3`、`shortfall≈5.0`、`coverCards=null`（现有卡也不够）；
  - 30%/天 → `plannedCards=3`、`shortfall≈94`、`coverCards=null`；
  - 10%/天 → `plannedCards=0`、缺口≈2.5%、`coverCards=0`。
- 模拟边界：
  - 卡在 11-20 到期、模拟到 11-27 时不得再用它；
  - `subEnd` 早于第一次自然重置 → 自然重置次数 0，不允许凭空加窗；
  - `t_exhaust == t_event` 固定「先耗尽、再重置」，结果不能因浮点抖动而变；
  - 迭代上限 1000，输入异常时不得死循环；
  - 到期日未知的卡单独计数，不能假设永久有效。

### 9.3 前端冒烟（`build/jsdom-smoke.js`）

- 详情卡渲染出「重置顾问」区块，四个按钮存在且顺序固定。
- 点 `现在就要用` → 标题变「现在重置」；点 `暂时不用` → 回「先留着 / 等自然重置」。
- `profile_set` 写失败 → `showToast` + 日期回滚；不静默。
- `profile_get` 读失败 → 默认 `patient`，不空白、不抛异常。
- 两种满额态（周窗满 + 5h 满 / 周窗满 + 5h 空）各渲染一次，文案不同但都不出现「现在重置」。
- 四种未知态（无周窗 / 重置时间缺失 / 快照跨重置点 / 数据不自洽）各渲染一次，不抛异常。
- 连点两次不同需求、旧请求后到 → 界面保留最后一次选择的结论，不被慢响应覆盖。
- 60 秒 ticker 后倒计时变化，且**没有**新的 `fetch` 调用（沿用现有断言）。
- 12h `need` TTL 到点后回到 `patient`，不需要用户再点一次。

### 9.4 提交前门禁

按 `AGENTS.md` §4 串行跑：`check-ipc-contract`（新增 3 命令三方齐全）、`check-silent-failures`、`check-file-size`（不上调预算）、`node --check app/src/codex-reset.js`、`check-css-forms`、`jsdom-smoke`、`cargo test --locked`、`cargo check --all-targets --locked`、`scan-secrets.ps1`。jsdom 未安装时如实写「未跑」，不拿历史结果冒充。

---

## 10. 分期

| 期 | 内容 | 验收 |
|---|---|---|
| P0 | `codex_reset.rs` 纯函数 + 3 命令 + 详情卡区块 + 四个按钮 + **§4.4 v2 全部门**（订阅过期/无周窗/满额两态/数据自洽/重置缺失或过点/刚用卡/`can_wait`/卡过期分支/卡数降级） | 40 条表驱动全绿；用户当前场景 `wait/gap_too_short`；`urgent` 能翻 `now`；满额与 `can_wait=false` 不误报 |
| P1 | `subEndAt` 设置 + **§6.2 v2 horizon**（卡到期日进模拟、`coverCards`、`SUNK_MIN_GAIN`）+ 12h need TTL + `burn_unknown` | 16.5 天账本算出「省卡 0 张、缺口 11%、补满 1 张」；20%/天变「3 张且仍差 5%」；卡在重置前到期时不被算进供给 |
| P2 | 卡临期 warning + 并发 `adviseSeq` + scope mismatch + profile 读失败降级 + 5h 满额两态文案 | 不新增通知；连点按钮旧响应不覆盖新结论；换账号 profile 不串；读失败不空白 |

---

## 11. 风险与未决

1. **重置后的每周锚点**：官方说「更改每周重置日期」，但没有给出「兑换时刻 vs 首次请求」的精确公式（付费重置是首次请求 +7 天）。本方案不写死，等用户在真机用一张后核对 `resetsAt`；若发现锚点偏移，只改 `cycleDays` 与测试，不改界面。
2. **平均速度是下限**：`used=100` 只能说明「到某个时刻为止用完了」，不能说明什么时候用完。P1 可加入本地快照历史（若已有按小时的窗口记录）做近 3 天斜率；没有历史就用本周期平均，并明确标注。
3. **订阅到期日手动输入**：多账号时按 scope 隔离；如果用户不填，计划区不可用，但即时建议仍可算。是否需要在设置页放一个全局默认？建议先不做，避免「一个日期管所有账号」。
4. **卡的数量与明细不一致**：`availableCount` 是权威计数，明细可能被上游截断。计划只用 count，到期日只用明细；两者冲突时文案说明，不用明细长度覆盖 count。
5. **5 小时窗**：本方案只看周窗；Plus 用卡会同时刷新 5 小时窗，可能让「今天还能不能用」变好，但这不影响「周窗该不该用卡」的结论。文案里只作为附注，不进公式。
6. **「订阅到期日」的精度**：11-29 是日期不是时刻，00:00 与 23:59 会让「0 张 vs 1 张」翻转。界面必须允许选到日，并在文案里写「按当日 23:59 计」，同时把结论写成区间而不是单点。
7. **快照与实时卡明细不一致**：本机实测 `quota_snapshot_v1:codex` 的 `resetCredits` 为 `null`，同一时刻实时接口有 3 张完整明细。顾问已改为用实时 `ChannelView` 规避，但**重启后、刷新前**详情卡回退到快照仍会显示「重置卡信息未获取」。这像是保存路径的问题（`Snapshot::new` 本身保留字段），建议单独立项查 `quota_attempt` 的保存时序；本方案不假装它不存在。
8. **`can_wait` 依赖手动日期**：`subEndAt` 错一天，`can_wait` 就可能翻转，`patient` 的 `WAIT` 会变成 `HOLD`。界面必须显示日期、允许改，并在未填时加 `plan_unavailable`，不能默认「还有很久」。
9. **卡到期与续订的耦合**：本工具按「不续订」算，有效截止取 `min(subEnd, 最早卡到期)`。如果用户后来续订，`subEndAt` 要同步改，否则卡到期日会被误当成订阅截止；profile 里要记录 `updatedAt`，续订后提示复核。
10. **并发/乱序**：四个按钮连点会并发发命令；没有 `adviseSeq` 时，慢的旧响应会覆盖新结论。这是仓库已经踩过的坑（`loadCheckin` 的并发乱序），必须在 P2 前修好，不能只靠“用户不会连点”。
11. **`dataUpdatedAt` 缺失**：没有它就无法判断 `lastRedeemedAt` 是否在数据刷新之后；不能默认等于 `now`，否则刚用卡会被误判为“数据已更新”。缺失时加 warning，并退回 `just_redeemed` 的保守判断。
12. **两项都满时官方行为不确定**：官方只说“至少恢复一项才算用掉”“100% 的项不重新计时”，没有说两项都 100% 时点重置是拒绝还是白耗。文案只能写“可能不生效/白耗”，不能承诺“点了没用”或“点了会扣”。

---

## 12. 来源

- OpenAI Help Center：在你的 ChatGPT 套餐中使用 Codex（储存重置刷新 5 小时与每周周期、更改每周重置日期）
  https://help.openai.com/zh-hans-cn/articles/11369540
- OpenAI Help Center：Codex 重置的运作方式（三种重置、只有低于 100% 才恢复）
  https://help.openai.com/zh-hans-cn/articles/20001498
- OpenAI Help Center：付费重置工作模式与 Codex 的每周速率限制（下次重置 = 首次请求 +7 天）
  https://help.openai.com/zh-hans-cn/articles/20001507
- 仓库内：`docs/product/product-codex-weekly-reset-2026-11-03.md`、`docs/research/codex-subscription-2026-10-30.md`

---

## 13. 验收清单

- [ ] 用户场景（r=0 / gap=6h / patient / 3 张 / 11-29 到期）给出「等自然重置」，不出现「立即重置」。
- [ ] 同场景点「现在就要用」→ 变「现在重置」，且正文明确写出「否则 6 小时后免费满额」的代价。
- [ ] r=100 → 「现在别用」，并解释「只恢复低于 100% 的窗口」。
- [ ] 周窗满 + 5h 满 / 周窗满 + 5h 空：两种文案都对，都不出现「现在重置」。
- [ ] `can_wait=false` + `patient` → 「等不到重置」，不出现 `wait`；同场景 `urgent` 且额度不够 → `now`。
- [ ] 卡在自然重置前过期 + `soon` 且额度不够 → `now/card_expires_before_reset`；同场景 `patient` → `wait` + warning。
- [ ] 没有周窗 / 重置时间缺失 / 快照跨重置点 / 刚用过卡 / 数据不自洽：五种「无法判断」各一句，不瞎猜。
- [ ] 卡数 null 但有明细 → 推导出数量并给 warning；卡数 null 且无明细 → `unknown`；count 与明细不一致 → count 权威 + warning。
- [ ] 计划区能算出「省卡 0 张 / 缺口 11% / 补满 1 张」；20%/天变「3 张且仍差 5% / 现有卡也不够」；卡在重置前到期时不被算进供给。
- [ ] 连点两个不同需求，旧响应后到不覆盖新结论；12h TTL 到点回 `patient`。
- [ ] 不新增网络请求；不自动兑换；不新增通知。
- [ ] `check-ipc-contract`、`check-file-size`、`check-silent-failures` 全绿；文件预算未上调。
- [ ] `profile_set` 失败有 toast + 回滚，不静默；`profile_get` 失败有默认值，不空白。
- [ ] 40 条纯函数表驱动全绿，且双向验证的“绝不返回”样本真的会红。
