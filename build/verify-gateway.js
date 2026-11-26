// 本地模型网关前端冒烟（含双向哨兵自检）：验证三类上游接入、代理默认开启、无尾斜杠地址、
// 密钥箱同密钥多次上架、代理开关失败回滚、详情展开状态保留以及思维链测试台。
// 运行需本机或 NODE_PATH 中已安装 jsdom。
const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");
const { JSDOM } = require("jsdom");

const root = path.resolve(__dirname, "..");
const html = fs.readFileSync(path.join(root, "app/src/index.html"), "utf8");
let gatewayJs = fs.readFileSync(path.join(root, "app/src/gateway-view.js"), "utf8");
let api = require("./lib/app-sources").apiSource();
const statePath = path.join(root, "app/src/gateway-ui-state.js");
let gatewayStateJs = fs.existsSync(statePath) ? fs.readFileSync(statePath, "utf8") : "";
const sentinel = process.env.GATEWAY_SENTINEL;
const newSentinels = {
  models: ["view", "avail.map((m) =>", "avail.filter((m) => !filter || m.toLowerCase().includes(filter)).map((m) =>", "G02 完整模型节点"],
  drafts: ["state", "restore(box, snap) {", "restore(box, snap) { return;", "G14 必要重绘保留当前脏字段"],
  diag: ["view", "renderDiagnostics(); // diagnostic-only", 'renderGateway(snapshotOpenState(byId("gatewayBox")), captureAddDraft()); // diagnostic-only', "G14 诊断局部更新保节点"],
  generation: ["state", "return ticket === generations[kind] && writes === 0;", "return true;", "G13 迟到完整状态成功不回滚"],
  writes: ["state", "function invalidateReads() {", "function invalidateReads() { return;", "G13 写开始失效旧读取"],
  publish: ["view", "await refreshGatewayData({ clearAdd: true }); // canonical-publish", "GW = published; renderGateway(snapshotOpenState(box), null); // canonical-publish", "G13 上架后读取canonical状态"],
  runtime: ["view", "if (runtimeWrite) await refreshGatewayData(true);", "if (false) await refreshGatewayData(true);", "G09 重启失败校准已停止状态"],
  commit: ["state", "if (current && current.revision === field.revision)", "if (current)", "G14 保存只清本次版本"],
  sync: ["state", "if (revision < syncRevision) return false;", "if (false) return false;", "G15 旧同步事件不覆盖新状态"],
};
function mutate(source, before, after, name) {
  const index = source.indexOf(before);
  if (index < 0) throw new Error(`${name} 哨兵未找到目标`);
  if (source.indexOf(before, index + before.length) >= 0) throw new Error(`${name} 哨兵目标不唯一`);
  return source.replace(before, after);
}

if (newSentinels[sentinel]) {
  const [target, before, after] = newSentinels[sentinel];
  if (target === "state") gatewayStateJs = mutate(gatewayStateJs, before, after, sentinel);
  else gatewayJs = mutate(gatewayJs, before, after, sentinel);
  if (sentinel === "writes") gatewayStateJs = mutate(gatewayStateJs,
    "return ticket === generations[kind] && writes === 0;", "return ticket === generations[kind];", sentinel);
} else if (process.env.GATEWAY_SENTINEL === "cmd") {
  api = mutate(api, 'invoke("gateway_publish"', 'invoke("__missing_gateway_publish__"', "cmd");
} else if (process.env.GATEWAY_SENTINEL === "proxy") {
  const marker = "const defaultProxyChecked = draft ? draft.proxy : true;";
  gatewayJs = mutate(gatewayJs, marker, "const defaultProxyChecked = draft ? draft.proxy : false;", "proxy");
} else if (process.env.GATEWAY_SENTINEL === "tabs") {
  const marker = '${GW_TAB === "tools" ? "" : "hidden"}';
  gatewayJs = mutate(gatewayJs, marker, "", "tabs");
} else if (process.env.GATEWAY_SENTINEL === "wire") {
  const marker = 'wire: byId("gwNewWire")?.value,';
  gatewayJs = mutate(gatewayJs, marker, 'wire: "chat_completions",', "wire");
} else if (!process.argv.includes("--behavior-only")) {
  const probeCmd = spawnSync(process.execPath, [__filename], {
    env: { ...process.env, GATEWAY_SENTINEL: "cmd" },
    encoding: "utf8",
    timeout: 15000,
  });
  if (probeCmd.status !== 1 || !probeCmd.stderr.includes("FAIL:")) {
    throw new Error("网关冒烟自检失败：未知命令哨兵未触发 FAIL");
  }
  console.log("  ok   注入未知 IPC 命令时冒烟变红");

  const probeProxy = spawnSync(process.execPath, [__filename], {
    env: { ...process.env, GATEWAY_SENTINEL: "proxy" },
    encoding: "utf8",
    timeout: 15000,
  });
  if (probeProxy.status !== 1 || !probeProxy.stderr.includes("默认开启系统代理")) {
    throw new Error("网关冒烟自检失败：代理默认值哨兵未触发 FAIL");
  }
  console.log("  ok   关闭新渠道默认代理时冒烟变红");

  const probeTabs = spawnSync(process.execPath, [__filename], {
    env: { ...process.env, GATEWAY_SENTINEL: "tabs" },
    encoding: "utf8",
    timeout: 15000,
  });
  if (probeTabs.status !== 1 || !probeTabs.stderr.includes("分栏默认隐藏")) {
    throw new Error("网关冒烟自检失败：分栏收起哨兵未触发 FAIL");
  }
  console.log("  ok   分栏被展开平铺时冒烟变红");

  const probeWire = spawnSync(process.execPath, [__filename], {
    env: { ...process.env, GATEWAY_SENTINEL: "wire" },
    encoding: "utf8",
    timeout: 15000,
  });
  if (probeWire.status !== 1 || !probeWire.stderr.includes("FAIL: 上架 Responses 发送原生协议与手动模型")) {
    throw new Error("网关冒烟自检失败：Responses 协议哨兵未触发指定 FAIL");
  }
  console.log("  ok   Responses 上架误传 Chat Completions 时冒烟变红");
  for (const [name, spec] of Object.entries(newSentinels)) {
    const probe = spawnSync(process.execPath, [__filename, "--behavior-only"], {
      env: { ...process.env, GATEWAY_SENTINEL: name }, encoding: "utf8", timeout: 30000,
    });
    if (probe.status !== 1 || !probe.stderr.includes(`FAIL: ${spec[3]}`)) {
      throw new Error(`网关 ${name} 哨兵未触发指定 FAIL (status=${probe.status}, error=${probe.error || ""})\n${probe.stderr}`);
    }
    console.log(`  ok   ${name} 坏样本触发指定行为断言`);
  }
} else if (sentinel) {
  throw new Error(`未知网关哨兵：${sentinel}`);
}

