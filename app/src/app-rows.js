// ───────────── 渲染 ─────────────
/** 今日积分到账一行(只有记到账的渠道有 `grantToday`)。
 *
 *  口径见 docs/design/design-workbuddy-ai-channel-2026-10-23.html §4.1:
 *    · 有活跃奖励 → "今日活跃 +30 · 08:12"(不是自己领的就不写"已领取")
 *    · 只有充值   → "今日新增 +500 · 非活跃"(充值到账 ≠ 活跃获取)
 *    · 什么都没到 → "今日未获取积分"(确定的结论,不是"读不到")
 *  渠道摘要只显示活动与数额，来源由到账记录明细承载。
 */
function grantLine(c) {
  const g = c.grantToday;
  if (!g) return "";
  if (g.hasActivity) {
    const t = g.lastAt ? " · " + fmtClock(g.lastAt) : "";
    return `今日活跃 +${points(g.activityTotal)}${t}`;
  }
  if (g.total > 0) return `今日新增 +${points(g.total)} · 非活跃`;
  return "今日未获取积分";
}

/** 30 天记录明细。懒加载:点「记录」才查库,不在每轮轮询里读。
 *  倒序 —— 最近的在上;来源如实标(现在全是"推算",接上平台接口后会变"平台确认")。 */
function grantListHtml(id) {
  const rows = GRANTS[id];
  if (!rows) return '<div class="glist"><div class="grow"><span>读取中…</span></div></div>';
  if (!rows.length)
    return '<div class="glist"><div class="grow"><span>还没有记录 —— 第一次到账会在下一轮取数后出现</span></div></div>';
  return `<div class="glist">${rows
    .slice()
    // 最近的在上。后端给的是升序(按天分组要顺着看),这里再排一次 ——
    // 别把"显示顺序"押在调用方的返回顺序上。
    .sort((a, b) => b.ts - a.ts)
    .map(
      (r) =>
        `<div class="grow"><span>${esc(r.day)} ${fmtClock(r.ts)}</span><b>+${points(
          r.amount
        )}</b><i>${r.source === "api" ? "平台确认" : "推算"}</i></div>`
    )
    .join("")}</div>`;
}

/** 状态轨是单行省略:把完整文案挂到 title / aria-label 上,键盘与读屏都能读到全文,
 *  又不会为了换行把行高改来改去。 */
function plainText(html) {
  const d = document.createElement("div");
  d.innerHTML = String(html || "");
  return (d.textContent || "").replace(/\s+/g, " ").trim();
}

