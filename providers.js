/**
 * ⚠ 已过时(2026-09-18):本文件是设计阶段的 JS 原型,仅作 token-dashboard
 * 原型页的数值参考。**权威实现是 app/core/src/providers.rs** —— 这里曾埋过一个
 * 「业务失败恒为 true」的 bug(Rust 侧已修复并有回归测试),改渠道口径请改 Rust,
 * 不要从本文件抄起。
 *
 * 渠道适配器配置
 *
 * 两种 kind,决定卡片渲染方式:
 *   amount   金额型 —— 有 remaining / used / total,可算钱、可推"预计可用天数"
 *   percent  配额型 —— 只有已消耗百分比 + 重置时间,无法折算金额
 *
 * extractor 统一返回:
 *   { isValid, kind, remaining, used, total, unit, windows?, extra?, error? }
 *   取不到的值一律 null,不要用 0 冒充(0 会被渲染成"已耗尽")
 */

/**
 * 轮询策略
 *
 * 这四个接口都是账务/元数据接口,不是推理接口 —— 查询余额不消耗任何 token,
 * 不产生计费。所以可以放心定时轮询,唯一约束是请求频率礼貌性,不是钱。
 *
 * 快照存本地 SQLite,用相邻快照差值反推消耗(充值型渠道接口不给累计消耗)。
 * 程序未运行的时段会形成空洞,界面需标注消耗为"推算值"。
 */
const POLL = {
  activeSec: 60,    // 面板展开时:1 分钟
  idleSec: 300,     // 折叠/托盘态:5 分钟
  backoffSec: 900,  // 连续失败后退避:15 分钟
  onWake: true,     // 系统唤醒 / 网络恢复时立即补一次
  jitterSec: 5,     // 加抖动,避免多渠道同秒并发
};

/**
 * 密钥存储(决策)
 *
 * 输入:界面明文输入,不做二次确认。
 * 落盘:Windows 凭据管理器(Credential Manager),Tauri 侧用 keyring crate,
 *       服务名 "TokenScope",账户名用渠道 id。密钥永不写入 JSON/SQLite,
 *       也永不出现在日志和导出文件里。
 * 出网:仅向该渠道自身的 url 发请求,无遥测、无云同步。
 */
const SECRET_STORE = { backend: "windows-credential-manager", service: "TokenScope" };

const round2 = (n) => Math.round(Number(n) * 100) / 100;
const round1 = (n) => Math.round(Number(n) * 10) / 10;