const dom = new JSDOM(html, {
  runScripts: "outside-only",
  pretendToBeVisual: true,
  url: "http://localhost/",
});
let w = dom.window;
let doc = w.document;
const calls = [];
const toasts = [];
let failNextProxyUpdate = false;
let failNextTestChat = false;
let holdNextTestChat = false;
let resolveHeldTestChat;
let finishUnpublish;
const heldReads = { gateway_status: [], gateway_diagnostics: [] };
const heldWrites = [];
let failNextRuntime = false;
let failNextStatus = false;
let stalePublishReply = false;
const listeners = new Map();
const clone = (value) => JSON.parse(JSON.stringify(value));
const settle = () => new Promise((resolve) => setImmediate(resolve));
function barrier(queue) {
  let release;
  const promise = new Promise((resolve) => { release = resolve; });
  const entry = { promise, release, started: false };
  queue.push(entry);
  return entry;
}
async function snapshotRead(cmd, value) {
  const snapshot = clone(value);
  const held = heldReads[cmd].shift();
  if (held) {
    held.started = true;
    const error = await held.promise;
    if (error) throw error;
  }
  return snapshot;
}
async function waitWrite() {
  const held = heldWrites.shift();
  if (held) {
    held.started = true;
    const error = await held.promise;
    if (error) throw error;
  }
}
let diagnosticsState = [
  { id: 1, timestamp: 1000, channelId: "ch_existing", channelAlias: "main", source: "openai_compat",
    model: "main/deepseek-r1", stream: true, status: 200, latencyMs: 320, firstByteMs: 110, frameCount: 4,
    transforms: [], thinkingChars: 42, contentChars: 12, toolCallCount: 1, toolCallChars: 18,
    finishReason: "tool_calls", promptTokens: 10, completionTokens: 20, diagNote: null, error: null },
  { id: 2, timestamp: 999, channelId: "ch_existing", channelAlias: "main", source: "openai_compat",
    model: "main/empty-check", stream: true, status: 200, latencyMs: 1650, firstByteMs: 1600, frameCount: 2,
    transforms: [], thinkingChars: 0, contentChars: 0, toolCallCount: 0, toolCallChars: 0,
    finishReason: "content_filter", promptTokens: 15, completionTokens: 0,
    diagNote: "上游已结束（finish_reason=content_filter，2 帧），未返回文本或工具调用", error: null },
];

const vaultItems = [
  {
    id: "v1",
    platform: "OpenAI",
    label: "主力 Key",
    hint: "sk-12...abcd",
    note: "",
    apiEndpoint: "https://api.example.com/v1",
    website: "",
    createdAt: 100,
    updatedAt: 100,
  },
  {
    id: "v2",
    platform: "Gemini",
    label: "Google Key",
    hint: "AIza...9999",
    note: "",
    apiEndpoint: "",
    website: "",
    createdAt: 101,
    updatedAt: 101,
  },
];