function renderRow(c, i) {
  const gemini = isAntigravity(c) && window.PanelLayoutUI && panelLayout() === "thumbnail" && currentForm() === "panel" ? window.PanelLayoutUI.gemini(c) : null;
  if (gemini) c = gemini.channel;
  const tone = gemini ? gemini.tone : hvTone(c);
  const noKey = !c.hasKey;
  const failed = c.hasKey && !c.valid && !c.stale;
  const r = remainRatio(c);
  const pct = r === null ? null : Math.round(r * 100);
  const isPct = c.kind === "percent";
  const isPts = c.kind === "points";
  // 配额型(OpenCode Go):大数字固定取「周期」窗的剩余(实测锚定开通日,旧称「本月」),
  // 下面三行各带一条迷你条把 5 小时 / 本周 / 周期都摊开,右侧附重置倒计时
  const wins = isPct && c.windows && c.windows.length ? c.windows : [];
  const mw = wins.length ? mainWindow(c) : null;
  const quotaMain = (c.id === "codex" || isAntigravity(c)) && !!mw;
  const codex = c.id === "codex" && !!mw;
  const ag = isAntigravity(c) && window.AntigravityUI;
  const fiveHour = codex ? wins.find((w) => w.label === "5 小时") : null;
  const codexDual = codex && mw.label === "本周" && !!fiveHour && !noKey && c.remaining != null;

  let main;
  if (noKey) main = "——";
  else if (c.remaining === null) main = "——";
  else if (isPct) {
    // 主数字取绑定窗的余量;「用掉还是剩余」只在这里翻一次,四处同源
    const v = dispPct(Math.max(0, Math.min(100, mw ? mw.remainPercent : c.remaining)));
    main = v.toFixed(1) + '<i class="pct">%</i>';
  } else if (isPts) {
    // 积分不带货币符号;整数部分千分位,小数部分照旧。
    // 单位按渠道的 unit 走:credits = 积分/分(历史口径),豆包这类其它单位照它自己的叫法显示
    const [ip, dp] = points(c.remaining).split(".");
    const ptsUnit = c.unit && c.unit !== "credits" ? c.unit : "分";
    main = ip + (dp ? '<i class="dec">.' + dp + "</i>" : "") + '<i class="unit">' + esc(ptsUnit) + "</i>";
  } else {
    // 金额型:统一走 money(千分位 + 币种,非 CNY 渠道照它自己的币种标,不换算);
    // 小数部分单独包一层 <i class="dec"> 缩一号
    const prefix = curSign(c.unit);
    const [int, dec] = money(c.remaining, c.unit).slice(prefix.length).split(".");
    main = esc(prefix) + int + (dec ? '<i class="dec">.' + dec + "</i>" : "");
  }

  // 申请制额度(4SAPI):分母是本轮额度,副标题给周期状态
  const claim = c.claim || null;

  // 第二行给状态语义,大数字下方给数值口径,两处不重复。
  // 2026-10-23(设计 docs/design/design-row-detail-2026-10-23.html §5):失败 / 需登录 / 陈旧
  // 这三种"旧快照"行,同一句话只说一次 —— 状态标签说"坏了",副标题只说原因,
  // 右侧只说"多久以前的快照"。以前这三样各写两遍,折叠行 94px、五行字。
  const hv = gemini ? gemini.health : hvRow(c.id);
  const isStale = !!c.stale || !!(hv && hv.state === "stale");
  const isOld = isStale || failed;
  const hasTag = !!(hv && hv.qualifier && hv.qualifier !== "~") || !!(CFG && CFG.textTone);

  let sub;
  // web = 挂件内网页登录型(WorkBuddy):没登录时说"未登录 X",不是"未检测到登录"
  // —— 后者会让人以为打开客户端就能修好,而桌面端 5.6.2 起把令牌加密存了(读不到)。
  if (noKey)
    sub =
      c.authSource === "app"
        ? `未检测到 ${esc(c.name)} 登录信息`
        : c.authSource === "web"
        ? `未登录 ${esc(c.name)}`
        : "未配置密钥";
  else if ((hv && hv.state === "auth") || (c.recovery && c.recovery.phase === "auth")) sub = esc(c.error || "登录凭据已失效");
  else if (failed && c.recovery && c.recovery.phase === "retrying") sub = "正在获取 · 自动重试中";
  else if (failed)
    sub = hasTag
      ? c.error
        ? esc(c.error)
        : ""
      : `取数失败${c.error ? " · " + esc(c.error) : ""}`;
  else if (c.stale) sub = esc(hv && hv.secondary ? hv.secondary : (c.recovery && c.recovery.phase === "retrying" ? "更新中 · 保留上次额度" : "暂时无法更新 · 保留上次额度"));
  else if (wins.length) {
    // Codex 的套餐档位(free/plus/pro)画在副标题里。OpenCode Go 的 extra 里也可能有
    // 一条「套餐」($10/期 · 未折进消耗),那不是 ChatGPT 订阅 —— 只认 Codex(§6.2)
    const plan = c.id === "codex" ? (c.extra || []).find((e) => e[0] === "套餐") : null;
    sub = plan ? `<span class="plan" title="ChatGPT 订阅">${esc(plan[1])}</span>` : "";
    if (c.id === "codex" && c.resetCredits) {
      const n = c.resetCredits.availableCount;
      sub += `${sub ? " · " : ""}重置卡 ${n == null ? "未知" : esc(n) + " 张"}`;
    }
  }
  else if (c.limited) sub = "已限流";
  else if (isPts) {
    // 记到账的渠道(WorkBuddy AI):这一行给"今天有没有活跃获取" —— 那是它最该被看见的
    // 信息,比"近期会过期多少"更天天变。到期信息仍在右侧 `.p` 与展开区里。
    const gl = grantLine(c);
    const soon7 = expiringWithin(c, SUMMARY_EXPIRY_DAYS);
    sub = gl
      ? esc(gl)
      : soon7 > 0
      ? `近 ${SUMMARY_EXPIRY_DAYS} 天将过期 ${points(soon7)} 分`
      : !c.expiryComplete
        ? c.expiring && c.expiring.length
          ? "到期信息不完整"
          : "未提供到期明细"
        : c.expiring && c.expiring.length
          ? `近 ${SUMMARY_EXPIRY_DAYS} 天无到期`
          : "没有待到期积分";
  } else if (claim) sub = `本轮额度 ${esc(money(claim.amount))}${claimTail(claim)}`;
  // 以前没开申请制时这里显示「额度 ¥{c.total}」:c.total 是**累计发放**(只增不减),
  // 不是额度上限 → "剩 21%" 会随每次充值一路下滑,与还能用多少钱毫无关系(报告 P1-11)。
  // 峰谷渠道(DeepSeek):第二行如实说"现在贵不贵",只报状态不报单价(规则会变)
  else if (c.peak) sub = esc(peakParts(c).label);
  else sub = ""; // 金额型拿到多少就是可用多少,不加说明

  if (ag && c.id !== "antigravity") sub = ag.accountStatus(c);
  let subVal;
  if (noKey)
    subVal = c.authSource === "app" ? "需打开应用" : c.authSource === "web" ? "待登录" : "待配置";
  else if (failed) subVal = c.updatedAt ? snapAgeSpan(c.updatedAt) : "未连上";
  // 陈旧(取数成功但过期):年龄比"剩多少 / 哪个窗"更重要 —— 数字再准也是旧的
  else if (isStale) subVal = c.updatedAt ? snapAgeSpan(c.updatedAt) : "上次快照";
  else if (mw) {
    // 重置时间未知时不再写一个悬空的「—」:写「周期窗 · —」比只写「周期窗」更难读
    const ms = resetMs(mw);
    subVal = `${esc(mw.label)}窗${ms !== null ? " · " + fmtReset(ms, true) : ""}`;
  }
  else if (isPts) {
    // 最近到期由卡片底部的状态轨承担(日期 + 数量 + 相对时间);
    // 右侧限定语不再重复一次,把空间留给大数字。
    subVal = "";
  }
  // 峰谷倒计时由 ticker 原地填充，不把墙钟文字写进行签名
  else if (c.peak)
    subVal = `<span class="tdyn" data-tdyn="peak" data-peakcd='${JSON.stringify(c.peak)}'></span>`;
  else if (pct !== null) subVal = `剩 ${pct}%`;
  else subVal = "";

  if (codexDual) {
    const shortValue = dispPct(Math.max(0, Math.min(100, fiveHour.remainPercent)));
    subVal = `<span class="quota-secondary"><span>5 小时${CFG.showUsed ? "已用" : "剩余"}</span><b style="color:${toneFgSoft(quotaTone(c, fiveHour))}">${shortValue.toFixed(1)}%</b></span>`;
    // 两项额度占右侧两行,旧数据的年龄移到左侧,仍然显式可见。
    if (isOld) sub = `${c.updatedAt ? snapAgeSpan(c.updatedAt) : "上次快照"}${sub ? " · " + sub : ""}`;
  }
  if (ag && mw && !noKey && !isOld) subVal = esc(mw.label) + (CFG.showUsed ? "已用" : "剩余");
  // 失联 / 旧快照:数值降为 text-3 并加 ~,不让"上次成功值"看起来像当前值。
  // 紧凑条里由 CSS 的 ::before 画 ~(省一个节点),面板里用真实节点。
  const compactNow = document.body.className.includes("form-compact");
  // Codex 的窗口名与重置时间由状态轨承担(「本周窗 · N 天后重置」);
  // 主数字只留百分比,避免「本周剩余」与「本周窗」在同一张卡上重复。
  const mainValue = isOld && !compactNow ? `<i class="tilde">~</i>${main}` : main;
  const mainTone = isOld ? "off" : quotaMain ? quotaTone(c, mw) : tone;

  // 金额型第二行两边都空时给今日消耗兜底:否则整行会比别的行矮一截(Hapi 39px vs 51px)
  if (!noKey && !failed && c.kind === "amount" && !sub && !subVal && c.day !== null) {
    sub = `今日 ${money(c.day, c.unit)}`;
    subVal = "&nbsp;"; // 右侧留一行空位,和别的行一样高(不留就矮 2px)
  }

  // 组合预警(裁决②):周期余量低于标红线 **且** 距重置 >3 天才提示 ——
  // 重置就在眼前的低余量不值得喊,避免天天狼来了
  let winWarn = "";
  if (isPct && mw && !failed) {
    const ms = resetMs(mw);
    const rem = Math.max(0, Math.min(100, mw.remainPercent));
    if (ms !== null && rem < CFG.critPercent && ms - Date.now() > 3 * 86400e3) {
      winWarn = `<div class="win-warn">周期余量仅 ${rem.toFixed(1)}%,距重置还有 ${Math.round(
        (ms - Date.now()) / 86400e3
      )} 天 —— 省着用或等重置</div>`;
    }
  }

  // 状态点(U2 分层):未配置=空心环,失联=斜杠灰点 —— 两种灰色不再共用
  const dcls = gemini ? gemini.dot : dotCls(c);
  const dot = `<span class="dot${dcls ? " " + dcls : ""}"></span>`;

  // 配额型的三条迷你条已经表达了余量,积分型的到期列表也没有"余量比例"可画
  const bar =
    pct !== null && !failed && !noKey && !wins.length && !isPts
      ? `<div class="bar"><i style="width:${pct}%;background:${TONE_HEX[tone]}"></i></div>`
      : "";

  // 撑不到下次可申请:按当前速度余额不够撑到冷静期结束,行内直接提示
  const claimWarn =
    claim && claim.shortageRisk && claim.daysOfBalance !== null && claim.daysOfBalance !== undefined
      ? `<div class="claim-warn">按当前速度余额约可用 ${Math.max(1, Math.round(
          claim.daysOfBalance
        ))} 天,可能撑不到下次可申请</div>`
      : "";

  // 紧凑条放不下整句警示(第二行会把 46px 的单行条撑爆)——折叠形态只在名称行
  // 挂一枚 ⚠:黄=撑不到下次申请,红=周期余量告急;整句仍只在面板形态显示(CSS 控制)
  const wmark = winWarn
    ? '<span class="wmark bad" title="周期余量告急">⚠</span>'
    : claimWarn
      ? '<span class="wmark warn" title="余额可能撑不到下次可申请">⚠</span>'
      : "";

  // 签到徽标:只在该渠道开了签到时出现,折叠成紧凑条也保留(与 .wmark 同一套机制)
  const ckb = ckBadgeHtml(c);

  // 「更新于」不占行头:CSS 会把陈旧行的数字降透明度,旧快照的年龄由右侧 `.p` 写一次
  // (2026-10-23 起;以前这里还有一条黄色 `.rhint`,与右侧「上次快照 / 12 分钟前快照」重复)

  // 两级行:需要动作的(T0)与已跌破红线的行给两行高与一条进度条,其余只留一行。
  // 状态标签是"颜色之外"的第二个通道(P4 · 颜色不单独承重)。
  const urgent = !!(hv && (hv.tier === 0 || hv.tone === "bad"));
  // 状态标签:颜色之外的第二个通道(P4)。默认只给"需要动作"的挂一枚;
  // 开了「非颜色模式」后每一行都写文字态 —— 灰度截图下也读得出来(D12)。
  const TONE_WORD = { ok: "正常", warn: "预警", bad: "告急", off: "无数据" };
  const qualifier = ag && !isOld && !noKey ? "" : hv && hv.qualifier;
  const qtag =
    qualifier && qualifier !== "~"
      ? `<span class="qtag">${esc(qualifier)}</span>`
      : CFG && CFG.textTone
        ? `<span class="qtag">${TONE_WORD[tone] || ""}</span>`
        : "";

  // 卡片状态轨:第三行说清"接下来会发生什么"(到期 / 重置 / 快照),
  // 不重复大数字。积分型的最近一笔到期在这里常驻 —— 不用先触发告警。
  let railLeft = sub || "";
  let railRight = subVal || "";
  let railWarn = false;
  if (c.id === "google-cloud" && c.hasKey && window.GoogleCloudUI) {
    const rail = window.GoogleCloudUI.rail(c);
    railLeft = rail.left;
    railRight = rail.right;
    railWarn = rail.warn;
  } else if (ag && mw && c.id !== "antigravity") {
    const rail = ag.rail(c);
    railLeft = rail.left;
    railRight = c.stale ? window.PanelLayoutUI.weeklyReset(c, true) : rail.right;
  } else if (noKey || failed || isStale) {
    // 原因与快照年龄已经分别写在 .s / .p 里,状态轨不再重复(HUD 同一句话只说一次)
    railLeft = "";
    railRight = "";
  } else if (ag && mw) {
    const rail = ag.rail(c);
    railLeft = rail.left;
    railRight = rail.right;
  } else if (isPts) {
    const next = window.ExpiryUI.nextEvent(c);
    if (next && next.type === "points") {
      const e = next.entry;
      railLeft = `最近 ${window.PanelLayoutUI.expiryDateHtml(e.at)} ${fmtClock(e.at)} 到期 · ${points(e.amount)} 分`;
      railRight = expiryRelativeSpan(e.at);
      railWarn = e.at * 1000 - Date.now() <= 7 * 86400e3;
    } else if (next && next.type === "account") {
      railLeft = `账户额度 ${window.PanelLayoutUI.expiryDateHtml(next.at)} ${fmtClock(next.at)} 到期`;
      railRight = expiryRelativeSpan(next.at);
      railWarn = next.at * 1000 - Date.now() <= 7 * 86400e3;
    } else {
      const acc = accountExpiryOf(c);
      if (acc && acc.state === "dated" && acc.at) {
        railLeft = `账户额度 ${expiryParts(acc.at).date} ${expiryParts(acc.at).time} 到期`;
        railRight = expiryRelativeSpan(acc.at);
      } else if (acc && acc.state === "never") {
        railLeft = "账户额度不过期";
        railRight = "逐笔到期未知";
      } else if (acc && acc.state === "dateOnly") {
        railLeft = `账户额度 ${acc.at} 到期`;
        railRight = "逐笔到期未知";
      } else if (Number(c.remaining || 0) <= 0) {
        railLeft = "当前没有剩余积分";
        railRight = "";
      } else if (!c.expiryComplete) {
        railLeft = "逐笔到期时间暂未提供";
        railRight = "到期未知";
      } else {
        railLeft = "当前剩余积分没有已知到期日";
        railRight = "";
      }
    }
  } else if (mw) {
    // 后端视图的 secondary 信息量更大(绑定窗 + 恢复时间),优先用它;
    // 拿不到时给窗口重置时间,不重复右侧 `.p` 的窗口限定语。
    const sec = hv && hv.secondary && !claimWarn && !winWarn ? esc(hv.secondary) : "";
    if (sec) railLeft = sec;
    else {
      const ms = resetMs(mw);
      railLeft = ms !== null
        ? `${mw.label}窗 · ${fmtReset(ms, true)}后重置`
        : "配额窗口 · 按重置恢复,不折算金额";
    }
    railRight = "";
  } else if (c.kind === "amount") {
    // 副标题已经说了申请制 / 峰谷;状态轨给"今天花了多少"和剩余比例,
    // 两条信息不重复。没有今日数据时如实写"暂无",不补 0。
    railLeft = c.day !== null
      ? `今日 ${money(c.day, c.unit)}`
      : c.peak
        ? esc(peakParts(c).label)
        : "今日消耗暂无";
    railRight = pct !== null ? `剩 ${pct}%` : hv && hv.qualifier ? esc(hv.qualifier) : "";
  }
  if ((c.id === "codex" || isAntigravity(c)) && !noKey && !failed && !isStale && !(hv && hv.state === "auth")) {
    railRight = [railRight, window.PanelLayoutUI.weeklyReset(c, true)].filter(Boolean).join(" · ");
  }
  const railIcon = isPts || mw ? "#i-clock" : failed || isStale ? "#i-info" : "#i-info";
  const thumbnail = window.PanelLayoutUI && panelLayout() === "thumbnail" && currentForm() === "panel" ? window.PanelLayoutUI.summary(c, { noKey, failed, isOld, mw, sub, subVal, railLeft, railRight, winWarn, claimWarn }) : {};

  // data-state:后端判的七态,给 CSS 用 —— 紧凑条与胶囊里没有 .rhint 那行字,
  // 只有靠它才能把"这是旧快照"画出来(设计 P0 验收①)
  return `<div class="row${urgent ? " urg" : ""}${codex ? " codex-quota" : ""}${codexDual ? " codex-dual" : ""}" data-id="${c.id}" data-kind="${c.kind}" data-railwarn="${railWarn ? "1" : "0"}" data-dtone="${dcls}" data-state="${(hv || {}).state || ""}">
    <div class="rhead" tabindex="0" role="button" aria-haspopup="dialog" aria-label="查看 ${esc(c.name)} 详情">
      <div class="r1">
        ${iconHtml(c, "ico", noKey || failed)}
        <div class="nm">
          <div class="n"><span class="nn">${esc(c.name)}</span><span class="nsh">${esc(c.short)}</span>${thumbnail.name || ""}${dot}${wmark}${ckb}${qtag}</div>
          <div class="s">${sub}</div>
        </div>
        <div class="val">
          ${thumbnail.label || ""}<div class="v" style="color:${toneFg(mainTone)}">${mainValue}</div>
          <div class="p">${subVal}</div>
        </div>
        <div class="caret">&#9654;</div>
      </div>
      <div class="r2"${railLeft || railRight ? ' role="note"' : ""}>${
        railLeft || railRight
          ? `<svg aria-hidden="true"><use href="${railIcon}"/></svg><span class="rt">${railLeft}</span>${railRight ? `<span class="end">${railRight}</span>` : ""}`
          : // 状态轨空态常驻一行不可见占位:既避开 .r2:empty{display:none},又有相同行高
            `<span class="rt rail-hold" aria-hidden="true">&nbsp;</span>`
      }</div>
      ${bar}
      ${claimWarn}
      ${winWarn}
      ${thumbnail.html || ""}
      <span class="sweep"></span>
    </div>
    <div class="rbody" data-body="${c.id}" hidden></div>
  </div>`;
}

