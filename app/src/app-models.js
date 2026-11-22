// ───────────── 模型页:多来源 + 弹层确认 ─────────────
// 设计见 docs/design/design-models-multi-source.html。三条不变的口径:
//   ① 列表只列「这个来源能用的 ∪ 已配置到这个来源的」,别的永不出现;
//   ② 有价格的来源按单价从高到低排,没价格的排在末尾并写明原因;
//   ③ 写入/取消写入的确认走**弹层**(以前长在列表底部,得滚到底才看得见)。
// 密钥仍由后端现读现写、不回前端;「复制三件套」也是后端拼好字符串。
const MDL = {
  views: {},                 // 来源 id → get_model_catalog 的返回
  src: "",                   // 当前来源
  sources: [],               // 后端给的来源表 [{id,name,hasPrice,listPublic}]
  clients: [],               // inject_state 的客户端状态
  stateError: "",            // inject_state 失败原因(与"没装客户端"分开显示)
  log: [],                   // 写入台账(不含密钥)
  q: "",                     // 搜索词
  sheet: null,               // 当前弹层内容(预告 / 结果 / 客户端详情)
  loading: false,
};
/// 既能当余额渠道、又能当模型来源的三个渠道(id 与来源 id 一致)。
const MODEL_SOURCES = new Set(["4sapi", "hapi", "opencode-go"]);
const CLIENT_NAME = {
  trae: "Trae",
  workbuddy: "WorkBuddy",
  "workbuddy-ai": "WorkBuddy AI",
  zcode: "ZCode",
  dsh: "DeepSeek Harness",
};
/// 唯一「边跑边写」的客户端:DSH 的 profile 按请求解析,它自己的设置层与凭据层都会
/// 收敛外部编辑 —— 所以运行中不是「先退出」,而是「下一个请求生效」。
const HOT_CLIENTS = new Set(["dsh"]);

/** 单价显示。价目表口径是元/百万 token;按次计费的是元/次。
    金额走统一的 money(千分位 + 两位);只有 <¥1 的档位保留三位小数 ——
    0.005/百万 token 用两位口径会被抹成 0.01,单价信息就没了。 */
function mdlMoney(v) {
  if (v === null || v === undefined) return "—";
  return v < 1 ? "¥" + Number(v).toFixed(3) : money(v);
}
function mdlPriceText(r) {
  const p = r.price || {};
  if (p.perCall !== null && p.perCall !== undefined) return mdlMoney(p.perCall) + " / 次";
  if (p.input === null || p.input === undefined) return "";
  const out = p.output === null || p.output === undefined ? "—" : mdlMoney(p.output);
  return "入 " + mdlMoney(p.input) + " · 出 " + out + " / 百万 token" + (p.tiered ? " *" : "");
}
/** 「贵」用状态色标出来 —— 选哪个模型直接决定余额能撑多久,和余额是同一类判断。 */
function mdlPriceTone(r) {
  const p = r.price || {};
  const v = p.input === null || p.input === undefined ? null : p.input;
  if (v === null) return "";
  if (v >= 100) return "bad";
  if (v >= 20) return "warn";
  return "";
}

function mdlView() {
  return MDL.views[MDL.src] || null;
}
function mdlHasPrice() {
  const v = mdlView();
  return !!(v && v.hasPrice);
}
/** 这个来源里,某个模型配到了哪些客户端。 */
function mdlDoneFor(clientId, modelId) {
  const c = MDL.clients.find((x) => x.client === clientId);
  if (!c) return false;
  return (c.models || []).some((m) => m.id === modelId && m.source === MDL.src);
}
function mdlConfiguredIds() {
  const set = new Set();
  for (const c of MDL.clients) {
    for (const m of c.models || []) if (m.source === MDL.src) set.add(m.id);
  }
  return set;
}