let statusState = {
  port: 18771,
  lan: true,
  auth: true,
  running: true,
  startupError: null,
  syncState: { pending: false, error: null, revision: 0 },
  lanIps: [{ name: "WLAN", address: "192.168.1.88" }],
  presets: [
    { id: "custom", name: "自定义 OpenAI 端点", baseUrl: "", defaultAlias: "custom", hint: "" },
    { id: "deepseek", name: "DeepSeek 官方", baseUrl: "https://api.deepseek.com/v1", defaultAlias: "deepseek", hint: "" },
  ],
  agentPlatformDefaults: {
    baseUrl: "https://aiplatform.googleapis.com/v1/projects/project-11ce9e7c-f85d-4842-951/locations/global/endpoints/openapi",
    models: ["google/gemini-3.8-flash", "google/gemini-3.1-flash-lite"],
  },
  channels: [
    {
      channelId: "ch_existing",
      vaultId: "v1",
      noUpstreamKey: false,
      source: "openai_compat",
      template: "custom",
      baseUrl: "https://api.example.com/v1",
      alias: "main",
      host: "api.example.com",
      label: "主力 Key",
      platform: "OpenAI",
      fetchedModels: ["gpt-4o-mini", "deepseek-r1"],
      manualModels: [],
      availableModels: ["gpt-4o-mini", "deepseek-r1"],
      allowedModels: null,
      systemProxy: true,
      customHeaders: {},
      thinkingMode: "auto",
      thoughtPolicy: "follow_client",
      count: 2,
      totalCount: 2,
      models: ["main/gpt-4o-mini", "main/deepseek-r1"],
      stale: false,
      lastStatus: "ok",
      lastError: null,
      lastCheckedAt: 1000,
    },
  ],
  vault: vaultItems,
  diagCount: 0,
};