// ───────────── 行展开详情:五段骨架 ─────────────
// 2026-10-23(docs/design/design-row-detail-2026-10-23.html §4.2):核心 → 明细 → 最近变动 → 签到 → 页脚,
// 固定顺序,没内容的段不画。同一份列表只画一次:折叠行给结论,展开区给完整列表与动作。

/** ① 核心 · 金额:今日 / 本周 / 本月。 */
function coreAmount(c) {
  const f = (v) => money(v, c.unit);
  return `<div class="sec"><div class="core-use">
    <div>今日<b>${f(c.day)}</b></div>
    <div>本周<b>${f(c.week)}</b></div>
    <div>本月<b>${f(c.month)}</b></div>
    ${
      c.estimated
        ? '<span class="est" title="消耗由本机余额快照变化估算;挂件未运行时段不计入">推算</span>'
        : ""
    }
  </div></div>`;
}

/** ① 核心 · 配额:一窗一行。重置 < 24 小时写相对时间,更久写「N 天 · MM-DD HH:MM」。 */
function coreWindows(c) {
  if (isAntigravity(c) && window.AntigravityUI) return window.AntigravityUI.windows(c);
  const codex = c.id === "codex";
  const exactReset = codex || isAntigravity(c);
  const rows = (c.windows || []).map((w) => {
    const wr = Math.max(0, Math.min(100, w.remainPercent));
    const lim = w.status === "rate-limited";
    const wt = lim ? "bad" : wr < CFG.critPercent ? "bad" : wr < CFG.warnPercent ? "warn" : "ok";
    const ms = resetMs(w);
    let when = "";
    let whenOk = false;
    if (ms !== null) {
      const left = ms - Date.now();
      if (left <= 0) when = "正在重置";
      else if (left < 86400e3) when = fmtReset(ms, true) + "后重置";
      else {
        // 天数走同一个 fmtReset,别再自己 floor 一次 —— 行头写「19 天」、核心段写「18 天」
        // 是同一天的两个说法,谁看都会以为出错(2026-10-23 真机复核抓到)
        const d = new Date(ms);
        when = `${fmtReset(ms, true)} · ${d.getMonth() + 1}-${String(d.getDate()).padStart(
          2,
          "0"
        )} ${fmtClock(Math.floor(ms / 1000))}`;
      }
      // 限流中且 1 小时内重置:写「马上恢复」并染绿 —— 中性倒计时里唯一的好消息
      if (lim && left > 0 && left < 3600e3) {
        when = "马上恢复";
        whenOk = true;
      }
    }
    if (exactReset) {
      when = ms === null ? "重置时间未知" : `下次重置 ${exactLocalTime(ms / 1000)} · <span data-reset-at="${ms}">${esc(quotaResetCountdown(ms))}</span>`;
      whenOk = false;
    }
    return `<div class="core-w${exactReset ? " codex-w" : ""}">
      <span class="wl">${esc(w.label)}</span>
      <i class="wb"><i style="width:${wr}%;background:${TONE_HEX[wt]}"></i></i>
      <b class="wv" style="color:${lim ? "var(--bad)" : toneFg(wt)}">${dispPct(wr).toFixed(1)}%</b>
      <span class="wt${whenOk ? " ok" : ""}">${when}</span>
    </div>`;
  });
  if (!rows.length) {
    const v =
      c.remaining === null
        ? "——"
        : Math.max(0, Math.min(100, Number(c.remaining))).toFixed(1) + "%";
    return `<div class="sec"><div class="core-use"><div>当前剩余<b>${v}</b></div></div></div>`;
  }
  return `<div class="sec">${exactReset ? `<div class="sec-h"><span>配额窗口</span><span>${esc(localZoneLabel())}</span></div>` : ""}${rows.join("")}${
    // 口径说明只放一次:周窗到点清零,5 小时窗到点恢复 —— 两件事措辞不能混
    codex ? `<div class="core-note">周窗到期即清零,不滚存;5 小时窗按滚动恢复</div>` : ""
  }</div>`;
}