function mdlVisibleRows() {
  const v = mdlView();
  if (!v) return [];
  const q = MDL.q.trim().toLowerCase();
  const done = mdlConfiguredIds();
  const out = [];
  // 固定规则:列表 = 「这个来源能用的」∪「已配置到这个来源的」。
  // 取不到可用集合时只列已配置的 —— 不把"不知道能不能用"当成"能用"。
  const knowUsable = (v.usableAt || 0) > 0;
  for (const r of v.rows || []) {
    const useful = knowUsable ? r.usable === true || done.has(r.id) : done.has(r.id);
    if (!useful) continue;
    if (q) {
      const hay = (
        r.id + " " + (r.vendor || "") + " " + (r.tags || []).join(" ") + " " + (r.description || "")
      ).toLowerCase();
      if (!hay.includes(q)) continue;
    }
    out.push(r);
  }
  // 排序:有价格的来源按单价从高到低(最烧钱的在最上面),价格相同按名称;
  // 没价格的来源(订阅制 / 站点不给价)整列排在末尾,内部「已配置优先 + 名称」。
  if (v.hasPrice) {
    out.sort((a, b) => {
      const pa = (a.price || {}).input;
      const pb = (b.price || {}).input;
      const ha = pa === null || pa === undefined ? -1 : pa;
      const hb = pb === null || pb === undefined ? -1 : pb;
      if (ha !== hb) return hb - ha;
      return a.id.localeCompare(b.id);
    });
  } else {
    out.sort((a, b) => {
      const da = done.has(a.id) ? 0 : 1;
      const db = done.has(b.id) ? 0 : 1;
      return da - db || a.id.localeCompare(b.id);
    });
  }
  return out;
}

function mdlClientChip(c) {
  const n = (c.models || []).length;
  const st = !c.installed ? "未装" : c.running ? "运行中" : n ? n + " 条" : "未配";
  return (
    '<div class="mcli-c' + (c.installed ? "" : " off") + '" data-act="mdlclistate" data-id="' +
    esc(c.client) + '" title="' + esc(c.label + " · " + (c.files || []).join(" / ")) + '">' +
    "<b>" + esc(c.short) + "</b><span>" + esc(st) + "</span></div>"
  );
}

function mdlRowHtml(r) {
  const done = mdlDoneFor;
  const bits = [];
  if (r.context) bits.push(r.context);
  for (const t of r.tags || []) if (t !== r.context) bits.push(t);
  if (r.vendor) bits.push(r.vendor);
  // 页面里不出现「分组」字样(用户口径):不列、不标、不解释
  const unusable = r.usable === false;
  const configured = mdlConfiguredIds().has(r.id);
  const badges = [];
  if (configured) badges.push('<span class="tagd ok">已配置</span>');
  if (r.tiered) badges.push('<span class="tagd">阶梯</span>');
  if (unusable) badges.push('<span class="tagd bad">不可用</span>');
  if (MDL.src === "opencode-go") {
    const apiLabel = {
      "open-ai-chat-completions": "Chat API",
      "open-ai-responses": "Responses API",
      "anthropic-messages": "Anthropic API",
      "google-generative": "Google API",
    }[r.apiProtocol];
    badges.push(apiLabel
      ? '<span class="tagd" title="官网公布的模型接口协议">' + esc(apiLabel) + "</span>"
      : '<span class="tagd bad" title="官网模型接口列表未标注该模型协议">协议待确认</span>');
  }
  const btns = ["trae", "workbuddy", "workbuddy-ai", "zcode", "dsh"]
    .map((id) => {
      const st = MDL.clients.find((x) => x.client === id);
      const installed = st ? st.installed : false;
      const running = st ? st.running : false;
      const on = done(id, r.id);
      // DSH 是唯一「边跑边写」的:它的 profile 按请求解析,运行中不必先退出,
      // 所以既不给它「运行中」的黄色警示,也不在提示里让人去退它。
      const hot = HOT_CLIENTS.has(id);
      const cls = on ? " on" : running && !hot ? " run" : installed ? "" : " no";
      const tip = installed
        ? (on ? "已配置 · 点开可更新或取消写入" : "写入 " + st.label) +
          (hot
            ? (running ? "(运行中也能写 · 下一个请求生效)" : "(不用退出 · 下一个请求生效)")
            : running ? "(正在运行 · 先退出它再写,否则会被它覆盖)" : "")
        : "没检测到配置文件";
      return (
        '<button class="cdot' + cls + '" data-act="mdlcli" data-id="' + esc(r.id) + '" data-c="' + id +
        '" title="' + esc(tip) + '"' + (installed ? "" : " disabled") + ">" +
        esc(st ? st.short : "?") + "</button>"
      );
    })
    .join("");
  // 不可用的行不给单价:它的价不属于你这把 key;没有价格的来源干脆不显示价格行
  const price = mdlHasPrice() && !unusable ? mdlPriceText(r) : "";
  return (
    '<div class="mrow' + (configured ? " done" : "") + '">' +
    '<div class="mtop"><span class="mname">' + esc(r.id) +
    '<button class="minfo" data-act="mdlinfo" data-id="' + esc(r.id) +
    '" title="基本信息 · 逐项可复制(客户端里手填时用这个)">ⓘ</button>' +
    '</span><span class="cdots">' + btns + "</span></div>" +
    '<div class="mtags">' + esc(bits.join(" · ")) + badges.join("") + "</div>" +
    (price ? '<div class="mprice ' + mdlPriceTone(r) + '">' + esc(price) + "</div>" : "") +
    "</div>"
  );
}