const PROVIDERS = [
  // ───────────── 1. 4SAPI:积分制,500000 积分 = 1 USD ─────────────
  {
    id: "4sapi",
    name: "4SAPI",
    short: "4S",
    color: "#e8622c",
    kind: "amount",
    request: {
      url: "https://4sapi.com/api/usage/token",
      method: "GET",
      headers: { Authorization: "Bearer {{apiKey}}" },
    },
    extractor(response) {
      const d = response?.data ?? {};
      const RATE = 500000;

      if (d.total_granted == null || d.total_available == null) {
        const remaining = Number(
          response?.remaining ?? response?.quota?.remaining ?? response?.balance ?? NaN
        );
        return {
          isValid: Number.isFinite(remaining),
          kind: "amount",
          remaining: Number.isFinite(remaining) ? remaining : null,
          used: null,
          total: null,
          unit: "CNY",
          error: Number.isFinite(remaining) ? null : "无可识别的余额字段",
        };
      }

      const granted = parseFloat(d.total_granted);
      const available = parseFloat(d.total_available);
      const used = parseFloat(d.total_used ?? granted - available);

      return {
        // 任一显式 false 即业务失败。之前写 `||`:正常响应没有 is_active 字段,
        // 右侧恒真会吞掉 code:false,失败被当成功渲染 —— 权威实现在
        // app/core/src/providers.rs 的 extract_4sapi,并配有回归测试。
        isValid: (response?.code === undefined || response?.code === true) &&
                 response?.is_active !== false,
        kind: "amount",
        remaining: round2(available / RATE),
        used: round2(used / RATE),
        total: round2(granted / RATE), // 独立字段,不再塞进 unit 字符串
        unit: "CNY", // 接口标 USD,实际结算为人民币,统一按 ¥ 展示

      };
    },
  },
  // ───────────── 2. OpenCode Go:未公开接口,只返回百分比 ─────────────
  // 源码: sst/opencode  packages/console/app/src/routes/zen/go/v1/usage.ts
  {
    id: "opencode-go",
    name: "OpenCode Go",
    short: "OC",
    color: "#f5a623",
    kind: "percent",
    unstable: true, // 未公开,随时可能变;失败要静默降级而非报警
    request: {
      url: "https://opencode.ai/zen/go/v1/usage",
      method: "GET",
      headers: { Authorization: "Bearer {{apiKey}}" }, // 只认 Bearer,x-api-key 无效
    },
    extractor(response) {
      if (response?.type === "error") {
        const t = response.error?.type;
        return {
          isValid: false,
          kind: "percent",
          error:
            t === "EntitlementError" ? "未订阅 Go 套餐" :
            t === "AuthError" ? "密钥无效或缺失" : (response.error?.message || "请求失败"),
        };
      }

      const u = response?.usage;
      if (!u) return { isValid: false, kind: "percent", error: "响应结构异常" };

      const win = (key, label) => {
        const w = u[key];
        if (!w) return null;
        const pct = Number(w.percent) || 0; // percent = 已消耗
        return {
          label,
          percent: pct,
          remainPercent: round1(100 - pct),
          status: w.status, // "ok" | "rate-limited"
          resetsAt: w.resetsAt, // ISO 8601
        };
      };

      // 三个窗口任一触顶都会限流,所以全部展示
      const windows = [
        win("rolling", "5 小时"),
        win("weekly", "本周"),
        win("monthly", "本月"),
      ].filter(Boolean);

      const tightest = windows.reduce((a, b) => (b.percent > a.percent ? b : a), windows[0]);

      return {
        isValid: true,
        kind: "percent",
        remaining: tightest ? tightest.remainPercent : null,
        used: tightest ? tightest.percent : null,
        total: 100,
        unit: "%",
        windows,
        limited: windows.some((w) => w.status === "rate-limited"),
      };
    },
  },

  // ───────────── 3. DeepSeek 官方 ─────────────
  {
    id: "deepseek",
    name: "DeepSeek",
    short: "DS",
    color: "#4d6bfe",
    kind: "amount",
    request: {
      url: "https://api.deepseek.com/user/balance",
      method: "GET",
      headers: { Authorization: "Bearer {{apiKey}}" },
    },
    extractor(response) {
      const info = response?.balance_infos?.[0];
      if (!info) return { isValid: false, kind: "amount", error: "无余额信息" };

      return {
        isValid: response.is_available !== false,
        kind: "amount",
        remaining: round2(parseFloat(info.total_balance)),
        used: null,  // 官方不返回累计消耗,需本地流水推算
        total: null, // 充值型无"限额"概念,不画进度条
        unit: info.currency || "CNY",
        extra: {
          赠送余额: round2(parseFloat(info.granted_balance || 0)),
          充值余额: round2(parseFloat(info.topped_up_balance || 0)),
        },
      };
    },
  },

  // ───────────── 4. Hapi:积分制,20 积分 = 1 USD ─────────────
  {
    id: "hapi",
    name: "Hapi",
    short: "HA",
    color: "#22a06b",
    kind: "amount",
    request: {
      url: "https://ai.yuchuantest.com/v1/usage",
      method: "GET",
      headers: {
        Authorization: "Bearer {{apiKey}}",
        "User-Agent": "cc-switch/1.0",
      },
    },
    extractor(response) {
      const d =
        response?.data && typeof response.data === "object" ? response.data
        : response && typeof response === "object" ? response : {};

      if (d.error || d.balance == null) {
        return { isValid: false, kind: "amount", error: d.error || "无 balance 字段" };
      }

      return {
        isValid: d.isValid !== false,
        kind: "amount",
        remaining: round2(parseFloat(d.balance) / 20),
        used: null,
        total: null,
        unit: "CNY", // 同上,统一 ¥
      };
    },
  },
];

if (typeof module !== "undefined")
  module.exports = { PROVIDERS, POLL, SECRET_STORE, round1, round2 };