function coreCodexResets(c) {
  if (c.id !== "codex") return "";
  // 重置顾问拆在 codex-reset.js 里；加载失败时保留旧的只读卡列表，别让整段消失。
  if (window.CodexResetUI) return window.CodexResetUI.render(c);
  const r = c.resetCredits;
  const n = r && r.availableCount;
  const count = n == null ? "未获取" : `${esc(n)} 张可用`;
  const cards = (r && r.credits) || [];
  const more = !!(rowMore[c.id] || {}).resets;
  const shown = more ? cards : cards.slice(0, 3);
  const rows = shown.map((card) => {
    const at = card.expiresAt;
    const expires = at == null ? "接口未提供到期日" : `${exactLocalTime(at)} 到期${at * 1000 <= Date.now() ? " · 已到期,待刷新" : ""}`;
    return `<div class="reset-credit"><span>${esc(card.title)}</span><b>${esc(expires)}</b></div>`;
  }).join("");
  const notes = [];
  if (!r || r.error) notes.push(r && r.error ? r.error : "重置卡信息未获取,刷新后重试");
  else if (n > 0 && r.credits == null) notes.push("接口仅返回数量,到期明细未提供");
  else if (n > cards.length) notes.push(`接口返回 ${cards.length} 张到期明细,可用数量以 ${n} 张为准`);
  if (r && r.lastRedeemedAt) notes.push(`最近用卡重置 ${exactLocalTime(r.lastRedeemedAt)}`);
  const updated = r && r.updatedAt ? `<div class="fn-note">查询于 ${esc(exactLocalTime(r.updatedAt))}${c.stale || !c.valid ? " · 上次快照" : ""}</div>` : "";
  const toggle = cards.length > 3 ? `<button class="sec-more" data-act="sec-more" data-id="codex" data-sec="resets" aria-expanded="${more}">${more ? "收起" : `全部 ${cards.length} 张`}</button>` : "";
  return `<div class="sec codex-resets"><div class="sec-h"><span>重置卡</span><b>${count}</b></div>${rows}${toggle}${notes.map((s) => `<div class="fn-note">${esc(s)}</div>`).join("")}${updated}</div>`;
}