/** 渲染异常不许把界面留在「正在取…」上(真机踩过:后端成功 + 前端抛异常,看着像在加载)。 */
function renderModels() {
  try {
    renderModelsInner();
  } catch (err) {
    const meta = $("mdlMeta");
    if (meta) meta.innerHTML = '<div class="mdlwarn">模型页渲染出错:' + esc(err && err.message) + "</div>";
    console.error("renderModels", err);
  }
}

function renderModelsInner() {
  const meta = $("mdlMeta");
  const list = $("mdlList");
  if (!meta || !list) return;
  // 来源芯片:始终三个,没取到的来源标「—」
  const chips = MDL.sources
    .map((s) => {
      const v = MDL.views[s.id];
      const sub = !v ? "—" : v.usableAt ? v.usableCount + " 个可用" : "可用数未知";
      return (
        '<div class="srcbar-i' + (s.id === MDL.src ? " on" : "") + '" data-act="mdlsrc" data-id="' +
        esc(s.id) + '" title="' + esc(s.name) + '">' + esc(s.name) + "<i>" + esc(sub) + "</i></div>"
      );
    })
    .join("");
  const srcEl = $("mdlSrc");
  if (srcEl) srcEl.innerHTML = chips;
  // 空列表有两种意思:"探测环节坏了"与"这台机器真没装客户端",必须分开说(P2-35)
  $("mdlCli").innerHTML = MDL.clients.length
    ? MDL.clients.map(mdlClientChip).join("")
    : MDL.stateError
      ? '<span class="dim">检测客户端失败:' + esc(MDL.stateError) + "</span>"
      : '<span class="dim">没检测到可写入的客户端(Trae / WorkBuddy / ZCode / DeepSeek Harness)</span>';

  const v = mdlView();
  if (!v) {
    meta.textContent = MDL.loading ? "正在取价目表…" : "还没取到模型清单";
    list.innerHTML = "";
    return;
  }
  const rows = mdlVisibleRows();
  const done = mdlConfiguredIds();
  const isGo = v.source === "opencode-go";
  meta.innerHTML =
    '<div class="mrow1"><span class="dim">' +
    (isGo
      ? "OpenCode Go 按模型选择协议端点(仅兼容的模型可自动写入)"
      : '写进客户端 <b>' + esc(v.baseUrl) + "</b>") +
    "</span>" +
    '<span class="dim">' +
    (v.hasPrice ? "单价按站点自动分组折算" : isGo ? "订阅制:不按 token 计价" : "该来源不提供单价") +
    "</span></div>" +
    '<div class="mrow1"><span class="dim">显示 ' + rows.length +
    (v.usableAt ? " · 可用 " + v.usableCount : " · 可用数未知") +
    " · 已配置 " + done.size + "</span>" +
    '<span class="dim">' +
    (v.fetchedAt ? "清单 " + relTime(v.fetchedAt) + " · " : "") +
    (v.hasPrice ? "价格 ¥高→低" : "按 已配置 → 名称") + "</span></div>" +
    (v.usableAt
      ? ""
      : '<div class="mdlwarn">' +
        (v.hasKey || v.listPublic
          ? "可用集合没取到(刷新试试)"
          : "未配 " + esc(v.sourceName) + " 密钥") +
        ":按「不列不可用模型」的规则,这里只显示你已经配到客户端的那几条</div>") +
    (v.error ? '<div class="mdlwarn">' + esc(v.error) + "</div>" : "");
  list.innerHTML = rows.length
    ? rows.map(mdlRowHtml).join("")
    : '<div class="empty">' +
      ((v.usableAt || 0) === 0
        ? "还没取到" + esc(v.sourceName) + "的模型清单" +
          (v.hasKey || v.listPublic ? "(刷新试试)" : ":先在「渠道」页签填密钥") +
          " —— 按规则这里只显示已配置的那几条"
        : "没有符合条件的模型" +
          " —— 只列这个来源能用的、以及已经配到客户端的") +
      "</div>";
}

