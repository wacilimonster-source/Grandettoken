// 网关 Magpie 升级（路由组、SQLite 台账、8 客户端接入、可选 Prompt 脱敏）双向 jsdom 冒烟。
// 运行需本机或 NODE_PATH 中已安装 jsdom。
const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");
const { JSDOM } = require("jsdom");

const root = path.resolve(__dirname, "..");
const html = fs.readFileSync(path.join(root, "app/src/index.html"), "utf8");
let gatewayJs = fs.readFileSync(path.join(root, "app/src/gateway-view.js"), "utf8");
const gatewayStateJs = fs.readFileSync(path.join(root, "app/src/gateway-ui-state.js"), "utf8");
let gatewayAdvJs = fs.readFileSync(path.join(root, "app/src/gateway-advanced.js"), "utf8");

const sentinel = process.env.MAGPIE_SENTINEL;
const sentinels = {
  mask_default: [
    "enabled: false,",
    "enabled: true,",
    "脱敏默认关闭（初始未勾选）",
  ],
  mask_rollback: [
    "restoreMaskingDom(box, prevMask);",
    "/* sentinel-skip-rollback */",
    "脱敏保存失败回滚开关并报错提示",
  ],
  no_direct_apply: [
    "const prev = state.preview;",
    "const prev = state.preview || { client: 'claude-code', modelId: 'group/coding', isGroup: true, items: [{ label: 'Claude Code', action: 'fake', changed: true, configured: true, files: [] }] };",
    "接入写入必须先预览预告，未预览前不出现写入按钮",
  ],
  cmd: [
    'invoke("gateway_save_group"',
    'invoke("__missing_gateway_save_group__"',
    "保存路由组调用正式命令",
  ],
};

function mutate(source, before, after, name) {
  const index = source.indexOf(before);
  if (index < 0) throw new Error(`${name} 哨兵未找到目标`);
  if (source.indexOf(before, index + before.length) >= 0) {
    throw new Error(`${name} 哨兵目标不唯一`);
  }
  return source.replace(before, after);
}

if (sentinels[sentinel]) {
  const [before, after] = sentinels[sentinel];
  gatewayAdvJs = mutate(gatewayAdvJs, before, after, sentinel);
} else if (sentinel) {
  throw new Error(`未知 Magpie 网关哨兵：${sentinel}`);
} else if (!process.argv.includes("--behavior-only")) {
  for (const [name, spec] of Object.entries(sentinels)) {
    const probe = spawnSync(process.execPath, [__filename, "--behavior-only"], {
      env: { ...process.env, MAGPIE_SENTINEL: name },
      encoding: "utf8",
      timeout: 20000,
    });
    const combined = `${probe.stdout || ""}\n${probe.stderr || ""}`;
    if (probe.status !== 1 || (name === "cmd" ? !combined.includes("FAIL:") : !combined.includes(`FAIL: ${spec[2]}`))) {
      throw new Error(`Magpie ${name} 哨兵未触发指定 FAIL (status=${probe.status}):\n${combined}`);
    }
    console.log(`  ok   ${name} 坏样本触发指定 FAIL`);
  }
}

const dom = new JSDOM(html, {
  runScripts: "outside-only",
  pretendToBeVisual: true,
  url: "http://localhost/",
});
const w = dom.window;
const doc = w.document;
const calls = [];
const toasts = [];
let failNextMaskSave = false;

const clone = (v) => JSON.parse(JSON.stringify(v));
const settle = () => new Promise((resolve) => setImmediate(resolve));