/** 账户级有效期:新字段优先;老快照只对 Qoder 回退 `extra` 的日期文案。
 *  回退严格只认「YYYY-MM-DD」/「不过期」,不把任意 extra 解释成日期。 */
function accountExpiryOf(c) {
  if (c && c.accountExpiry && c.accountExpiry.state) return c.accountExpiry;
  if (!c || (c.id !== "qoder" && c.id !== "qoder-cn")) return null;
  const line = (c.extra || []).find(([k]) => k === "账户额度到期");
  if (!line) return null;
  const v = String(line[1] || "").trim();
  if (v === "不过期") return { state: "never", at: null };
  if (/^\d{4}-\d{2}-\d{2}$/.test(v)) return { state: "dateOnly", at: v };
  return { state: "unknown", at: null };
}

/** 账户额度到期卡(Qoder):账户级时间与逐笔积分到期严格分开展示。 */
function accountExpiryHtml(c) {
  const acc = accountExpiryOf(c);
  if (acc?.state === "exhausted") return "";
  if (!acc) {
    return `<div class="expiry-summary">
      <div class="expiry-kicker"><svg aria-hidden="true"><use href="#i-clock"/></svg>账户额度到期</div>
      <div class="expiry-date" style="color:var(--tx3)">暂未提供</div>
      <div class="expiry-meta"><span>尚无法确认账户有效期</span><strong>账户级有效期</strong></div>
    </div>`;
  }
  if (acc.state === "dated" && acc.at) {
    const p = expiryParts(acc.at);
    const urgent = acc.at * 1000 - Date.now() <= 7 * 86400e3;
    return `<div class="expiry-summary${urgent ? " urgent" : ""}">
      <div class="expiry-kicker"><svg aria-hidden="true"><use href="#i-clock"/></svg>账户额度到期</div>
      <div class="expiry-date">${p.date}<small>${p.time}</small></div>
      <div class="expiry-meta"><span>${expiryRelativeSpan(acc.at)}</span><strong>账户级有效期</strong></div>
    </div>`;
  }
  if (acc.state === "never") {
    return `<div class="expiry-summary">
      <div class="expiry-kicker"><svg aria-hidden="true"><use href="#i-clock"/></svg>账户额度到期</div>
      <div class="expiry-date">长期有效</div>
      <div class="expiry-meta"><span>账户额度不过期</span><strong>账户级有效期</strong></div>
    </div>`;
  }
  if (acc.state === "dateOnly") {
    // 旧快照只有日期:只显示这一天,不补 23:59、不画小时倒计时
    return `<div class="expiry-summary">
      <div class="expiry-kicker"><svg aria-hidden="true"><use href="#i-clock"/></svg>账户额度到期</div>
      <div class="expiry-date">${esc(acc.at)}</div>
      <div class="expiry-meta"><span>旧快照只记录了日期</span><strong>账户级有效期</strong></div>
    </div>`;
  }
  return `<div class="expiry-summary">
    <div class="expiry-kicker"><svg aria-hidden="true"><use href="#i-clock"/></svg>账户额度到期</div>
    <div class="expiry-date">暂未提供</div>
    <div class="expiry-meta"><span>尚无法确认账户有效期</span><strong>账户级有效期</strong></div>
  </div>`;
}

/**
 * 详情里的「登录网页版」就地入口(2026-11-08 用户反馈"页面上找不到入口")。
 *
 * 逐笔明细不完整或沿用旧数据时，就地显示原因与登录入口。
 */
function qoderLoginBar(c) {
  if (c.id !== "qoder" && c.id !== "qoder-cn") return "";
  const notice = window.ExpiryUI.qoderNotice(c);
  if (notice.fresh) return "";
  return `<div class="qoder-login-bar">
    <span>${esc(notice.text)}</span>
    <button class="btn s" data-act="qoderweblogin" data-id="${c.id}"${
    QODER_LOGGING ? " disabled" : ""
  }>${QODER_LOGGING ? "登录中…" : notice.button}</button>
    <button class="why" data-why="qoderweb" aria-expanded="false" aria-label="网页会话的说明"></button>
  </div>`;
}

/* ───────── 逐笔到期:平台来源代号 → 中文 ─────────
 * Qoder 网页接口给的是原始代号(RESOURCE_PACKAGE_SOURCE_BONUS),活动发的包给的是
 * campaignKey(act-20261110-699),老快照里还可能是 "Purchased Credits" 这种英文包名。
 * 这里统一译出中文,**不动 expiring[] 数据本身**:core / 同步 / 诊断日志仍留原文,
 * 鼠标悬停每行也能看到原始代号,去官网对账不会丢线索。
 * 放在展示层而不是 core 的另一个原因:**数据库里的旧快照是英文**,改取数层要等
 * 下一次网页会话刷新才生效,改渲染则装上就能读懂。
 * 纪律:只译有依据的枚举与词素;认不出的代号保留在「积分包 · 原文」里,不编业务含义。
 */
// 产品文档 §1 从官方 web bundle 实拆到的 source 枚举(PURCHASED / BONUS / REFUND…)。
// 真机返回值会带 RESOURCE_PACKAGE_SOURCE_ 前缀,查表前先剥掉。
const PACK_SOURCE_ZH = {
  BONUS: "赠送积分包",
  PURCHASED: "购买积分包",
  REFUND: "退款积分包",
  TRIAL: "试用积分包",
};
// 词素兜底:平台以后加新代号时,认得的词照直译,不认得的整条退回原文。
const PACK_WORD_ZH = {
  BONUS: "赠送", PURCHASED: "购买", PURCHASE: "购买", REFUND: "退款", TRIAL: "试用",
  GIFT: "赠送", REWARD: "奖励", COMPENSATION: "补偿", SUBSCRIPTION: "订阅",
  ADDON: "加量", ADD: "加量", CHECKIN: "签到", SIGNIN: "签到", DAILY: "每日",
  FREE: "免费", CAMPAIGN: "活动", ACTIVITY: "活动", INVITE: "邀请", REFERRAL: "邀请",
  MONTHLY: "月度", YEARLY: "年度", ANNUAL: "年度", PRO: "Pro",
};
// 只承载结构、不承载来源语义的词,拼中文时略过。
const PACK_STRUCT_WORDS = new Set([
  "RESOURCE", "PACKAGE", "SOURCE", "TYPE", "CODE", "QUOTA", "CREDIT", "CREDITS",
  "POINT", "POINTS", "VALUE", "AMOUNT", "ID", "KEY",
]);
// 组织版 / 老快照里的英文包名(测试样本见 core::qoderweb)。
const PACK_PHRASE_ZH = {
  "purchased credits": "购买积分包",
  "bonus credits": "赠送积分包",
  bonus: "赠送积分包",
  "trial credits": "试用积分包",
  "free credits": "免费积分包",
  "add-on credits": "加量包",
  "gift credits": "活动赠送积分包",
};