/** 弹层内容:写入预告 / 取消写入 / 写入结果 / 客户端详情 —— 四合一。 */
function renderModelSheet() {
  const ov = $("mdlOv");
  const box = $("mdlOvIn");
  if (!ov || !box) return;
  const s = MDL.sheet;
  if (!s) {
    ov.style.display = "none";
    box.innerHTML = "";
    return;
  }
  ov.style.display = "flex";

  if (s.kind === "info") {
    box.innerHTML = infoSheetHtml(s);
    return;
  }

  if (s.kind === "client") {
    const c = MDL.clients.find((x) => x.client === s.client);
    if (!c) {
      MDL.sheet = null;
      renderModelSheet();
      return;
    }
    const recent = MDL.log.filter((r) => r.client === s.client).slice(0, 3);
    const bySource = {};
    for (const m of c.models || []) (bySource[m.source || "其它"] ||= []).push(m.id);
    const chipsHtml = Object.keys(bySource).length
      ? Object.entries(bySource)
          .map(
            ([src, ids]) =>
              '<div class="srow"><span class="sk">' +
              esc(src === "其它" ? "其它" : (MDL.sources.find((x) => x.id === src) || {}).name || src) +
              '</span><span class="sv">' +
              ids
                .map(
                  (m) =>
                    '<button class="btn s" data-act="mdlcli2" data-c="' + esc(c.client) + '" data-src="' +
                    esc(src) + '" data-id="' + esc(m) + '" title="点开看这条的状态 / 取消写入">' + esc(m) + "</button>"
                )
                .join(" ") +
              "</span></div>"
          )
          .join("")
      : '<div class="srow"><span class="sk">已写入</span><span class="sv">还没有 —— 去模型页挑一个</span></div>';
    box.innerHTML =
      '<div class="ov-h"><b>' + esc(c.label) + "</b>" +
      '<span class="dim" style="font-size:11px" data-act="mdlclose">✕</span></div>' +
      '<div class="srow"><span class="sk">状态</span><span class="sv">' +
      (c.installed
        ? c.running
          ? HOT_CLIENTS.has(c.client)
            ? "正在运行(不用退出 · 下一个请求生效)"
            : "正在运行(写入前需先退出)"
          : "未运行"
        : "没检测到配置文件") + "</span></div>" +
      '<div class="srow"><span class="sk">配置</span><span class="sv">' + esc((c.files || []).join("  ·  ")) + "</span></div>" +
      chipsHtml +
      '<div class="srow"><span class="sk">选中</span><span class="sv">' +
      (c.selected ? esc(c.selected) : "读不到(它自己的选中项)") + "</span></div>" +
      (c.note ? '<div class="note">' + esc(c.note) + "</div>" : "") +
      (recent.length
        ? '<div class="note" style="font-weight:600;color:#cdd5e4;margin-top:8px">最近写入</div>' +
          recent
            .map(
              (r) =>
                '<div class="srow"><span class="sk">' + esc(r.stamp.slice(9)) + '</span><span class="sv">' +
                esc(r.model) + (r.added ? " · 新增 " + r.added : "") +
                (r.updated ? " · 更新 " + r.updated : "") +
                (r.removed ? " · 移除 " + r.removed : "") +
                (r.backupOk
                  ? '<button class="btn s" data-act="openpath" data-id="' + esc(r.backupPath) + '">备份</button>' +
                    '<button class="btn s" data-act="mdlrollback" data-id="' + esc(r.backupPath) + '">回滚</button>'
                  : '<span class="dim"> · 备份已清理(只保留最近几份),这条回滚不了</span>') +
                "</span></div>"
            )
            .join("")
        : "") +
      '<div class="acts">' +
      '<button class="btn" data-act="openpath" data-id="' + esc((c.roots || [])[0] || "") + '">打开配置目录</button>' +
      '<button class="btn p" data-act="mdlclose">关闭</button></div>';
    return;
  }

  // 写入预告 / 结果
  const head =
    '<div class="ov-h"><b>' +
    (s.reports ? (s.mode === "remove" ? "取消写入结果" : "写入结果") : "写入 · ") +
    esc(s.model) + "</b>" +
    '<span class="dim" style="font-size:11px">' + esc(CLIENT_NAME[s.client] || s.client) +
    ' <span data-act="mdlclose" style="cursor:pointer">✕</span></span></div>';
  if (s.error) {
    box.innerHTML =
      head + '<div class="mdlwarn">' + esc(s.error) + "</div>" +
      '<div class="acts"><button class="btn" data-act="mdlcopy" data-id="' + esc(s.model) + '">复制三件套</button>' +
      '<button class="btn p" data-act="mdlclose">关闭</button></div>';
    return;
  }
  if (!s.previews) {
    box.innerHTML = head + '<div class="dim">正在生成写入预告…</div>';
    return;
  }
  if (s.reports) {
    box.innerHTML =
      head +
      (s.reports.some((r) => r.error)
        ? '<div class="mdlwarn">有几家没写成,原因见下。已写成的那些照常可以回滚。</div>'
        : "") +
      s.reports
        .map((r) => {
          // 没写成的那一家:给原因,不给"打开备份/回滚"两个必然报错的按钮(报告 P1-6 / P3-6)
          if (r.error) {
            return (
              '<div class="okbox"><b>✕ ' + esc(r.label) + "</b> · 这一家没写成" +
              '<div class="mdlwarn">' + esc(r.error) + "</div>" +
              (r.notes || []).map((n) => '<div class="note">' + esc(n) + "</div>").join("") +
              "</div>"
            );
          }
          const hasBk = !!r.backupPath;
          return (
            '<div class="okbox"><b>✓ ' + esc(r.label) + "</b> · " +
            (r.added ? "新增 " + r.added + " 条" : "") + (r.updated ? "更新 " + r.updated + " 条" : "") +
            (r.removed ? "移除 " + r.removed + " 条" : "") +
            (r.added + r.updated + r.removed === 0 ? "没有改动(本来就没有,或内容一致)" : "") +
            '<div class="srow"><span class="sk">生效</span><span class="sv">' + esc(r.effect) + "</span></div>" +
            (r.selected ? '<div class="srow"><span class="sk">选中</span><span class="sv">已写入选中项</span></div>' : "") +
            (hasBk
              ? '<div class="srow"><span class="sk">备份</span><span class="sv">' + esc(r.backup) +
                '<button class="btn s" data-act="openpath" data-id="' + esc(r.backupPath) + '">打开</button>' +
                '<button class="btn s" data-act="mdlrollback" data-id="' + esc(r.backupPath) + '">回滚这次写入</button>' +
                "</span></div>"
              : '<div class="srow"><span class="sk">备份</span><span class="sv">本次没动文件,无需备份</span></div>') +
            "</div>" +
            (r.notes || []).map((n) => '<div class="note">' + esc(n) + "</div>").join("")
          );
        })
        .join("") +
      '<div class="acts"><button class="btn" data-act="mdlcopy" data-id="' + esc(s.model) + '">复制三件套</button>' +
      '<button class="btn p" data-act="mdlclose">完成</button></div>';
    return;
  }
  const blocked = s.previews.find((p) => p.blocked);
  const blockedIsRuntime = blocked && /正在运行/.test(blocked.blocked);
  const changed = s.previews.some((p) => p.changed);
  const writesSecret = s.previews.some((p) => p.writesSecret);
  const writesSel = s.previews.some((p) => p.writesSelection);
  const removals = s.previews.filter((p) => p.removal);
  box.innerHTML =
    head +
    s.previews
      .map(
        (p) =>
          '<div class="srow"><span class="sk">目标</span><span class="sv">' + esc(p.label) + " · " +
          esc(p.files.join("  ·  ")) + "</span></div>" +
          '<div class="srow"><span class="sk">动作</span><span class="sv">' + esc(p.action) + "</span></div>" +
          '<div class="srow"><span class="sk">选中</span><span class="sv">' +
          (p.writesSelection ? "顺带写入客户端自己的选中项" : "不改选中项") + "</span></div>"
      )
      .join("") +
    (writesSecret
      ? '<div class="mdlwarn">会把这把 ' + esc(s.sourceName || "") +
        ' 密钥<b>明文</b>写进上面那个客户端自己的配置文件' +
        (s.client === "dsh" ? "(DSH 这边是 <code>.credentials.yaml</code> 的 <code>refs:</code>,它自己也是这么存的)" : "") +
        "。挂件自己不存副本:密钥不进数据库、不进日志、不进写入台账。" +
        "<b>但回滚要靠备份</b> —— 备份会拷那个客户端那几个文件,里面的密钥本来是明文," +
        "所以每份快照都用 <b>Windows DPAPI 加密</b>后才写进 <code>backup-inject/&lt;时间戳&gt;/</code>" +
        "(解密钥由当前 Windows 账户托管,不落盘;升级前的旧明文快照会在下次写入时自动加密)。" +
        "<b>代价</b>:换了 Windows 账户或重装系统后,旧快照将无法解密。" +
        "(每个客户端最多 5 份)。快照是密文,手动拷回去无效,请用「一键回滚」;" +
        "想清掉就去模型页的「打开配置目录」。"+
        "</div>"
      : "") +
    (!writesSel && !blocked && !removals.length && s.client !== "dsh"
      ? '<div class="note">选中项这次不写:写入后在客户端里选一次,挂件会记住口径,以后就能直接帮你选。</div>'
      : "") +
    (removals.length
      ? '<div class="note" style="font-weight:600;color:#cdd5e4;margin-top:8px">当前状态:已配置</div>' +
        removals
          .map(
            (p) =>
              '<div class="srow"><span class="sk">会移除</span><span class="sv">' + esc(p.label) + " · " +
              esc(p.removal.what) + "</span></div>" +
              (p.removal.clearsSelection
                ? '<div class="note">它的「选中项」正指向这条模型,会一并清掉(回到"没选"的状态)。</div>'
                : "") +
              '<div class="note">其它条目与你的手改都不动;移除前照样先备份。</div>'
          )
          .join("")
      : "") +
    (blocked
      ? '<div class="mdlwarn">' + esc(blocked.blocked) +
        '<div class="note">' +
        (blockedIsRuntime
          ? "退出它之后直接再点一次下面的按钮就行 —— 按钮会自己重查一遍,这个窗口不用关。" +
            (s.recheckAt ? "(" + esc(s.recheckAt) + " 刚查过,它还在运行)" : "")
          : "本次不会修改客户端配置。") +
        "</div></div>"
      : "") +
    '<div class="acts">' +
    (blocked
      ? blockedIsRuntime
        // 别在这里禁掉按钮:用户常常是**弹层开着的时候**才去退客户端,禁掉的按钮
        // 点了毫无反应,看着就像"写入没成功"(实机反馈)。所以按钮留着,点它 = 重查。
        ? '<button class="btn p"' + (s.busy ? " disabled" : ' data-act="injectgo"') + ">" +
          esc(s.busy ? "正在重查…" : "写入(" + blocked.label + " 在运行 · 点这里重查)") + "</button>"
        : '<button class="btn p" disabled>写入(当前不可写)</button>'
      : changed
      ? '<button class="btn p" data-act="injectgo"' + (s.busy ? " disabled" : "") + ">" +
        (s.busy ? "写入中…" : "写入") + "</button>"
      : '<button class="btn" disabled>内容一致 · 无需写入</button>') +
    (removals.length
      ? '<button class="btn danger" data-act="injectrm"' + (s.busy ? " disabled" : "") + ">" +
        "取消写入(移除这条)</button>"
      : "") +
    '<button class="btn" data-act="mdlcopy" data-id="' + esc(s.model) + '">复制三件套</button>' +
    '<button class="btn" data-act="mdlclose">取消</button></div>';
}