let statusState = {
  port: 18771,
  lan: false,
  auth: true,
  running: true,
  startupError: null,
  syncState: { pending: false, error: null, revision: 1 },
  lanIps: [],
  presets: [],
  agentPlatformDefaults: { baseUrl: "https://example.invalid", models: [] },
  channels: [
    {
      channelId: "ch_existing",
      vaultId: "v1",
      noUpstreamKey: false,
      source: "openai_compat",
      wire: "chat_completions",
      template: "deepseek",
      baseUrl: "https://api.deepseek.com/v1",
      alias: "ds",
      host: "api.deepseek.com",
      label: "DeepSeek",
      platform: "DeepSeek",
      fetchedModels: ["deepseek-chat", "deepseek-reasoner"],
      manualModels: [],
      availableModels: ["deepseek-chat", "deepseek-reasoner"],
      allowedModels: null,
      systemProxy: true,
      customHeaders: {},
      thinkingMode: "auto",
      thoughtPolicy: "follow_client",
      count: 2,
      totalCount: 2,
      models: ["ds/deepseek-chat", "ds/deepseek-reasoner"],
      stale: false,
      lastStatus: "ok",
      lastError: null,
      lastCheckedAt: 1000,
    },
  ],
  vault: [{ id: "v1", platform: "DeepSeek", label: "DS Key", hint: "sk-1..2", apiEndpoint: "https://api.deepseek.com/v1" }],
  diagCount: 1,
  groups: [],
  masking: undefined,
  cooldowns: [
    { channelId: "ch_existing", model: "deepseek-reasoner", untilTs: 2000, remainingSecs: 45, reason: "HTTP 429" },
  ],
  endpoints: {
    openaiBaseUrl: "http://127.0.0.1:18771/v1",
    anthropicBaseUrl: "http://127.0.0.1:18771",
  },
  groupModels: [],
};

let usageState = {
  sinceTs: 100,
  totalRequests: 10,
  okRequests: 9,
  errorRequests: 1,
  failoverRequests: 2,
  promptTokens: 1000,
  cachedPromptTokens: 800,
  cacheCreationTokens: 100,
  completionTokens: 250,
  reasoningTokens: 60,
  cacheHitRate: 0.8,
  ttftP50Ms: 120,
  ttftP95Ms: 420,
  estimatedCostCny: 0.1234,
  pricedRequests: 6,
  byChannel: [
    { key: "ch_existing", label: "ds", requests: 10, errors: 1, promptTokens: 1000, cachedPromptTokens: 800, cacheCreationTokens: 100, completionTokens: 250, reasoningTokens: 60, estimatedCostCny: 0.1234, pricedRequests: 6 },
  ],
  byModel: [
    { key: "group/coding", label: "group/coding", requests: 10, errors: 1, promptTokens: 1000, cachedPromptTokens: 800, cacheCreationTokens: 100, completionTokens: 250, reasoningTokens: 60, estimatedCostCny: 0.1234, pricedRequests: 6 },
  ],
  recent: [
    { id: 1, ts: 1795000000, channelId: "ch_existing", channelAlias: "ds", routeGroup: "group/coding", model: "group/coding", protocol: "anthropic_messages", stream: true, status: 200, latencyMs: 520, firstByteMs: 110, promptTokens: 100, cachedPromptTokens: 80, completionTokens: 25, reasoningTokens: 5, failoverCount: 1, maskedCount: 2, estimatedCostCny: 0.012, errorCode: null },
  ],
};

let diagState = [
  {
    id: 1, timestamp: 1795000000, channelId: "ch_existing", channelAlias: "ds", source: "openai_compat",
    model: "group/coding", upstreamModel: "deepseek-chat", routeGroup: "group/coding",
    failoverCount: 1, maskedCount: 2, protocol: "anthropic_messages",
    stream: true, status: 200, latencyMs: 520, firstByteMs: 110, frameCount: 5,
    transforms: [], thinkingChars: 10, contentChars: 20, toolCallCount: 0, toolCallChars: 0,
    finishReason: "stop", promptTokens: 100, cachedPromptTokens: 80, cacheCreationTokens: 0,
    completionTokens: 25, reasoningTokens: 5, estimatedCostCny: 0.012, diagNote: null, error: null,
  },
];