/** 把一条来源代号译成中文。已是中文(如 WorkBuddy 的「每日积分」)原样返回 —— 幂等。 */
function packLabelZh(raw) {
  const s = String(raw || "").trim();
  if (!s) return "积分包";
  if (/[\u3400-\u9fff]/.test(s)) return s;
  const phrase = PACK_PHRASE_ZH[s.toLowerCase()];
  if (phrase) return phrase;
  const upper = s.toUpperCase();
  // campaignKey 自带活动日期,只解释格式,不猜是哪一期活动、也不把它当到期日。
  const act = /^ACT-(\d{4})(\d{2})(\d{2})-\d+$/.exec(upper);
  if (act) return `活动积分包 · ${act[1]}-${act[2]}-${act[3]}`;
  const key = upper.replace(/^RESOURCE_PACKAGE_SOURCE_/, "");
  if (PACK_SOURCE_ZH[key]) return PACK_SOURCE_ZH[key];
  if (/^[A-Z0-9_\- ]+$/.test(upper)) {
    const words = upper.split(/[^A-Z0-9]+/).filter(Boolean);
    const semantic = words.filter((w) => !PACK_STRUCT_WORDS.has(w));
    const zh = semantic.map((w) => PACK_WORD_ZH[w]);
    if (semantic.length && zh.every(Boolean)) return [...new Set(zh)].join("") + "积分包";
  }
  return `积分包 · ${s}`;
}

/** 积分到期:WorkBuddy / Trae / WorkBuddy AI / Qoder 共用的唯一组件。
 *  `expiring` 与 `expiryComplete` 是唯一数据源;不从渠道名或文案猜日期。 */
function pointsExpiryHtml(c, showAll) {
  const nowS = Date.now() / 1000;
  const isQoder = c.id === "qoder" || c.id === "qoder-cn";
  const all = (c.expiring || []).filter(
    (e) => e && Number(e.at) > 0 && Number.isFinite(Number(e.at)) && Number.isFinite(Number(e.amount))
  );
  const upcoming = all.filter((e) => e.at > nowS).sort((a, b) => a.at - b.at);
  const past = all.filter((e) => e.at <= nowS).sort((a, b) => b.at - a.at);
  let html = isQoder ? accountExpiryHtml(c) : "";

  if (!all.length) {
    const unknownItemized = c.expiryComplete !== true;
    if (isQoder) {
      // 账户卡已经说明了账户级有效期;这里只补"逐笔未知"这一行,
      // 不再叠一张「暂未提供」的大卡(两句话说的是同一件事)。
      html += `<div class="expiry-table"><div class="expiry-entry">
        <div><b>逐笔积分到期</b><p>${
          unknownItemized ? "当前未获取逐笔明细" : "当前剩余积分没有已知到期日"
        }</p></div>
        <span class="setting-end">${unknownItemized ? "未知" : "无记录"}</span></div></div>`;
      html += `<p class="expiry-footnote">${
        unknownItemized
          ? "逐笔到期信息尚未获取完整;账户有效期与逐笔积分到期不同,不能据此判断所有积分的到期时间。"
          : "账户有效期与逐笔积分到期不同,不能据此判断所有积分在同一时间到期。"
      }</p>`;
      return html;
    }
    const note = unknownItemized
      ? "接口未提供完整的逐笔到期信息,无法判断积分包何时到期"
      : Number(c.remaining || 0) > 0
        ? "当前剩余积分没有已知到期日"
        : "当前没有剩余积分";
    html += `<div class="expiry-summary">
      <div class="expiry-kicker"><svg aria-hidden="true"><use href="#i-clock"/></svg>${
        unknownItemized ? "逐笔到期时间暂未提供" : "最近一笔到期"
      }</div>
      <div class="expiry-date">暂未提供</div>
      <div class="expiry-meta"><span>${note}</span><strong>${unknownItemized ? "到期信息不完整" : "到期信息完整"}</strong></div>
    </div>`;
    html += `<p class="expiry-footnote">${unknownItemized
      ? "渠道尚未返回完整的逐笔到期信息;已知记录会照常显示,不把未知说成「无到期」。"
      : "接口已返回完整明细,但没有可用于展示的到期记录。"}</p>`;
    return html;
  }

  const first = upcoming[0];
  if (first) {
    const p = expiryParts(first.at);
    const urgent = first.at * 1000 - Date.now() <= 7 * 86400e3;
    html += `<div class="expiry-summary${urgent ? " urgent" : ""}">
      <div class="expiry-kicker"><svg aria-hidden="true"><use href="#i-clock"/></svg>${c.expiryComplete === false ? "最近已知到期" : "最近一笔到期"}</div>
      <div class="expiry-date">${p.date}<small>${p.time}</small></div>
      <div class="expiry-meta"><span>${expiryRelativeSpan(first.at)}</span><strong>${points(first.amount)} 积分将到期</strong></div>
    </div>`;
  } else {
    html += `<div class="expiry-summary">
      <div class="expiry-kicker"><svg aria-hidden="true"><use href="#i-clock"/></svg>最近一笔到期</div>
      <div class="expiry-date" style="color:var(--tx3)">没有未到期</div>
      <div class="expiry-meta"><span>已知积分包均已过期,等待下次刷新</span><strong>${past.length} 笔已到期</strong></div>
    </div>`;
  }

  const head = showAll ? [...upcoming, ...past] : upcoming.length ? upcoming.slice(0, 3) : past.slice(0, 3);
  const total = upcoming.length + past.length;
  let translated = 0;
  const rows = head
    .map((e) => {
      const isPast = e.at <= nowS;
      const p = expiryParts(e.at);
      const urgent = !isPast && e.at * 1000 - Date.now() <= 7 * 86400e3;
      // 平台原始代号只进 tooltip:显示用中文,但对账时仍能查到原文。
      const rawLabel = String(e.label || "").trim() || "积分";
      const label = packLabelZh(rawLabel);
      if (label !== rawLabel) translated++;
      const tip = `${points(e.amount)} 分 · ${label}${label !== rawLabel ? ` · 平台原始代号:${rawLabel}` : ""}`;
      return `<div class="expiry-entry core-exp${isPast ? " past" : urgent ? " urgent" : ""}" title="${esc(tip)}">
        <div><b>${p.date} ${p.time}</b><p>${esc(label)}</p></div>
        <div class="expiry-amount"><strong>${points(e.amount)} <small>分</small></strong>
          <p>${isPast ? "已到期,待刷新" : expiryRelativeSpan(e.at)}</p></div>
      </div>`;
    })
    .join("");
  const more = total > head.length || showAll
    ? `<button class="sec-more" data-act="sec-more" data-id="${c.id}" data-sec="expiry" aria-expanded="${showAll ? "true" : "false"}">${
        showAll ? "收起" : `全部 ${total} 笔`
      }</button>`
    : "";
  html += `<div class="expiry-section-label">逐笔到期明细 · 最近到期优先${
    more ? `<span style="float:right">${more}</span>` : ""
  }</div><div class="expiry-table">${rows}</div>`;

  const notes = [];
  if (!upcoming.length) notes.push("没有未到期的已知积分包;已到期记录保留在列表里。");
  if (c.expiryComplete !== true) {
    notes.push("到期信息不完整:以上只是已获取的记录,不能当作全部积分的最近到期。");
  }
  const sumExp = all.reduce((n, e) => n + Number(e.amount || 0), 0);
  const remain = Number(c.remaining || 0);
  if (c.expiryComplete === true && remain - sumExp > 0.01) {
    notes.push(`已知到期记录合计 ${points(sumExp)} 分,少于当前余额 ${points(remain)} 分;其余积分没有已知到期日。`);
  } else if (c.expiryComplete === true && sumExp - remain > 0.01) {
    notes.push(`已知到期记录合计 ${points(sumExp)} 分,多于当前余额 ${points(remain)} 分;总额仍以后端余额为准。`);
  }
  notes.push("每笔显示尚未使用的积分;不同来源的积分分别到期。");
  if (translated) notes.push("来源代号已译为中文;悬停每行可查看平台原始代号。");
  html += `<p class="expiry-footnote">${notes.map(esc).join("<br>")}</p>`;
  if (isQoder) {
    html += `<p class="expiry-footnote">账户有效期与逐笔积分到期不同,不能据此判断所有积分在同一时间到期。</p>`;
  }
  return html;
}