async function loadModels(force, source) {
  const src = source || MDL.src || "4sapi";
  MDL.loading = true;
  if (!MDL.views[src]) renderModels();
  try {
    const view = await invoke("get_model_catalog", { source: src, force: !!force });
    MDL.views[src] = view;
    if (view.sources && view.sources.length) MDL.sources = view.sources;
    if (!MDL.src) MDL.src = view.source || src;
  } catch (e) {
    showToast("取模型清单失败:" + e, true);
  } finally {
    MDL.loading = false;
    renderModels();
  }
}
async function loadInjects() {
  try {
    const ov = await invoke("inject_state");
    MDL.clients = ov.clients || [];
    MDL.log = ov.log || [];
  } catch (e) {
    MDL.clients = [];
    // 后端现在会把"探测环节坏了"与"你没装客户端"分开说,这里别再把它吞成空列表
    MDL.stateError = String(e);
  }
  if (MDL.clients.length) MDL.stateError = "";
  renderModels();
  renderModelSheet();
}
/** 模型基本信息里那几行(标签 → 要复制的值)。逐项复制与「复制全部」共用一份口径。 */
function infoFields(s) {
  const i = (s && s.info) || {};
  return {
    模型名: i.id || "",
    显示名: i.displayName || "",
    接口协议: i.apiType || "",
    接口地址: i.baseUrl || "",
    完整端点: i.endpoint || "",
    上下文: i.contextTokens ? String(i.contextTokens) : "",
    图片: i.multimodal ? "支持" : "不支持",
  };
}

