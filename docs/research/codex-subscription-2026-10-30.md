# Codex 订阅与重置卡（2026-10-30）

用户从免费套餐升级后,Grandettoken 已能从实时 `wham/usage.plan_type` 读取新套餐,
无需根据本机 JWT 的旧套餐推断。改动前已按服务端时长识别常见配额窗口,但没有重置卡,
且窗口不足一天时只显示相对倒计时,不展示准确日期。

本次只读实测:当前账号为 `plus`,主窗 18000 秒、次窗 604800 秒;
`rate-limit-reset-credits` 返回 `available_count: 0, credits: []`。
这只能说明查询当时没有可用卡,不能据此推断该套餐不支持重置卡。

实现补充:

- 每轮配额成功后,用同一份 OAuth token 和 `ChatGPT-Account-Id` 查询重置卡明细,4 秒超时,
  响应上限 512 KB。只读 GET,不兑换、不消耗卡,不写回 Codex 登录态。
- `CODEX_HOME/auth.json` 优先,否则读取用户目录的 `.codex/auth.json`。
- 可用数量以接口为准;明细可能不全,不能拿明细长度替代数量。未知数量和 0 张分开。
  仅有数量、null 明细、空明细各自有明确语义。可选查询失败不会使配额查询失败。
- 明细按到期时间排序,排除过期/已使用卡;无到期日显示“接口未提供到期日”。
  如有 `redeemed` 历史,显示最新 `redeemed_at`,不把用量变化推断为用卡重置。
- 配额窗口显示完整本地日期、秒和时区,倒计时每分钟更新。时间已过而未刷新时,
  显示“已到重置时间,待刷新”。
- 仅周窗的套餐可正常显示;15 分钟、1 小时等非典型时长如实命名。
- UI 不获得或持久化卡 ID;本轮卡查询失败不拼接另一个账号的旧卡信息。

来源（读取接口契约后自行实现,未复制第三方源码）:

- [官方 App Server 账号与额度](https://learn.chatgpt.com/docs/app-server)
- [官方 Codex 套餐说明](https://learn.chatgpt.com/docs/pricing)
- [CodexBar Codex 数据源说明](https://github.com/steipete/CodexBar/blob/main/docs/codex.md)
- [CodexBar OAuth 查询与 REST 字段](https://github.com/steipete/CodexBar/blob/main/Sources/CodexBarCore/Providers/Codex/CodexOAuth/CodexOAuthUsageFetcher.swift)

边界:仍依赖 ChatGPT 内部账务接口;尚未覆盖模型独立配额、额外购买 credits、
多账号管理或兑换操作。当前真实账号为 0 张,有卡分支由接口契约夹具验证。