/** 近 7 天消耗柱:直接聚合 channel_changes 的现成台账,不新增取数。
 *  零消耗的日子画一根很淡的基线;标题写"无记录",不把缺失数据说成 0。 */
function spendBars(c) {
  if (c.kind !== "amount" || changesState[c.id] === "loading") return "";
  const list = changesCache[c.id] || [];
  if (!list.length) return "";
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const days = Array(7).fill(0);
  for (const g of list) {
    if (!(g.delta < 0)) continue;
    const d = new Date(g.endTs * 1000);
    d.setHours(0, 0, 0, 0);
    const idx = Math.round((today.getTime() - d.getTime()) / 86400000);
    if (idx >= 0 && idx < 7) days[6 - idx] += -g.delta;
  }
  const max = Math.max(...days);
  if (!(max > 0)) return "";
  const bars = days
    .map((v, i) => {
      const date = new Date(today.getTime() - (6 - i) * 86400000);
      const ds = `${date.getMonth() + 1}-${String(date.getDate()).padStart(2, "0")}`;
      const tip = v > 0 ? `${ds} · ${money(v, c.unit || "CNY")}` : `${ds} · 无记录`;
      const h = Math.max(2, Math.round((v / max) * 34));
      return `<i class="${v > 0 ? "" : "zero"}" style="height:calc(${h}px*var(--u))" title="${esc(tip)}"></i>`;
    })
    .join("");
  return `<div class="sec"><div class="sec-h"><span>近 7 天消耗</span><span>推算</span></div><div class="sparkbars">${bars}</div></div>`;
}

/** ③ 最近变动:默认 3 条;先画占位,取到再替换。 */
function ledgerHtml(c, showAll) {
  const st = changesState[c.id];
  const list = changesCache[c.id] || [];
  if (st === "loading") {
    return `<div class="sec"><div class="sec-h"><span>最近变动</span></div><div class="lg-empty">读取中…</div></div>`;
  }
  if (st === "error") {
    return `<div class="sec"><div class="sec-h"><span>最近变动</span></div><div class="lg-empty">读不到变动记录</div></div>`;
  }
  if (!list.length) {
    // 确实取到了台账但没有变动:如实说"无记录";没取过则不装作知道
    if (st !== "done") return "";
    return `<div class="sec"><div class="sec-h"><span>最近变动</span></div><div class="lg-empty">7 天无变动记录</div></div>`;
  }
  const head = showAll ? list : list.slice(0, 3);
  const more =
    list.length > 3
      ? `<button class="sec-more" data-act="sec-more" data-id="${c.id}" data-sec="ledger" aria-expanded="${
          showAll ? "true" : "false"
        }">${showAll ? "收起" : `全部 ${list.length} 条`}</button>`
      : "";
  const rows = head
    .map((g) => {
      const amt = Math.abs(g.delta) < 0.005 ? "±0" : money(Math.abs(g.delta), c.unit || "CNY");
      const cls = g.delta < 0 ? "bad" : "ok";
      // 时间列只有 84px:今天的区间写 HH:MM–HH:MM(跨零点也只写钟点,紧挨着
      // 今天的条目看得懂);更早的只写结束时刻 MM-DD HH:MM。以前两头都带日期,
      // 「10-22 21:00–10-22 21:20」在三档字号下都被省略号切掉(2026-10-23 边界复核)。
      const when =
        g.startTs !== g.endTs && sameLocalDay(g.endTs, Date.now() / 1000)
          ? `${fmtClock(g.startTs)}–${fmtClock(g.endTs)}`
          : fmtStamp(g.endTs);
      const tail =
        g.samples > 1
          ? `推算消费(${g.samples} 次轮询)`
          : g.delta < 0
            ? "推算消费"
            : "入账(充值/额度)";
      const gapH =
        g.gapSecs > 7200 ? `<i class="gap">含 ${Math.round(g.gapSecs / 3600)} 小时未观测</i>` : "";
      return `<div class="lrow"><span class="lt">${when}</span><b class="${cls}">${
        g.delta < 0 ? "−" : "+"
      }${amt}</b><span class="lx">${tail}${gapH}</span></div>`;
    })
    .join("");
  return `<div class="sec"><div class="sec-h"><span>最近变动</span>${more}</div><div class="ledger">${rows}</div></div>`;
}

/** 失败原因在折叠行被省略号截断时,才把全文补到展开区页脚(设计 §4.3)。 */
function revealFailFull(rowEl) {
  if (!rowEl) return;
  const sub = rowEl.querySelector(".nm .s");
  const full = rowEl.querySelector(".fn-fail");
  if (sub && full && sub.scrollWidth > sub.clientWidth + 1) full.hidden = false;
}