/** 模型基本信息弹层:手工在客户端里新建自定义模型时照着填的那几个值。
 * 密钥在这里只给后四位,「显示」才出明文(key_reveal,8 秒自动收回)。 */
function infoSheetHtml(s) {
  const head =
    '<div class="ov-h"><b>' + esc(s.model) + '</b><span class="dim" style="font-size:11px">' +
    esc(s.sourceName || s.source || "") +
    ' 基本信息 <span data-act="mdlclose" style="cursor:pointer">✕</span></span></div>';
  if (s.error) {
    return (
      head + '<div class="mdlwarn">' + esc(s.error) + "</div>" +
      '<div class="acts"><button class="btn p" data-act="mdlclose">关闭</button></div>'
    );
  }
  if (!s.info) return head + '<div class="dim">正在读取…</div>';
  const f = infoFields(s);
  const tips = {
    模型名: "OpenAI 兼容接口的 model 参数;Trae 里也是它",
    显示名: "WorkBuddy / ZCode 里的名字口径(Trae 用上面的模型名)",
    接口协议: "OpenCode Go 模型的请求协议;不同模型可能不同",
    接口地址: "客户端配置中的 base_url",
    完整端点: "模型实际调用的完整 API 端点",
    上下文: "上下文窗口(token);取不到就不显示",
    图片: "能不能收图",
  };
  const rows = Object.keys(f)
    .filter((k) => f[k] !== "" && !(k === "图片" && f[k] === "不支持"))
    .map(
      (k) =>
        '<div class="irow"><span class="ik" title="' + esc(tips[k] || "") + '">' + esc(k) + "</span>" +
        '<span class="iv">' + esc(f[k]) + "</span>" +
        '<button class="btn s" data-act="infocopy" data-id="' + esc(k) + '">复制</button></div>'
    )
    .join("");
  const keyRow = s.info.hasKey
    ? '<div class="krow"><span class="k1">API Key · <span class="khint">' +
      esc(s.info.keyHint || "••••") + "</span></span>" +
      '<button class="btn s" data-act="infokeycopy" title="复制整把密钥(明文不回界面)">复制</button>' +
      '<button class="btn s" data-act="infokeyreveal" title="显示明文,8 秒后自动隐">显示</button></div>' +
      '<div class="reveal-box" data-rev="info"></div>'
    : '<div class="mdlwarn">还没配 ' + esc(s.sourceName || s.source) +
      " 密钥 —— 先在「渠道」页签填一次,这里才有可复制的密钥(地址与模型名照样能复制)。</div>";
  return (
    head +
    '<div class="note">这里显示官网公布的接口协议与端点。自动写入会检查目标客户端是否支持该协议;' +
    "不兼容时会阻止写入。需要手动配置时,可复制对应字段。</div>" +
    rows + keyRow +
    '<div class="acts">' +
    '<button class="btn" data-act="infocopyall">复制全部(不含密钥)</button>' +
    '<button class="btn p" data-act="mdlclose">关闭</button></div>'
  );
}