const eightClients = [
  "trae", "workbuddy", "workbuddy-ai", "zcode", "dsh", "claude-code", "codex", "opencode",
].map((id) => ({
  client: id,
  short: id.toUpperCase().slice(0, 2),
  label: id === "claude-code" ? "Claude Code" : id === "codex" ? "Codex" : id === "opencode" ? "OpenCode" : id,
  installed: true,
  running: false,
  process: `${id}.exe`,
  roots: [`~/${id}`],
  files: [`~/${id}/config`],
  models: [],
  selected: null,
  learnedPrefix: null,
  note: "测试就绪",
}));

let logState = [];

function installWindow() {
  w.$ = (id) => doc.getElementById(id);
  w.esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  w.showToast = (msg, bad) => toasts.push({ msg, bad: !!bad });
  w.confirm = () => true;
  w.CSS = w.CSS || { escape: (s) => String(s).replace(/["\\]/g, "\\$&") };
  w.listen = async () => () => {};
  w.invoke = async (cmd, args = {}) => {
    calls.push({ cmd, args: clone(args) });
    switch (cmd) {
      case "gateway_status":
        return clone(statusState);
      case "gateway_diagnostics":
        return clone(diagState);
      case "gateway_secret":
        return "gw_magpie_smoke_secret_01";
      case "copy_text":
        return null;
      case "gateway_save_group": {
        const inp = args.input;
        const gr = {
          id: inp.id,
          name: inp.name || inp.id,
          strategy: inp.strategy || "order",
          sticky: inp.sticky !== false,
          stickyTtlSecs: inp.stickyTtlSecs || 1800,
          cooldownSecs: inp.cooldownSecs || 60,
          targets: clone(inp.targets || []),
        };
        statusState.groups = statusState.groups.filter((g) => g.id !== gr.id);
        statusState.groups.push(gr);
        statusState.groupModels = statusState.groups.map((g) => `group/${g.id}`);
        return clone(statusState);
      }
      case "gateway_delete_group":
        statusState.groups = statusState.groups.filter((g) => g.id !== args.id);
        statusState.groupModels = statusState.groups.map((g) => `group/${g.id}`);
        return clone(statusState);
      case "gateway_set_masking":
        if (failNextMaskSave) {
          failNextMaskSave = false;
          throw new Error("模拟脱敏配置落盘失败");
        }
        statusState.masking = clone(args.input);
        return clone(statusState);
      case "gateway_usage_summary":
        return clone(usageState);
      case "gateway_clear_usage":
        usageState.totalRequests = 0;
        usageState.okRequests = 0;
        usageState.byChannel = [];
        usageState.byModel = [];
        usageState.recent = [];
        return null;
      case "gateway_inject_clients":
        return { clients: clone(eightClients), log: clone(logState) };
      case "gateway_inject_preview":
        return [{
          client: args.client,
          label: "Claude Code",
          files: ["~/.claude/settings.json"],
          action: "局部合并 env 中的 ANTHROPIC_BASE_URL / AUTH_TOKEN / MODEL",
          changed: true,
          writesSecret: false,
          writesSelection: true,
          configured: false,
          removal: null,
          blocked: null,
          notes: ["本地网关接入：真实上游密钥留在 Windows 凭据管理器", "DPAPI 备份"],
        }];
      case "gateway_inject_apply":
        logState = [{
          stamp: "20261123",
          at: 1795000010,
          client: args.client,
          label: "Claude Code",
          model: args.modelId,
          backup: "~/.claude/backup-inject/20261123",
          backupPath: "C:\\mock\\.claude\\backup-inject\\20261123",
          backupOk: true,
          added: 1,
          updated: 0,
          removed: 0,
          selected: true,
        }];
        eightClients.find((c) => c.client === args.client).models = [{ id: args.modelId, source: "gateway" }];
        return [{
          client: args.client,
          label: "Claude Code",
          stamp: "20261123",
          backup: "~/.claude/backup-inject/20261123",
          backupPath: "C:\\mock\\.claude\\backup-inject\\20261123",
          added: 1,
          updated: 0,
          removed: 0,
          selected: true,
          effect: "打开或重启 Claude Code 后立即生效",
          notes: [],
          error: null,
        }];
      case "gateway_inject_remove":
        return [{
          client: args.client, label: "Claude Code", stamp: "20261123",
          backup: "~/.claude/backup-inject/20261123", backupPath: "C:\\mock\\.claude\\backup-inject\\20261123",
          added: 0, updated: 0, removed: 1, selected: false, effect: "下一个会话生效", notes: [], error: null,
        }];
      case "inject_rollback":
        return "已从备份还原";
      default:
        throw new Error(`未登记 IPC 命令：${cmd}`);
    }
  };
}

let failed = 0;
function ck(name, ok, extra = "") {
  if (!ok) {
    failed += 1;
    console.error(`FAIL: ${name}${extra ? ` -> ${extra}` : ""}`);
  } else {
    console.log(`  ok   ${name}`);
  }
}

async function run() {
  installWindow();
  w.eval(`${gatewayStateJs}\n//# sourceURL=gateway-ui-state.js`);
  w.eval(`${gatewayJs}\n//# sourceURL=gateway-view.js`);
  w.eval(`${gatewayAdvJs}\n//# sourceURL=gateway-advanced.js`);

  await w.loadGateway();
  await settle();

  // 1. Diagnostics extended badges
  doc.querySelector('[data-gw-tab="tools"]').click();
  const diagTxt = doc.querySelector(".gw-diag-list")?.textContent || "";
  ck("诊断列表展示缓存命中率、切备与脱敏次数",
    diagTxt.includes("缓存命中 80%") && diagTxt.includes("切备 1") && diagTxt.includes("脱敏 2"), diagTxt);

  // 2. Virtual routing groups tab & CRUD
  doc.querySelector('[data-gw-tab="groups"]').click();
  await settle();
  const grpPane = doc.querySelector('[data-gw-pane="groups"]');
  ck("路由组面板展示冷却候选提示", grpPane.textContent.includes("ds/deepseek-reasoner") && grpPane.textContent.includes("HTTP 429"));
  doc.getElementById("gwGrpId").value = "coding";
  doc.getElementById("gwGrpName").value = "编码容灾池";
  doc.getElementById("gwGrpStrategy").value = "expiring";
  doc.querySelector('[data-gw-adv="save-group"]').click();
  await settle();
  ck("保存路由组调用正式命令", calls.some((c) => c.cmd === "gateway_save_group" && c.args.input.id === "coding" && c.args.input.strategy === "expiring"));
  ck("保存后渲染 group/coding 卡片", doc.querySelector('[data-gw-pane="groups"]').textContent.includes("group/coding"));
  doc.querySelector('[data-gw-adv="copy-group"][data-group-id="coding"]')?.click();
  await settle();
  ck("一键复制 group/<id>", calls.some((c) => c.cmd === "copy_text" && c.args.text === "group/coding"));

  // 3. Usage ledger tab (1/7/30d, cache hit, TTFT, cost, clear)
  doc.querySelector('[data-gw-tab="usage"]').click();
  await settle();
  const usagePane = doc.querySelector('[data-gw-pane="usage"]');
  ck("用量账本显示缓存命中率与 P50/P95 TTFT 与估算费用",
    usagePane.textContent.includes("80.0%") &&
    usagePane.textContent.includes("120 / 420 ms") &&
    usagePane.textContent.includes("0.1234"));
  doc.querySelector('[data-gw-adv="usage-days"][data-days="30"]').click();
  await settle();
  ck("切换 30 天时间窗重新拉取台账", calls.some((c) => c.cmd === "gateway_usage_summary" && c.args.days === 30));

  // 4. Connect tab (8 clients, preview gate, apply, rollback, masking default off + failure rollback)
  doc.querySelector('[data-gw-tab="connect"]').click();
  await settle();
  const connPane = doc.querySelector('[data-gw-pane="connect"]');
  ck("接入与安全页签展示 Anthropic /v1/messages 入口与全部 8 个客户端",
    connPane.textContent.includes("http://127.0.0.1:18771") &&
    connPane.textContent.includes("Claude Code") &&
    connPane.textContent.includes("Codex") &&
    connPane.textContent.includes("OpenCode") &&
    connPane.querySelectorAll(".gw-adv-client-row").length === 8);

  const maskCb = doc.getElementById("gwMaskEnabled");
  ck("脱敏默认关闭（初始未勾选）", !!maskCb && maskCb.checked === false);

  // Preview gate sentinel check BEFORE clicking preview
  ck("接入写入必须先预览预告，未预览前不出现写入按钮",
    !connPane.querySelector('[data-gw-adv="apply-inject"]'));

  // Failure rollback on masking toggle
  maskCb.checked = true;
  failNextMaskSave = true;
  doc.querySelector('[data-gw-adv="save-masking"]').click();
  await settle();
  ck("脱敏保存失败回滚开关并报错提示",
    doc.getElementById("gwMaskEnabled").checked === false &&
    toasts.some((t) => t.bad && t.msg.includes("模拟脱敏配置落盘失败")));

  // Success enable masking
  doc.getElementById("gwMaskEnabled").checked = true;
  doc.getElementById("gwMaskPatterns").value = "my-internal-secret";
  doc.querySelector('[data-gw-adv="save-masking"]').click();
  await settle();
  ck("手动开启脱敏成功落盘",
    calls.some((c) => c.cmd === "gateway_set_masking" && c.args.input.enabled === true && c.args.input.customPatterns[0] === "my-internal-secret"));

  // Preview + Apply + Rollback on Claude Code
  doc.querySelector('[data-gw-adv="preview-inject"][data-client="claude-code"]').click();
  await settle();
  ck("点击预览后调用 gateway_inject_preview 并显示确认按钮",
    calls.some((c) => c.cmd === "gateway_inject_preview" && c.args.client === "claude-code" && c.args.modelId === "group/coding" && c.args.isGroup === true) &&
    !!doc.querySelector('[data-gw-adv="apply-inject"]'));

  doc.querySelector('[data-gw-adv="apply-inject"]').click();
  await settle();
  ck("确认写入调用 gateway_inject_apply 并生成回滚记录",
    calls.some((c) => c.cmd === "gateway_inject_apply" && c.args.client === "claude-code" && c.args.isGroup === true) &&
    !!doc.querySelector('[data-gw-adv="rollback-inject"]'));

  doc.querySelector('[data-gw-adv="rollback-inject"]').click();
  await settle();
  ck("一键回滚调用 inject_rollback",
    calls.some((c) => c.cmd === "inject_rollback"));

  // Delete route group
  doc.querySelector('[data-gw-tab="groups"]').click();
  await settle();
  doc.querySelector('[data-gw-adv="delete-group"][data-group-id="coding"]').click();
  await settle();
  ck("删除路由组调用 gateway_delete_group 并移除卡片",
    calls.some((c) => c.cmd === "gateway_delete_group" && c.args.id === "coding") &&
    !doc.querySelector('[data-gw-pane="groups"]').textContent.includes("group/coding"));

  // Clear usage
  doc.querySelector('[data-gw-tab="usage"]').click();
  await settle();
  doc.querySelector('[data-gw-adv="clear-usage"]').click();
  await settle();
  ck("清空台账调用 gateway_clear_usage", calls.some((c) => c.cmd === "gateway_clear_usage"));

  w.close();
  if (failed > 0) {
    console.error(`gateway-magpie 冒烟结果: ${failed} 项失败`);
    process.exit(1);
  }
  console.log("gateway-magpie 冒烟全部通过");
}

run().catch((e) => {
  console.error("FAIL:", e);
  process.exit(1);
});
