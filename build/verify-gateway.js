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

if (process.env.GATEWAY_SENTINEL === "cmd") {
  gatewayJs = gatewayJs.replace('invoke("gateway_publish"', 'invoke("__missing_gateway_publish__"');
} else if (process.env.GATEWAY_SENTINEL === "proxy") {
  const marker = "const defaultProxyChecked = draft ? draft.proxy : true;";
  if (!gatewayJs.includes(marker)) throw new Error("proxy 哨兵未找到目标行");
  gatewayJs = gatewayJs.replace(marker, "const defaultProxyChecked = draft ? draft.proxy : false;");
} else if (process.env.GATEWAY_SENTINEL === "tabs") {
  const marker = '${GW_TAB === "tools" ? "" : "hidden"}';
  if (!gatewayJs.includes(marker)) throw new Error("tabs 哨兵未找到目标行");
  gatewayJs = gatewayJs.replace(marker, "");
} else if (process.env.GATEWAY_SENTINEL === "wire") {
  const marker = 'wire: byId("gwNewWire")?.value,';
  if (!gatewayJs.includes(marker)) throw new Error("Responses 哨兵未找到上架协议");
  gatewayJs = gatewayJs.replace(marker, 'wire: "chat_completions",');
} else {
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
let failNextProxyUpdate = false;
let failNextTestChat = false;
let holdNextTestChat = false;
let resolveHeldTestChat;
let finishUnpublish;

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

w.$ = (id) => doc.getElementById(id);
w.esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
w.showToast = (msg, bad) => toasts.push({ msg, bad: !!bad });
w.confirm = () => true;
w.CSS = w.CSS || { escape: (s) => String(s).replace(/["\\]/g, "\\$&") };

// 严格 invoke 桩：未知命令必须抛出错误，防止静默假绿
w.invoke = async (cmd, args = {}) => {
  calls.push({ cmd, args });
  switch (cmd) {
    case "gateway_status":
      return JSON.parse(JSON.stringify(statusState));
    case "gateway_diagnostics":
      return [
        {
          id: 1,
          timestamp: 1000,
          channelId: "ch_existing",
          channelAlias: "main",
          source: "openai_compat",
          model: "main/deepseek-r1",
          stream: true,
          status: 200,
          latencyMs: 320,
          firstByteMs: 110,
          frameCount: 4,
          transforms: [],
          thinkingChars: 42,
          contentChars: 12,
          toolCallCount: 1,
          toolCallChars: 18,
          finishReason: "tool_calls",
          promptTokens: 10,
          completionTokens: 20,
          diagNote: null,
          error: null,
        },
        {
          id: 2,
          timestamp: 999,
          channelId: "ch_existing",
          channelAlias: "main",
          source: "openai_compat",
          model: "main/empty-check",
          stream: true,
          status: 200,
          latencyMs: 1650,
          firstByteMs: 1600,
          frameCount: 2,
          transforms: [],
          thinkingChars: 0,
          contentChars: 0,
          toolCallCount: 0,
          toolCallChars: 0,
          finishReason: "content_filter",
          promptTokens: 15,
          completionTokens: 0,
          diagNote: "上游已结束（finish_reason=content_filter，2 帧），未返回文本或工具调用",
          error: null,
        },
      ];
    case "gateway_publish": {
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
      return JSON.parse(JSON.stringify(statusState));
    }
    case "gateway_update_channel": {
      if (failNextProxyUpdate) {
        failNextProxyUpdate = false;
        throw new Error("模拟代理更新失败");
      }
      const inp = args.input;
      const ch = statusState.channels.find((c) => c.channelId === inp.channelId);
      if (!ch) throw new Error("channel not found");
      if (typeof inp.systemProxy === "boolean") ch.systemProxy = inp.systemProxy;
      if (inp.wire) ch.wire = inp.wire;
      return JSON.parse(JSON.stringify(statusState));
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