/** 打开某个模型的「基本信息」(纯读:不写盘、不碰任何客户端)。 */
async function openModelInfo(model, source) {
  const src = source || MDL.src;
  const sheet = { kind: "info", mode: "info", model, source: src };
  MDL.sheet = sheet;
  renderModelSheet();
  let info, failure = "";
  try {
    info = await invoke("model_info", { source: src, model });
  } catch (e) {
    failure = String(e);
  }
  if (MDL.sheet !== sheet) return;
  sheet.info = info;
  sheet.sourceName = (MDL.sources.find((x) => x.id === src) || {}).name || src;
  if (failure) sheet.error = failure;
  renderModelSheet();
}

/** 打开某个模型 × 某个客户端的弹层(纯读,还没有任何写盘动作)。 */
async function openModelSheet(client, model, source) {
  const src = source || MDL.src;
  const sheet = { kind: "write", mode: "write", client, model, source: src };
  MDL.sheet = sheet;
  renderModelSheet();
  // 预告要读客户端配置,可能跑一两秒。这期间用户按 Esc 或点遮罩会把 MDL.sheet 置空,
  // 于是"await 之后再取 MDL.sheet"会撞上 null 直接抛 TypeError(实测:关掉弹层后
  // inject_preview 才回来,报 "Cannot set properties of null (setting 'error')")。
  // 先把这个对象存进局部变量,回来时只对"还是当前弹层"的结果落账。
  let previews, failure = "";
  try {
    previews = await invoke("inject_preview", { client, source: src, model });
  } catch (e) {
    failure = String(e);
  }
  if (MDL.sheet !== sheet) return;
  sheet.previews = previews;
  sheet.sourceName = (MDL.sources.find((s) => s.id === src) || {}).name || "";
  if (failure) sheet.error = failure;
  renderModelSheet();
}
/** 确认写入:预告看过之后才走到这里。 */
/** 点「写入 / 取消写入」前先重查一次"客户端还在不在运行"。
 *
 * 为什么要有这一步:用户常常是**弹层已经开着**的时候才去退客户端。以前这里是个禁用的
 * 按钮 —— 退完客户端回来一看按钮还是灰的、点了毫无反应,看着就像"写入没成功"
 * (实机反馈)。现在按钮留着可点,点它 = 重新生成一次预告:还挡着就把原因跟时间点
 * 就地写出来,放开了就继续做原先那件事。
 * 返回 true = 可以继续;false = 还被挡着(界面已更新,调用方直接收手)。 */