function renderDetail(c, inDialog = false) {
  if (c.id === "google-cloud" && window.GoogleCloudUI) return window.GoogleCloudUI.detail(c);
  const noKey = !c.hasKey;
  const failed = c.hasKey && !c.valid && !c.stale;

  // 行详情只留一个短操作入口;具体设置集中在管理页。
  const manageLink = `<button class="btn s" data-act="open-manage" data-id="${c.id}">渠道设置</button>`;

  if (noKey) {
    // 网页登录型渠道(WorkBuddy):说清为什么不能自动读 —— 桌面客户端把令牌加密了,
    // 只能在本挂件里登录一次网页账号。别让人去开客户端,那没用。
    if (c.authSource === "web") {
      return `<div class="rbody-in">
        <div class="hint" style="margin:0 0 8px">读不到桌面端登录态;请在渠道设置里登录 ${esc(
          c.name
        )} 网页账号。</div>
        <div class="detail-actions">${manageLink}</div>
      </div>`;
    }
    // 复用本机登录态的渠道:没有"密钥"可填,只能引导用户去客户端登录一次
    if (c.authSource === "app") {
      return `<div class="rbody-in">
        <div class="hint" style="margin:0 0 8px">未检测到 ${esc(
          c.name
        )} 登录态。打开客户端登录后刷新;挂件只读凭据,不会改写或续期。</div>
        <div class="detail-actions">${manageLink}</div>
      </div>`;
    }
    return `<div class="rbody-in">
      <div class="hint" style="margin:0 0 8px">未配置密钥,当前没有数据。密钥保存在 Windows 凭据管理器。</div>
      <div class="detail-actions">${manageLink}</div>
    </div>`;
  }

  const state = hvRow(c.id) || {};
  const dim = failed || state.state === "stale" || !!c.stale;
  const more = rowMore[c.id] || {};

  // ① 核心(口径三选一);积分型统一走「最近一笔 + 逐笔明细」组件,
  // 不再按 id 分支 —— WorkBuddy / Trae / WorkBuddy AI / Qoder 共用同一套。
  let core;
  if (c.kind === "points") core = pointsExpiryHtml(c, !!more.expiry);
  else if (c.kind === "percent" && (c.windows || []).length) core = coreWindows(c);
  else core = coreAmount(c);

  // ② 明细:申请制四项 + extra。已经出现在行头或核心段的不再列(设计 §4.2):
  // 本轮额度 / 可再申请 / 套餐档位 / 额度包笔数;逐笔到期明细由核心段兜底文案承担。
  const claimKv = [];
  if (c.claim) {
    const cl = c.claim;
    claimKv.push([
      "上次申请",
      cl.lastClaimAt
        ? `${fmtDay(cl.lastClaimAt)}(${cl.source === "auto" ? "自动检测" : "手动"})`
        : "未记录",
    ]);
    if (cl.dailyBurn !== null && cl.dailyBurn !== undefined) {
      claimKv.push(["日均消耗(推算)", money(cl.dailyBurn, c.unit)]);
    }
    if (cl.daysOfBalance !== null && cl.daysOfBalance !== undefined) {
      claimKv.push(["预计可用", `约 ${Math.round(cl.daysOfBalance)} 天`]);
    }
    if (c.total !== null && c.total !== undefined) {
      claimKv.push(["累计发放", money(c.total, c.unit)]);
    }
  }
  // 只去掉**确实重复**的几条,不再按条数截断:截断会把「统计范围(只取了前 200 笔)」
  // 「套餐 · 未折进消耗」这类警示悄悄吞掉(2026-10-23 边界复核)。
  //   套餐         —— 只有 Codex 把它画进了副标题;Qoder / OpenCode Go 的套餐只在这里
  //   额度包/积分包 —— 笔数,核心段已经逐笔列出(有逐笔数据时才算重复)
  //   逐笔到期明细  —— 「接口未提供」,核心段的兜底文案已经说了
  const hasExpList = c.kind === "points" && (c.expiring || []).length > 0;
  const dupExtra = (k) =>
    (isAntigravity(c) && (c.windows || []).length && window.AntigravityUI && window.AntigravityUI.ownsExtra(k)) ||
    (k === "套餐" && c.id === "codex") ||
    ((k === "额度包" || k === "积分包") && hasExpList) ||
    (k === "逐笔到期明细" && c.kind === "points" && (!hasExpList ||
      ((c.id === "qoder" || c.id === "qoder-cn") && !window.ExpiryUI.qoderNotice(c).fresh))) ||
    // 账户额度到期已经由积分组件顶部的账户卡说了,不再在明细里重复
    (k === "账户额度到期" && c.kind === "points" && !!accountExpiryOf(c));
  const extras = (c.extra || []).filter(([k]) => !dupExtra(k));
  // 显示宽度:中日韩字符按 2 计。半栏大约放得下 26 个单位(键 + 值),
  // 再长就独占一行 —— 按字符数判会把「只取了前 200 笔,实际 230 笔」放进半栏切掉
  const dispW = (s) => [...String(s)].reduce((n, ch) => n + (/[\u2e80-\uffef]/.test(ch) ? 2 : 1), 0);
  const kv = [...claimKv, ...extras].map(([k, v]) => [k, String(v), dispW(k) + dispW(v) > 26]);
  // 窄的在前、宽的在后:宽条目夹在中间会把两栏网格切出半行空洞
  kv.sort((a, b) => Number(a[2]) - Number(b[2]));
  const kvHtml = kv.length
    ? `<div class="sec"><div class="kv">${kv
        .map(
          ([k, v, wide]) =>
            `<div${wide ? ' class="wide"' : ""}><span>${esc(k)}</span><b>${esc(v)}</b></div>`
        )
        .join("")}</div></div>`
    : "";

  // ⑤ 页脚说明:只留跟当前数据状态有关的提示(统计缺口 / 换过密钥 / 峰谷规则陈旧)。
  // 接口是否稳定、凭据从哪来属于固定套话,不再展示(2026-11-03)。
  const notes = [];
  if (c.gapDayFrom) {
    notes.push([`今日从 ${fmtClock(c.gapDayFrom)} 起才有数据,今日消耗可能偏低`, "warn"]);
  }
  if (c.accounts > 1) {
    notes.push(["已更换过密钥;这里只统计当前密钥,旧账户历史不混算", ""]);
  }
  if (c.peak && c.peak.tableStale) {
    notes.push(["节假日安排尚未更新,峰谷时段暂按周末规则估算", ""]);
  }
  const failFull =
    failed && c.error ? `<div class="fn-note bad fn-fail" hidden>取数失败:${esc(c.error)}</div>` : "";

  // ③ 最近变动(只金额型) ④ 签到一行(徽标已经说了"签没签",这里只说徽标没说的)
  const ledger = c.kind === "amount" ? ledgerHtml(c, !!more.ledger) : "";
  const spend = c.kind === "amount" ? spendBars(c) : "";
  let ckHtml = "";
  const cks = CK[c.id];
  if (CHECKIN_ON.has(c.id) && cks && cks.enabled) {
    const bits = [];
    if (cks.streakDays) bits.push(`连续 ${cks.streakDays} 天`);
    if (cks.monthCredits) bits.push(`本月 ${cks.monthCredits} 分`);
    if (cks.lastTryAt) bits.push(`上次尝试 ${relTime(cks.lastTryAt)}`);
    if (cks.nextTryAt && cks.nextTryAt * 1000 > Date.now()) bits.push(`下次 ${ckHold(cks)}`);
    if (bits.length) {
      ckHtml = `<div class="sec"><div class="ckline">签到 · ${bits.join(" · ")}</div></div>`;
    }
  }

  // ⑤ 页脚:说明在上、动作在下(右对齐)
  const notesHtml = notes.map(([t, cls]) => `<div class="fn-note${cls ? " " + cls : ""}">${esc(t)}</div>`).join("");
  const actions = [];
  if (MODEL_SOURCES.has(c.id) && !noKey) {
    const v = MDL.views[c.id];
    // 0 也是真实计数(key 没权限时 usableCount 就是 0):`||` 会把 0 当"未知"
    // 落到 total,误报成"230 个可用" —— 与模型页签自己的口径(usableAt 才显示)
    // 直接矛盾(报告 L2)。
    const n = v && v.usableAt ? v.usableCount : 0;
    actions.push(`<button class="btn s" data-act="open-models" data-id="${c.id}" title="浏览并配置 ${esc(c.name)} 的模型">模型配置${
      v && v.usableAt ? ` · ${n} 个可用` : ""
    }</button>`);
  }
  // 弹层底部已有「刷新 / 设置与管理」动作条,详情正文不再重复同一入口
  if (!inDialog) actions.push(manageLink);

  // 类名用 rb-dim 不用 dim:全局已有 .dim{color:var(--tx3)},撞名会把整张卡的数值再染一层灰
  queueMicrotask(() => tickPeakCountdowns());
  return `<div class="rbody-in${dim ? " rb-dim" : ""}">${core}${window.QuotaPlanUI?.detail(c) || ""}${qoderLoginBar(c)}${spend}${coreCodexResets(c)}${kvHtml}${ledger}${ckHtml}<div class="foot-det">${notesHtml}${failFull}<div class="fn-act">${actions.join("")}</div></div></div>`;
}