function installWindow() {
w.$ = (id) => doc.getElementById(id);
w.esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
w.showToast = (msg, bad) => toasts.push({ msg, bad: !!bad });
w.confirm = () => true;
w.CSS = w.CSS || { escape: (s) => String(s).replace(/["\\]/g, "\\$&") };
w.listen = async (event, callback) => {
  if (event !== "gateway-sync-status") throw new Error(`未登记事件：${event}`);
  listeners.set(event, callback);
  return () => listeners.delete(event);
};

// 严格 invoke 桩：未知命令必须抛出错误，防止静默假绿
w.invoke = async (cmd, args = {}) => {
  calls.push({ cmd, args });
  switch (cmd) {
    case "gateway_status":
      if (failNextStatus) { failNextStatus = false; throw new Error("模拟状态读取失败"); }
      return snapshotRead(cmd, statusState);
    case "gateway_diagnostics":
      return snapshotRead(cmd, diagnosticsState);
    case "gateway_clear_diagnostics":
      await waitWrite();
      diagnosticsState = [];
      return;
    case "gateway_start":
    case "gateway_restart":
    case "gateway_stop":
      await waitWrite();
      if (failNextRuntime) {
        failNextRuntime = false;
        statusState.running = false;
        statusState.startupError = "模拟新端口被占用";
        throw new Error("模拟重启失败（原始错误）");
      }
      statusState.running = cmd !== "gateway_stop";
      statusState.startupError = null;
      if (cmd !== "gateway_stop") Object.assign(statusState, args);
      return clone(statusState);
    case "gateway_refresh_channel":
      await waitWrite();
      return clone(statusState);
    case "gateway_test_channel":
      return { ok: true, latencyMs: 12, fetchedCount: 2 };
    case "gateway_publish": {
      await waitWrite();
      const inp = args.input;
      const ch = {
        channelId: "ch_new",
        vaultId: inp.vaultId,
        noUpstreamKey: !!inp.noUpstreamKey,
        source: inp.source,
        wire: inp.wire,
        template: inp.template || "",
        baseUrl: inp.baseUrl,
        alias: inp.alias,
        host: "host.local",
        label: "新渠道",
        platform: "Gemini",
        fetchedModels: ["google/gemini-3.8-flash"],
        manualModels: inp.manualModels || [],
        availableModels: ["google/gemini-3.8-flash", ...(inp.manualModels || [])],
        allowedModels: null,
        systemProxy: inp.systemProxy,
        customHeaders: {},
        thinkingMode: inp.thinkingMode || "auto",
        thoughtPolicy: inp.thoughtPolicy || "follow_client",
        count: 1,
        totalCount: 1,
        models: [`${inp.alias}/google/gemini-3.8-flash`],
        stale: false,
        lastStatus: "ok",
        lastError: null,
        lastCheckedAt: 1001,
      };
      statusState.channels.push(ch);
      if (stalePublishReply) {
        stalePublishReply = false;
        return { ...clone(statusState), port: 12345, running: false };
      }
      return clone(statusState);
    }
    case "gateway_update_channel": {
      if (failNextProxyUpdate) {
        failNextProxyUpdate = false;
        throw new Error("模拟代理更新失败");
      }
      const inp = clone(args.input);
      await waitWrite();
      const ch = statusState.channels.find((c) => c.channelId === inp.channelId);
      if (!ch) throw new Error("channel not found");
      for (const key of ["vaultId", "alias", "baseUrl", "systemProxy", "wire", "thinkingMode", "thoughtPolicy", "manualModels", "customHeaders", "allowedModels"]) {
        if (inp[key] !== undefined) ch[key] = clone(inp[key]);
      }
      ch.availableModels = [...new Set([...ch.fetchedModels, ...ch.manualModels])];
      const enabled = ch.allowedModels ? ch.availableModels.filter((m) => ch.allowedModels.includes(m)) : ch.availableModels;
      ch.models = enabled.map((m) => `${ch.alias}/${m}`);
      ch.count = enabled.length;
      ch.totalCount = ch.availableModels.length;
      return clone(statusState);
    }
    case "gateway_unpublish": {
      const error = await new Promise((resolve) => { finishUnpublish = resolve; });
      if (error) throw error;
      statusState.channels = statusState.channels.filter((c) => c.channelId !== args.channelId);
      return;
    }
    case "gateway_test_chat":
      if (holdNextTestChat) {
        holdNextTestChat = false;
        await new Promise((resolve) => { resolveHeldTestChat = resolve; });
        throw new Error("已取消本次联调测试");
      }
      if (failNextTestChat) {
        failNextTestChat = false;
        throw new Error("模拟联调请求失败");
      }
      return {
        ok: true,
        status: 200,
        latencyMs: 128,
        firstByteMs: 45,
        frameCount: 3,
        requestedModel: args.input.model,
        upstreamModel: "deepseek-r1",
        thinking: "先算 12+23 = 35",
        content: "35",
        toolCallCount: 0,
        toolCallChars: 0,
        finishReason: "stop",
        transforms: ["map:thinking_config(include_thoughts)"],
        promptTokens: 10,
        completionTokens: 20,
        diagNote: null,
        error: null,
      };
    case "gateway_cancel_test_chat":
      if (resolveHeldTestChat) {
        const done = resolveHeldTestChat;
        resolveHeldTestChat = null;
        done();
      }
      return true;
    case "copy_text":
      return null;
    default:
      throw new Error(`未登记的 IPC 命令：${cmd}`);
  }
};
}

const baseStatus = clone(statusState);
const baseDiagnostics = clone(diagnosticsState);
installWindow();
w.__TAURI__ = { ...w.__TAURI__, core: { invoke: w.invoke } };
w.eval(api);
if (gatewayStateJs) w.eval(gatewayStateJs);
w.eval(gatewayJs);

let failed = 0;
function ck(name, cond, detail) {
  if (!cond) {
    console.error(`  FAIL: ${name} (${detail || ""})`);
    failed++;
  } else {
    console.log(`  ok   ${name}`);
  }
}

async function resetScenario() {
  w.close();
  statusState = clone(baseStatus);
  diagnosticsState = clone(baseDiagnostics);
  heldReads.gateway_status.length = heldReads.gateway_diagnostics.length = heldWrites.length = 0;
  listeners.clear();
  w = new JSDOM(html, { runScripts: "outside-only", pretendToBeVisual: true, url: "http://localhost/" }).window;
  doc = w.document;
  installWindow();
  w.__TAURI__ = { ...w.__TAURI__, core: { invoke: w.invoke } };
  w.eval(api);
  if (gatewayStateJs) w.eval(gatewayStateJs);
  w.eval(gatewayJs);
  const box = doc.getElementById("gatewayBox");
  for (let parent = box; parent; parent = parent.parentElement) {
    parent.hidden = false;
    parent.style.display = "block";
  }
  await w.loadGateway();
}
const field = (name) => doc.querySelector(`[data-gw-${name}="ch_existing"]`);
const click = (action) => doc.querySelector(`[data-gw="${action}"]`)?.click();
function edit(el, value) {
  if (!el) throw new Error("fixture missing editable control");
  if (el.type === "checkbox") el.checked = value;
  else el.value = value;
  el.dispatchEvent(new w.Event(el.type === "checkbox" || el.tagName === "SELECT" ? "change" : "input", { bubbles: true }));
}
const diagText = () => doc.querySelector(".gw-diag-list")?.textContent || "";
const endpoint = () => doc.querySelector(".gw-endpoint-row code")?.textContent || "";
async function regressionCases() {
  if (gatewayStateJs) ck("状态helper从HTML先于view加载", html.indexOf('src="gateway-ui-state.js"') >= 0 &&
    html.indexOf('src="gateway-ui-state.js"') < html.indexOf('src="gateway-view.js"'));
  await resetScenario();
  edit(field("model-filter"), "deepseek");
  await w.loadGateway();
  ck("G02 完整模型节点", doc.querySelectorAll('[data-gw-model="ch_existing"]').length === 2);
  edit(field("model-filter"), "no-match-synthetic");
  const card = field("alias").closest("article");
  ck("G02 无匹配提示可见", [...card.querySelectorAll(".gw-empty-note")].some((el) =>
    !el.hidden && el.style.display !== "none" && el.textContent.includes("没有匹配的模型")));
  edit(field("model-filter"), "");
  click("save-channel");
  await settle();
  ck("G02 筛选重绘清空再保存不会下架隐藏模型", statusState.channels[0].allowedModels?.length === 2);
  const model = doc.querySelector('[data-gw-model="ch_existing"][value="gpt-4o-mini"]');
  if (model) edit(model, false);
  click("save-channel");
  await settle();
  ck("G02 明确取消模型仍生效", statusState.channels[0].allowedModels?.join(",") === "deepseek-r1");

  await resetScenario();
  doc.querySelector('details[data-gw-detail="ch-ch_existing"]').open = true;
  edit(field("alias"), "draft-alias");
  edit(field("baseurl"), "https://draft.example/v1");
  edit(field("manual"), "draft-model");
  edit(field("headers"), "X-Draft: yes");
  edit(field("thinking"), "fields_only");
  edit(doc.querySelector('[data-gw-card="ch_existing"] [data-gw-wire]'), "responses");
  edit(field("vault-edit"), "v2");
  edit(field("proxy"), false);
  edit(doc.querySelector('[data-gw-model="ch_existing"][value="gpt-4o-mini"]'), false);
  const aliasNode = field("alias");
  aliasNode.focus();
  aliasNode.setSelectionRange(2, 7);
  click("refresh-diag");
  await settle();
  ck("G14 诊断局部更新保节点", field("alias") === aliasNode && doc.activeElement === aliasNode && aliasNode.selectionStart === 2);
  click("clear-diag");
  await settle();
  ck("G14 清空诊断保节点和脏勾选", field("alias") === aliasNode && field("alias").value === "draft-alias" &&
    !doc.querySelector('[data-gw-model="ch_existing"][value="gpt-4o-mini"]').checked && diagText().includes("暂无请求记录"));
  const held = barrier(heldReads.gateway_status);
  const refresh = w.loadGateway();
  edit(field("alias"), "waiting-new-input");
  edit(doc.getElementById("gwAlias"), "new-add-draft");
  edit(doc.getElementById("gwPort"), "18882");
  aliasNode.setSelectionRange(3, 8);
  held.release();
  await refresh;
  ck("G14 必要重绘保留当前脏字段", field("alias").value === "waiting-new-input" &&
    field("baseurl").value === "https://draft.example/v1" && field("manual").value === "draft-model" &&
    field("headers").value === "X-Draft: yes" && field("thinking").value === "fields_only" &&
    field("vault-edit").value === "v2" && !field("proxy").checked &&
    doc.querySelector('[data-gw-card="ch_existing"] [data-gw-wire]').value === "responses" &&
    doc.getElementById("gwAlias").value === "new-add-draft" && doc.getElementById("gwPort").value === "18882");
  ck("G14 必要重绘保留焦点选区与勾选", doc.activeElement === field("alias") && field("alias").selectionStart === 3 &&
    field("alias").selectionEnd === 8 && !doc.querySelector('[data-gw-model="ch_existing"][value="gpt-4o-mini"]').checked);
  statusState.channels[0].systemProxy = false;
  statusState.channels[0].lastError = "canonical-derived-note";
  await w.loadGateway();
  ck("G14 未编辑字段采用canonical派生状态", !field("quick-proxy").checked && doc.querySelector('.gw-channel').textContent.includes("canonical-derived-note"));

  await resetScenario();
  edit(field("alias"), "submitted");
  const save = barrier(heldWrites);
  click("save-channel");
  await settle();
  ck("真实保存夹具等待而非仅桩代理", save.started && statusState.channels[0].alias === "main");
  edit(field("alias"), "typed-after-submit");
  save.release();
  await settle();
  ck("G14 保存只清本次版本", statusState.channels[0].alias === "submitted" && field("alias").value === "typed-after-submit");
  click("save-channel");
  await settle();
  statusState.channels[0].alias = "external-canonical";
  await w.loadGateway();
  ck("G14 成功提交字段不再覆盖后续canonical", field("alias").value === "external-canonical");
  edit(field("headers"), "X-Retry: draft");
  const failSave = barrier(heldWrites);
  click("save-channel");
  await settle();
  failSave.release(new Error("模拟配置保存失败"));
  await settle();
  ck("G14 保存失败保留草稿并恢复持锁按钮", field("headers").value === "X-Retry: draft" &&
    !doc.querySelector('[data-gw="save-channel"]').disabled && toasts.some((t) => t.bad && t.msg.includes("模拟配置保存失败")));

  await resetScenario();
  const old = barrier(heldReads.gateway_status);
  const loadOld = w.loadGateway();
  statusState.port = 18881;
  statusState.running = false;
  await w.loadGateway();
  old.release();
  await loadOld;
  ck("G13 迟到完整状态成功不回滚", endpoint().includes(":18881/v1") && !!doc.querySelector('[data-gw="start"]'));
  const oldError = barrier(heldReads.gateway_status);
  const errorLoad = w.loadGateway();
  statusState.port = 18883;
  await w.loadGateway();
  oldError.release(new Error("迟到读取失败"));
  await errorLoad;
  ck("G13 迟到完整状态失败不覆盖新页面", endpoint().includes(":18883/v1") && !doc.getElementById("gatewayBox").textContent.includes("读取网关失败"));

  await resetScenario();
  const beforeWrite = barrier(heldReads.gateway_status);
  const preLoad = w.loadGateway();
  const stopWrite = barrier(heldWrites);
  const stopNode = doc.querySelector('[data-gw="stop"]');
  click("stop");
  await settle();
  beforeWrite.release();
  await preLoad;
  ck("G13 写开始失效旧读取", doc.querySelector('[data-gw="stop"]') === stopNode && stopNode.disabled);
  const duringWrite = barrier(heldReads.gateway_status);
  const duringLoad = w.loadGateway();
  stopWrite.release();
  await settle();
  duringWrite.release();
  await duringLoad;
  ck("G13 写结束失效期间读取", !statusState.running && !!doc.querySelector('[data-gw="start"]') && endpoint().includes(":18771/v1"));
  edit(doc.getElementById("gwAlias"), "canonical-publish");
  edit(doc.getElementById("gwVault"), "v1");
  stalePublishReply = true;
  click("publish");
  await settle();
  ck("G13 上架后读取canonical状态", endpoint().includes(":18771/v1") && !!doc.querySelector('[data-gw="start"]'));
  ck("G14 成功上架清本次添加草稿", doc.getElementById("gwAlias").value === "");
  edit(doc.getElementById("gwAlias"), "submitted-add");
  edit(doc.getElementById("gwVault"), "v1");
  const publishWait = barrier(heldWrites);
  click("publish");
  await settle();
  edit(doc.getElementById("gwAlias"), "next-add-draft");
  publishWait.release();
  await settle();
  ck("G14 上架等待期间新草稿不被成功清除", doc.getElementById("gwAlias").value === "next-add-draft");

  await resetScenario();
  const fullDiag = barrier(heldReads.gateway_diagnostics);
  const fullLoad = w.loadGateway();
  diagnosticsState = [{ ...baseDiagnostics[0], model: "new-independent-diag" }];
  click("refresh-diag");
  await settle();
  fullDiag.release();
  await fullLoad;
  ck("G13 完整state与diag独立代际", diagText().includes("new-independent-diag"));
  const lateDiag = barrier(heldReads.gateway_diagnostics);
  click("refresh-diag");
  await settle();
  click("clear-diag");
  await settle();
  lateDiag.release();
  await settle();
  ck("G13 清空诊断失效旧诊断读取", diagText().includes("暂无请求记录"));

  await resetScenario();
  failNextRuntime = true;
  click("apply-restart");
  await settle();
  ck("G09 重启失败校准已停止状态", !statusState.running && !!doc.querySelector('[data-gw="start"]') &&
    toasts.some((t) => t.bad && t.msg.includes("模拟重启失败（原始错误）")));
  statusState.running = true;
  await w.loadGateway();
  failNextRuntime = true;
  failNextStatus = true;
  const errorToastStart = toasts.length;
  click("apply-restart");
  await settle();
  ck("G09 校准也失败保留原错误", toasts.slice(errorToastStart).some((t) => t.bad && t.msg.includes("模拟重启失败（原始错误）")));

  await resetScenario();
  statusState.syncState = { pending: true, error: null, revision: 1 };
  await w.loadGateway();
  const syncUi = () => doc.querySelector('[data-gw-sync-status]');
  ck("G15 status显示后台同步pending", !!syncUi() && syncUi().textContent.includes("同步中"));
  const syncListener = listeners.get("gateway-sync-status");
  ck("G15 注册后台同步状态事件", typeof syncListener === "function");
  if (syncListener) {
    const sameNode = field("alias");
    edit(sameNode, "sync-must-not-reset");
    syncListener({ payload: { pending: false, error: "synthetic sync save failed", revision: 2 } });
    ck("G15 后台保存失败可见且不重绘", syncUi()?.textContent.includes("synthetic sync save failed") && sameNode === field("alias"));
    syncListener({ payload: { pending: false, error: null, revision: 3 } });
    syncListener({ payload: { pending: true, error: "stale-sync-error", revision: 1 } });
    ck("G15 旧同步事件不覆盖新状态", !!syncUi() && !syncUi().textContent.includes("stale-sync-error") && !syncUi().textContent.includes("同步中"));
    await w.loadGateway();
    ck("G15 旧status同步快照不覆盖新事件", !syncUi()?.textContent.includes("同步中"));
  }
}

async function run() {
  await w.loadGateway();

  // 1. 分栏默认收起次要面板 & 已有渠道时添加抽屉与高级项默认收起
  const paneChannels = doc.querySelector('[data-gw-pane="channels"]');
  const paneTools = doc.querySelector('[data-gw-pane="tools"]');
  const paneNetwork = doc.querySelector('[data-gw-pane="network"]');
  ck(
    "分栏默认隐藏非当前面板",
    !!paneChannels && !paneChannels.hidden && !!paneTools && paneTools.hidden && !!paneNetwork && paneNetwork.hidden,
    `ch=${paneChannels?.hidden} tools=${paneTools?.hidden} net=${paneNetwork?.hidden}`
  );
  const addDrawer = doc.querySelector('details[data-gw-detail="add-channel"]');
  const chAdv = doc.querySelector('details[data-gw-detail="ch-adv-ch_existing"]');
  ck(
    "已有渠道时添加抽屉与高级配置默认折叠",
    !!addDrawer && !addDrawer.open && !!chAdv && !chAdv.open,
    `add=${addDrawer?.open} chAdv=${chAdv?.open}`
  );

  // 2. 地址末尾无斜杠
  const endpointCode = doc.querySelector(".gw-endpoint-row code")?.textContent || "";
  ck("接入地址末尾无斜杠(/v1)", endpointCode === "http://127.0.0.1:18771/v1", endpointCode);

  // 2. 新渠道默认开启系统代理
  const newProxy = doc.getElementById("gwNewProxy");
  ck("新建渠道默认开启系统代理", !!newProxy && newProxy.checked === true, String(newProxy?.checked));

  // 3. 三类接入 Tab 存在
  const tabs = [...doc.querySelectorAll("[data-gw-kind]")].map((x) => x.dataset.gwKind);
  ck(
    "展示三类上游接入页签",
    tabs.includes("openai_compat") && tabs.includes("agent_platform") && tabs.includes("gemini_aistudio"),
    tabs.join(",")
  );

  // 4. 同一把密钥支持重复出现在下拉框以便多端点发布
  const vaultOpts = [...doc.querySelectorAll("#gwVault option")].map((o) => o.value);
  ck("已上架密钥仍可二次选择上架(v1 在可选列中)", vaultOpts.includes("v1"), vaultOpts.join(","));

  // 5. 模型筛选 + <details> 展开态保持
  const det = doc.querySelector('details[data-gw-detail="ch-ch_existing"]');
  det.open = true;
  const filterInp = doc.querySelector('[data-gw-model-filter="ch_existing"]');
  filterInp.value = "deepseek";
  filterInp.dispatchEvent(new w.Event("input"));
  const checks = [...doc.querySelectorAll('[data-gw-model="ch_existing"]')];
  const hiddenCount = checks.filter((c) => c.closest(".gw-model-check").style.display === "none").length;
  ck("模型 checklist 搜索实时过滤", hiddenCount === 1, `hidden=${hiddenCount}`);

  // 6. 快捷开关代理失败时必须回滚状态 + toast 报错
  const quickProxy = doc.querySelector('[data-gw-quick-proxy="ch_existing"]');
  failNextProxyUpdate = true;
  quickProxy.checked = false;
  await quickProxy.onchange();
  ck(
    "代理设置写失败时回滚勾选框并报错提示",
    quickProxy.checked === true && toasts.some((t) => t.bad && t.msg.includes("模拟代理更新失败")),
    `checked=${quickProxy.checked}`
  );

  // 7. 切换到 Agent Platform 自动填入 Vertex 地址与别名
  doc.querySelector('[data-gw-kind="agent_platform"]').click();
  const baseUrlVal = doc.getElementById("gwBaseUrl").value;
  ck(
    "切换到 Agent Platform 自动填入默认端点",
    baseUrlVal.includes("aiplatform.googleapis.com") && baseUrlVal.endsWith("/endpoints/openapi"),
    baseUrlVal
  );

  // 8. 上架新渠道后检查参数 + 保留已有渠道详情展开态
  doc.getElementById("gwVault").value = "v2";
  doc.getElementById("gwAlias").value = "vertex1";
  doc.querySelector('[data-gw="publish"]').click();
  await new Promise((r) => setTimeout(r, 20));
  const pubCall = calls.find((c) => c.cmd === "gateway_publish");
  ck(
    "上架 Agent Platform 发送正确载荷且系统代理默认开启",
    !!pubCall &&
      pubCall.args.input.source === "agent_platform" &&
      pubCall.args.input.systemProxy === true &&
      pubCall.args.input.alias === "vertex1",
    JSON.stringify(pubCall?.args)
  );
  const detAfter = doc.querySelector('details[data-gw-detail="ch-ch_existing"]');
  ck("刷新重绘后保留已打开的渠道详情", !!detAfter && detAfter.open === true);
  ck("上架重绘后恢复上架和停止按钮", !doc.querySelector('[data-gw="publish"]').disabled && !doc.querySelector('[data-gw="stop"]').disabled);

  // 9. 切换到「模型与联调」页签并运行在线测试验证思维链折叠展示
  doc.querySelector('[data-gw-tab="tools"]').click();
  ck(
    "切到模型与联调页签正确切换可见面板",
    doc.querySelector('[data-gw-pane="tools"]')?.hidden === false &&
      doc.querySelector('[data-gw-pane="channels"]')?.hidden === true
  );
  doc.querySelector('[data-gw="run-test-chat"]').click();
  await new Promise((r) => setTimeout(r, 20));
  const thinkText = doc.querySelector(".gw-test-think pre")?.textContent || "";
  const contentText = doc.querySelector(".gw-test-content pre")?.textContent || "";
  ck(
    "测试台正确折叠展示思维链与正文",
    thinkText.includes("35") && contentText === "35",
    `think=${thinkText} content=${contentText}`
  );

  // 下架和刷新期间可复制地址，复制结束不能提前放开网关操作。
  // 重新进入网关，确保前一个场景的按钮状态不会影响这里。
  await w.loadGateway();
  doc.querySelector('[data-gw-tab="channels"]').click();
  doc.querySelector('[data-gw-kind="openai_compat"]').click();
  doc.querySelector('details[data-gw-detail="add-channel"]').open = true;
  doc.getElementById("gwVault").value = "v1";
  doc.getElementById("gwBaseUrl").value = "https://api.example.com/v1";
  doc.getElementById("gwAlias").value = "ar";
  const newWire = doc.getElementById("gwNewWire");
  ck("OpenAI 兼容新渠道默认 Chat Completions", newWire?.value === "chat_completions");
  newWire.value = "responses";
  newWire.dispatchEvent(new w.Event("change"));
  ck("选择 Responses 展开手动模型输入", !doc.getElementById("gwResponsesHint").hidden &&
    doc.querySelector('details[data-gw-detail="add-adv"]').open);
  doc.getElementById("gwManualModels").value = "responses-test-model";
  const removeBtn = () => doc.querySelector('[data-gw="remove"][data-id="ch_new"]');
  const gatewayButtons = () => [...doc.querySelectorAll('[data-gw="publish"], [data-gw="stop"], [data-gw="remove"]')];
  const settle = () => new Promise((resolve) => setImmediate(resolve));
  removeBtn().click();
  doc.querySelector('[data-gw="copy-local"]').click();
  await settle();
  ck("复制地址结束时网关操作仍在等待下架", gatewayButtons().every((button) => button.disabled));
  finishUnpublish(new Error("模拟下架写入失败"));
  await settle();
  ck("下架写入失败保留渠道并恢复按钮", !!removeBtn() &&
    gatewayButtons().every((button) => !button.disabled) &&
    toasts.some((t) => t.bad && t.msg.includes("模拟下架写入失败")));

  removeBtn().click();
  finishUnpublish();
  await settle();
  ck("下架重绘后恢复网关操作按钮", !removeBtn() && gatewayButtons().every((button) => !button.disabled));
  ck("下架重绘保留已填的上架草稿", doc.querySelector('details[data-gw-detail="add-channel"]').open &&
    doc.getElementById("gwVault").value === "v1" &&
    doc.getElementById("gwBaseUrl").value === "https://api.example.com/v1" &&
    doc.getElementById("gwAlias").value === "ar" &&
    doc.getElementById("gwNewWire").value === "responses" &&
    doc.getElementById("gwManualModels").value === "responses-test-model");
  doc.querySelector('[data-gw="publish"]').click();
  await settle();
  ck("下架后可直接上架保留的渠道草稿", calls.some((c) =>
    c.cmd === "gateway_publish" && c.args.input.alias === "ar"));
  const responsePayload = calls.find((c) => c.cmd === "gateway_publish" && c.args.input.alias === "ar")?.args.input;
  ck("上架 Responses 发送原生协议与手动模型", responsePayload?.wire === "responses" &&
    responsePayload.manualModels.join(",") === "responses-test-model");
  const responseCard = doc.querySelector('[data-gw-card="ch_new"]');
  ck("上架后渠道配置回显协议", responseCard?.querySelector("[data-gw-wire]")?.value === responsePayload?.wire);
  responseCard?.querySelector('[data-gw="save-channel"]')?.click();
  await settle();
  ck("保存渠道配置沿用所选协议", calls.some((c) => c.cmd === "gateway_update_channel" &&
    c.args.input.channelId === "ch_new" && c.args.input.wire === responsePayload?.wire));

  doc.querySelector('[data-gw-tab="tools"]').click();
  const diagText = doc.querySelector(".gw-diag-list")?.textContent || "";
  ck(
    "诊断列表展示首字耗时、工具调用、finishReason 与零输出说明",
    diagText.includes("首字 110ms") &&
      diagText.includes("工具 1次(18字)") &&
      diagText.includes("结束 tool_calls") &&
      diagText.includes("tok 10/20") &&
      diagText.includes("finish_reason=content_filter"),
    diagText
  );
  failNextTestChat = true;
  doc.querySelector('[data-gw="run-test-chat"]').click();
  await settle();
  const testBtn = doc.querySelector('[data-gw="run-test-chat"]');
  ck("联调请求异常后可重新发送", !testBtn.disabled && testBtn.textContent === "发送测试" &&
    toasts.some((t) => t.bad && t.msg.includes("模拟联调请求失败")));

  holdNextTestChat = true;
  doc.querySelector('[data-gw="run-test-chat"]').click();
  await settle();
  const cancelBtn = doc.querySelector('[data-gw="cancel-test-chat"]');
  ck("联调请求进行中显示取消按钮", !!cancelBtn && !cancelBtn.hidden && testBtn.disabled);
  cancelBtn.click();
  await settle();
  ck(
    "点击取消后调用后端取消命令并恢复发送按钮",
    calls.some((c) => c.cmd === "gateway_cancel_test_chat") &&
      !testBtn.disabled &&
      testBtn.textContent === "发送测试" &&
      cancelBtn.hidden &&
      toasts.some((t) => !t.bad && t.msg.includes("已取消本次联调测试"))
  );

  await regressionCases();
  w.close();
  if (failed > 0) {
    console.error(`gateway 冒烟结果: ${failed} 项失败`);
    process.exit(1);
  }
  console.log("gateway 冒烟全部通过");
}

run().catch((e) => {
  console.error("FAIL:", e);
  process.exit(1);
});