async function mdlRecheck(s) {
  if (!(s.previews || []).some((p) => p.blocked)) return true;
  s.busy = true;
  renderModelSheet();
  let fresh;
  try {
    fresh = await invoke("inject_preview", { client: s.client, source: s.source, model: s.model });
  } catch (e) {
    if (MDL.sheet === s) {
      s.busy = false;
      s.error = String(e);
      renderModelSheet();
    }
    return false;
  }
  if (MDL.sheet !== s) return false;
  s.busy = false;
  s.previews = fresh;
  const still = (fresh || []).find((p) => p.blocked);
  s.recheckAt = new Date().toTimeString().slice(0, 8);
  renderModelSheet();
  if (still) {
    showToast(s.recheckAt + " 查过:" + (still.label || "") + " 还在运行,先退出它再点一次", true);
    return false;
  }
  return true;
}

async function injectGo() {
  const s = MDL.sheet;
  if (!s || !s.model || s.busy) return;
  if (!(await mdlRecheck(s))) return;
  s.busy = true;
  renderModelSheet();
  try {
    s.reports = await invoke("inject_apply", { client: s.client, source: s.source, model: s.model });
    const ok = (s.reports || []).filter((r) => !r.error);
    // 只有一家、且写成了的时候才把"怎么生效"顺带说一句 —— 多家时详情在结果里
    showToast(
      "已写入 " + (CLIENT_NAME[s.client] || s.client) +
        (s.reports.length === 1 && ok.length === 1 ? " · " + ok[0].effect : "")
    );
  } catch (e) {
    s.error = String(e);
  }
  s.busy = false;
  renderModelSheet();
  loadInjects();
}
/** 取消写入:把这条模型从客户端里删掉。 */
async function injectRemove() {
  const s = MDL.sheet;
  if (!s || !s.model || s.busy) return;
  if (!(await mdlRecheck(s))) return;
  s.mode = "remove";
  s.busy = true;
  renderModelSheet();
  try {
    s.reports = await invoke("inject_remove", { client: s.client, source: s.source, model: s.model });
    const n = s.reports.reduce((a, r) => a + (r.removed || 0), 0);
    showToast(n ? "已从 " + (CLIENT_NAME[s.client] || s.client) + " 移除 " + n + " 条" : "本来就没有这条");
  } catch (e) {
    s.error = String(e);
  }
  s.busy = false;
  renderModelSheet();
  loadInjects();
}
/** 切来源(不重新联网,除非那一份还没取过)。 */
async function pickSource(id) {
  if (!id || id === MDL.src) return;
  MDL.src = id;
  MDL.sheet = null;
  renderModelSheet();
  renderModels();
  if (!MDL.views[id]) await loadModels(false, id);
}
/** 打开模型页签;带来源时直接切过去(渠道行的「去挑模型」用的就是这个)。 */
async function ensureModels(source) {
  if (!MDL.clients.length) await loadInjects();
  const want = source || MDL.src || "4sapi";
  if (!MDL.src) MDL.src = want;
  if (!MDL.views[want]) await loadModels(false, want);
  renderModels();
}

