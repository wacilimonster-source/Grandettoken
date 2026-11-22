// ───────────── 每日签到(Trae / WorkBuddy)─────────────
// 设计见 docs/design/design-auto-checkin-2026-10-13.html。三条不许破的规矩:
//   ① 活动监测与自动领取分开控制;关闭监测就一个请求都不发;
//   ② **不把"点了按钮"说成"领到了"**:数额只显示平台自己给的字段,拿不到就不写数字;
//   ③ 签到状态**不参与余额渲染** —— 签到失败永远不可能让余额显示成 0。
let QODER_LOGGING = false; // 「登录网页版」正在跑(逐包到期会话,qoderweb-login 事件收尾)

/**
 * 打开 Qoder 网页登录窗(逐包到期明细用)。设置页卡片与详情弹层共用这一个入口。
 * 结果走 qoderweb-login 事件:登录窗自动关,前端只管按钮与提示。
 * **必须带 id**:国际版与国内版是两套账号,后端据此选站点/Cookie 域/会话键。
 */
async function qoderWebLogin(id, switchAccount = false) {
  try {
    await invoke("qoderweb_login", { id, switch: !!switchAccount });
    QODER_LOGGING = true;
    renderChCards();
    if (DETAIL_ID) renderDetailSheet();
    // 已登录态点的是「重新登录」:提示别再说成"完成登录",它只是重新确认会话并刷新。
    const logged = window.ExpiryUI.qoderNotice(CHANNELS.find((x) => x.id === id) || {}).fresh;
    showToast(
      switchAccount
        ? "已退出旧网页登录 · 请用要使用的账号登录,完成后自动关窗"
        : logged
          ? "已打开网页会话窗口 · 确认后自动关窗并刷新明细"
          : "登录窗口已打开 · 完成登录后会自动关窗"
    );
  } catch (err) {
    showToast("打不开登录窗口:" + err, true);
  }
}
let CK = {};            // 渠道 id → checkin_state 的那一条
let CK_LOG = false;     // 「领取」正在跑
let CK_ACTIVITY_FILTER = "all";
const CHECKIN_ON = new Set(); // 能力名单来自 checkin_state，包含关闭监测的渠道。
const CHECKIN_QODER = new Set(["qoder", "qoder-cn"]);

/** 退避没到 / 今天已处理完:按钮留着但点不动,并写明还要等多久(报告 P0-6)。 */
function ckHold(s) {
  const left = s && s.nextTryAt ? Math.ceil((s.nextTryAt * 1000 - Date.now()) / 1000) : 0;
  if (left > 0) return (left >= 60 ? Math.round(left / 60) + " 分钟" : left + " 秒") + "后再试";
  return "今天已处理";
}

/** 各渠道的跨日时刻(与 core::checkin::CheckinDef::day_roll_hour 对应)。
 *  Qoder 是 10:00 —— 官方活动描述原文「每日 10:00（UTC+8）刷新」。 */
const CK_ROLL = { trae: 0, workbuddy: 0, qoder: 10, "qoder-cn": 10 };

/**
 * claim 之后的余额到账核对(**纯证据**,设计见 docs/design/design-arrival-confirm-2026-10-24.html)。
 *
 * 判定永远以平台回查为准(签到红线②),所以这里只有「确认得更实」的说法,
 * **没有「确认失败」**—— 取数间隔里的正常消耗会把 +100 压成负增量
 * (grants.rs 已论证过),见不到增量只能说"未见",明说"不代表没领到"。
 * 增量按纪律带「按余额推算」标注,不当平台事实说。
 *
 * 返回一句话(不带分隔符);后端还没给核对单(老版本 / 没取到基线)返回 null,
 * 调用方落回旧文案。
 */
function ckArrivalNote(s) {
  const a = s && s.arrival;
  if (!a) return null;
  if (a.status === "confirmed" && a.delta !== null && a.delta !== undefined) {
    const d = "+" + points(a.delta);
    const match =
      s.credits !== null &&
      s.credits !== undefined &&
      Math.abs(a.delta - s.credits) <= 5;
    return match
      ? "余额已确认到账(增量 " + d + ",与平台数额吻合)"
      : "余额已见增量 " + d + "(按余额推算)";
  }
  if (a.status === "pending") return "正在核对到账(随下一轮取数更新)";
  if (a.status === "unseen") return "余额未见增量(可能被消耗抵消,不代表没领到)";
  return null;
}

/**
 * 状态 → 一行字 + 一个按钮。八种状态照设计稿 7.2 的表。
 *
 * ⚠ **数额可能没有**(2026-10-21 真机):Trae / WorkBuddy 的签到接口常常**不返回**
 * 领到多少分(`credits` 为 null,本机日志 10-20 与 10-21 都是)。以前文案写死成
 * `"今日已领" + n + " 分"`,n 为空时就成了「今日已领 分」—— 悬空一个"分",
 * 用户看到的正是"显示不出领了多少分"。
 *
 * 规矩:有数额就报数额;**没有就明说"平台未返回数额"**,绝不留一个洞。
 * 这与签到红线一致 —— 数额只信平台给的字段,拿不到就不编。
 */
function ckText(s) {
  if (!s) return null;
  const hasN = s.credits !== null && s.credits !== undefined;
  const n = hasN ? " +" + s.credits : "";
  const amt = hasN ? " +" + s.credits + " 分" : "";
  const o = s.outcome || "";
  const activityTitle =
    (s.latestActivity && s.latestActivity.title) ||
    (o.startsWith("manual:") ? o.slice("manual:".length) : "");
  // 后端把"现在到底能不能点"算好了(can_try = 开关开 && 今天没完结 && 不在退避中)。
  // 以前前端不读它,退避中连点 N 次 = 对平台发 N×3 个请求,越限流撞得越凶。
  const canRun = s.canTry !== false;
  const run = (label, cls) => (canRun ? { btn: label, act: "ckrun", cls } : { btn: ckHold(s), disabled: true });
  if (!s.enabled) {
    // 关着也要把最近一次已知状态带上:上一轮手动验过/刚关掉的人,不该看不到"其实今天已领"
    // N18(2026-10-21):这里以前写死 " · 今天已领" + n + " 分",n 为空就成「今天已领 分」——
    // 与 0.1.52 修的是同一个洞,但当时只改了 claimed / already 两条,漏了这一条。
    const tail =
      o.startsWith("claimed") || o.startsWith("already")
        ? hasN
          ? " · 已领到" + amt
          : " · 已签到(数额未知)"
        : "";
    return { text: "活动监测未开启(关闭时不会发请求)" + tail, btn: "开启监测", act: "ckenable" };
  }
  if (s.note) return { text: s.note, btn: "去登录", act: "cklogin", warn: true };
  const activityState = s.activityState || {};
  if (activityState.status === "device_used") {
    return { text: "本设备名额已用" };
  }
  if (activityState.status === "claimable") {
    const scope = activityState.scope === "device" ? "本设备" : "当前账号";
    return Object.assign({ text: "发现可领取活动 · " + scope }, run("领取", "p"));
  }
  if (o.startsWith("confirming")) {
    return Object.assign(
      { text: s.lastMessage || "平台已收到领取请求,正在确认状态" },
      run("重新确认", "")
    );
  }
  // 连续天数只有 Qoder 的接口给(streak_days),有就带上 —— 比现有两个渠道多一个可展示项
  const streak = s.streakDays ? " · 连续 " + s.streakDays + " 天" : "";
  if (o.startsWith("claimed")) {
    // 到账核对:余额见着增量就说一声;没见着也不谎报失败(消耗可能把发放抵消掉)
    const an = ckArrivalNote(s);
    return {
      text:
        (hasN ? "已领到" + amt : "已签到(数额未知)") +
        streak +
        (an ? " · " + an : " · 已触发到账核对"),
    };
  }
  if (o.startsWith("already"))
    return {
      text: (hasN ? "已领到" + amt : "已签到(数额未知)") +
        streak + "(平台侧显示已签到)",
    };
  if (activityState.status === "manual" && (!o || o.startsWith("inactive"))) {
    return { text: "有活动:" + (activityState.title || activityTitle || "客户端活动") + " · 需在客户端参与" };
  }
  if (activityState.status === "inactive" && !o) {
    return { text: "本期没有可签的活动,等下一期" };
  }
  if (activityState.status === "claimed" && !o) {
    return { text: "平台已签" };
  }
  if (activityState.status === "unknown" && !o) {
    return { text: "活动状态暂时无法确认" };
  }
  if (o === "retry:ready") return Object.assign({ text: "今日可领" + n }, run("领取", "p"));
  // Qoder 的可领短码形如 retry:ready:{活动id} —— 同样落进"可领"
  if (o.startsWith("retry:ready:")) return Object.assign({ text: "今日可领" + n }, run("领取", "p"));
  if (o === "retry:device")
    return Object.assign(
      { text: "平台太挤 · 今天还会自动再试(第 " + (s.attempts || 1) + "/5 次)" },
      run("重试", "")
    );
  if (o.startsWith("retry"))
    return Object.assign(
      { text: "上次没成功,今天还会再试(第 " + (s.attempts || 1) + "/5 次)" },
      run("重试", "")
    );
  if (o.startsWith("auth")) return { text: "凭据失效 · 打开一次客户端,或在渠道卡登录", btn: "去登录", act: "cklogin", warn: true };
  // `recheck` = 上次那条 `auth` 是**旧版本**判的,而凭据链路可能已经修好了。
  // 后端在节拍器重新判定之前会这么报,界面**不该给「去登录」** —— 那会把用户引去网页登录,
  // 而更常见的情况是桌面端凭据本来就好用(2026-10-20 真机就是这么被误导的)。
  // N12(2026-10-21):以前这里只给 text、不给 btn,于是 ckRowHtml 连按钮都不画;
  // 而且"马上会再试一次"在**自动签到关着**时是句假话 —— 什么都不会发生,
  // 这个状态会一直挂到第二天。后端在这个状态下 can_try 是 true(陈旧的 auth 不挡当天),
  // 手动点完全跑得动,界面把入口露出来就行。
  if (o === "recheck")
    return Object.assign(
      {
        text: s.auto
          ? "正在重新确认凭据…(上次的失效判定已过期,马上会再试一次)"
          : "上次的凭据失效判定已过期,但自动签到是关着的 —— 点这里手动重试一次",
      },
      run("立即重试", "")
    );
  // `no_cred` = **本机没读到凭据**(没装桌面端 / 没登录 / DPAPI 解不开),
  // 与 `auth`(平台拒绝了这份凭据)是两件事,提示的方向也不同。
  if (o.startsWith("no_cred"))
    return { text: s.lastMessage || "本机没读到签到凭据 · 打开一次客户端即可", warn: true };
  // 无活动与"已结束"分开说:前者是本期没有打卡活动(活动换季),不是失败也不是已签
  if (o.startsWith("inactive")) return { text: s.lastMessage || "本期没有可签的活动,等下一期" };
  // manual = **有活动,但只能去客户端里领**(Qoder 的 VIEW_DETAILS 类活动)。
  // 不能说成"本期无活动" —— 用户在客户端里明明看得到那一条(2026-10-22 实证)。
  if (o.startsWith("manual"))
    return {
      text: "有活动:" + (activityTitle || "客户端里可查看") + " · 领取入口在客户端",
      warn: false,
    };
  if (o.startsWith("shape")) return { text: "签到接口有变动,已暂停自动签到 · 余额显示不受影响", warn: true };
  if (o.startsWith("failed")) return Object.assign({ text: "上次没成功:" + (s.lastMessage || o.slice(7)) }, run("重试", ""));
  return Object.assign({ text: "今天还没签" }, run("领取", "p"));
}

/**
 * 首页顶部「今天还有 N 个渠道没签到」提示条。
 *
 * **只在真有未签时出现**:全部签完就消失 —— "全部已签"是噪音不是信息;
 * 签到全关时也不出现(没开的人不该被提醒一件他没要求的事)。
 *
 * 2026-10-23 真机:只有一个渠道待签时,条上写「去查看」,点进去是签到设置区
 * (两个开关 + 流水),看完不知道要做什么 —— 真正的动作在渠道卡上。所以:
 *   · 只剩一个渠道时,把**渠道名和下一步**写进条上;能自己领就直接给「领取」;
 *   · 领不了(凭据失效 / 退避中 / 接口变动)就写清原因,落点改成**渠道卡的那一行**;
 *   · 多个渠道时保留汇总 + 去签到区(那里现在也画出每渠道状态与按钮)。
 */
function ckPendingStrip() {
  const ids = ckActiveIds();
  if (!ids.length) return "";
  const pending = ids.filter((id) => !(CK[id] && CK[id].doneToday));
  if (!pending.length) return "";

  if (pending.length === 1) {
    const id = pending[0];
    const s = CK[id] || {};
    const c = CHANNELS.find((x) => x.id === id) || {};
    const name = s.name || c.name || id;
    const detail = (ckText(s) || {}).text || "今天还没签";
    // `no_cred`(本机没读到凭据)虽然 can_try 是 true,但点一次只会再失败一次;
    // 该做的是去渠道卡打开客户端 / 重新登录,所以不给「领取」。
    const noCred = String(s.outcome || "").startsWith("no_cred");
    if (s.canTry !== false && !noCred) {
      // 后端把"现在到底能不能点"算好了:能点就直接把动作放在首页,少一次跳转
      return `<button type="button" class="ckstrip" data-act="ckrun" data-id="${esc(id)}"
        title="${esc(detail)}">${esc(name)} 还没签 · <b>领取</b></button>`;
    }
    // 不能自己领:别画成"可领取",说清是哪种,并把落点定到渠道卡上那一行
    return `<button type="button" class="ckstrip" data-act="ckstripopen" data-id="${esc(id)}"
      title="${esc(detail)}">${esc(name)} 签到待处理:${esc(ckShortReason(s))} · <b>去处理</b></button>`;
  }

  return `<button type="button" class="ckstrip" data-act="ckstripopen"
      title="打开签到中心">今天还有 <b>${pending.length}</b> 个渠道没签到 · 去查看</button>`;
}

/** 提示条用的短原因 —— 完整说法留在 tooltip 与渠道卡里,这里只够写一行。 */
function ckShortReason(s) {
  const o = String((s && s.outcome) || "");
  if (s && s.nextTryAt && s.nextTryAt * 1000 > Date.now()) return ckHold(s);
  if (o.startsWith("auth")) return "凭据失效";
  if (o.startsWith("no_cred")) return "本机没读到凭据";
  if (o.startsWith("shape")) return "接口有变动";
  if (o === "recheck") return "正在重新确认";
  if (o.startsWith("retry")) return "等自动重试";
  return "点开看详情";
}

/**
 * 首页卡片上的签到徽标。设计见 docs/product/product-home-checkin-badge-2026-10-21.md。
 *
 * 内部有十几种 short code,但对用户只有四态:**已签 / 可领 / 待签 / 需处理**。
 * 其余细节(reason、连续天数、退避剩余)全进 tooltip。
 *
 * 三条硬规矩:
 *   ① 签到**没开启**就一个像素都不画 —— 默认关,没开的人不该看到满屏"待签";
 *   ② `CK[id]` 还没读到就**不画** —— 宁可晚 200ms,也不先给假状态再跳变
 *      (这产品被"界面显示陈旧结论"坑过一次,见 0.1.50);
 *   ③ 徽标**不参与余额渲染** —— 颜色只表达签到态,绝不影响金额与状态点。
 */
function ckBadgeHtml(c) {
  if (!CHECKIN_ON.has(c.id)) return "";
  if (c.hidden) return "";
  const s = CK[c.id];
  if (!s || !s.enabled) return "";
  const o = String(s.outcome || "");
  const n = s.credits === null || s.credits === undefined ? "" : " +" + s.credits;
  const tip = esc(ckBadgeTip(s));
  if (s.activityState && s.activityState.status === "claimable") {
    return s.canTry === false
      ? `<span class="ckb" title="${tip}">可领</span>`
      : `<button class="ckb hot" data-act="ckrun" data-id="${esc(c.id)}" title="${tip}">可领</button>`;
  }
  if (s.activityState && s.activityState.status === "device_used")
    return `<span class="ckb ok" title="${tip}">本设备已签</span>`;
  if (s.activityState && s.activityState.status === "manual")
    return `<span class="ckb info" title="${tip}">有活动</span>`;
  if (s.activityState && s.activityState.status === "inactive" && (!o || o.startsWith("inactive")))
    return `<span class="ckb ok" title="${tip}">本期无活动</span>`;
  if (s.activityState && s.activityState.status === "claimed" && !o)
    return `<span class="ckb ok" title="${tip}">平台已签</span>`;
  if (s.activityState && s.activityState.status === "unknown" && !o)
    return `<span class="ckb" title="${tip}">状态未知</span>`;
  if (o.startsWith("confirming"))
    return `<span class="ckb" title="${tip}">确认中</span>`;

  // claimed = **我们刚替他领到了** → 与 already 一样只说「已签」,数额进 tooltip。
  // 2026-11-03:以前 claimed 有数额就画「✓ +100」,而 Qoder 的领取接口每次都回数额,
  // 于是同一枚徽标在 Qoder 上是 +100、在其它渠道是「已签」—— 统一成「已签」。
  if (o.startsWith("claimed")) return `<span class="ckb ok" title="${tip}">✓ 已签</span>`;
  // already = **平台侧已签**(可能是他自己在客户端点的)→ 只说"已签",数额进 tooltip。
  // 写成 "+100" 会让人以为是我们领的 —— 签到红线第 2 条(不把点了按钮说成领到了)。
  if (o.startsWith("already")) return `<span class="ckb ok" title="${tip}">✓ 已签</span>`;
  // inactive = 本期无活动:不是"已签",但今天确实没有待办,聚合里算它一份
  if (o.startsWith("inactive")) return `<span class="ckb ok" title="${tip}">本期无活动</span>`;
  // manual = 有活动,但只能在客户端领:要让人看见"有事可做",所以不用灰色的"本期无活动"
  if (o.startsWith("manual")) return `<span class="ckb info" title="${tip}">有活动</span>`;
  if (o.startsWith("retry:ready")) {
    // 后端把"能不能点"算好了(canTry):退避中就别给一个点了没反应的按钮
    return s.canTry === false
      ? `<span class="ckb" title="${tip}">待签</span>`
      : `<button class="ckb hot" data-act="ckrun" data-id="${esc(c.id)}" title="${tip}">可领${esc(n)}</button>`;
  }
  if (o.startsWith("auth")) {
    return `<button class="ckb warn" data-act="open-manage" data-id="${esc(c.id)}" title="${tip}">需登录</button>`;
  }
  if (o.startsWith("shape")) {
    return `<button class="ckb warn" data-act="open-manage" data-id="${esc(c.id)}" title="${tip}">已暂停</button>`;
  }
  if (o.startsWith("no_cred")) {
    return `<button class="ckb" data-act="open-manage" data-id="${esc(c.id)}" title="${tip}">未就绪</button>`;
  }
  // 空 / retry:* / recheck / failed → 一律"待签"(细节在 tooltip)
  return `<span class="ckb" title="${tip}">待签</span>`;
}

/** 徽标的 tooltip:把 short code 翻成一句人话,再补上关键时间。 */
function ckBadgeTip(s) {
  const o = String(s.outcome || "");
  const bits = [];
  if (o.startsWith("claimed")) {
    bits.push("已领到" + (s.credits ? " +" + s.credits + " 分" : ""));
    const an = ckArrivalNote(s);
    if (an) bits.push(an);
  }
  else if (o.startsWith("already"))
    bits.push("平台显示今天已签到" + (s.credits ? "(数额 " + s.credits + " 分)" : ""));
  else if (o.startsWith("inactive")) bits.push(s.lastMessage || "本期没有可签的活动,等下一期");
  else if (o.startsWith("manual"))
    bits.push("有活动:" + (s.latestActivity && s.latestActivity.title ? s.latestActivity.title : o.slice("manual:".length)) + " —— 领取入口在客户端里,挂件领不了");
  else if (o.startsWith("retry:ready"))
    bits.push("今天可领" + (s.credits ? " +" + s.credits + " 分" : ""));
  else if (o.startsWith("auth")) bits.push("凭据失效 —— 打开一次客户端");
  else if (o.startsWith("no_cred")) bits.push(s.lastMessage || "本机没读到签到凭据");
  else if (o.startsWith("shape")) bits.push("签到接口有变动,已暂停自动签到(余额不受影响)");
  else if (o === "recheck") bits.push("正在重新确认凭据");
  else if (o.startsWith("retry")) bits.push("上次没成功,今天还会自动再试");
  else bits.push("今天还没签");
  if (s.streakDays) bits.push("连续 " + s.streakDays + " 天");
  if (s.latestActivity && s.latestActivity.title)
    bits.push("最近活动: " + s.latestActivity.title);
  if (s.activityState) {
    bits.push((ACTIVITY_LABELS[s.activityState.status] || ACTIVITY_LABELS.unknown) + " · " + (s.activityState.scope === "device" ? "设备级" : "账号级"));
  }
  if (s.monthCredits) bits.push("本月共 " + s.monthCredits + " 分");
  if (s.nextTryAt) {
    const left = Math.ceil((s.nextTryAt * 1000 - Date.now()) / 1000);
    if (left > 0) {
      bits.push((left >= 60 ? Math.round(left / 60) + " 分钟" : left + " 秒") + "后再试");
    }
  } else if (!s.doneToday) {
    // 还没到本渠道的跨日时刻:必须说清是几点,不能说"明天"
    // (Qoder 的"今天"从 10:00 起算 —— 说"明天"会让人以为今天没戏了)
    // CK_ROLL 是**北京时间**(UTC+8),本地 getHours() 在别的时区会差出时区差
    // —— 与后端判定的 canTry/doneToday 口径对不上(报告 L5)。
    const roll = CK_ROLL[s.id] || 0;
    const bjHour = new Date(Date.now() + (new Date().getTimezoneOffset() + 480) * 60000).getHours();
    if (roll > 0 && bjHour < roll) bits.push(roll + ":00 后可签");
  }
  return bits.join(" · ");
}

// 活动状态统一标签:渠道卡行 / 签到中心 / 徽标 tooltip 三处共用,防止叫法漂移
const ACTIVITY_LABELS = {
  claimable: "可领取",
  manual: "需客户端参与",
  confirming: "领取确认中",
  claimed: "已领取",
  device_used: "本设备名额已用",
  inactive: "本期无活动",
  unknown: "暂时无法确认",
};

function ckRowHtml(id) {
  const s = CK[id];
  if (!s) return "";
  const t = ckText(s);
  if (!t) return "";
  const activity = s.activityState;
  const scopeLabel = activity && activity.scope === "device" ? "设备级" : "账号级";
  const regionLabel = activity && activity.region === "CN" ? "国内版" : activity && activity.region === "Global" ? "国际版" : "";
  const activityLine = activity && s.enabled
    ? '<div class="ckactivity" title="' + esc(activity.detail || activity.title || "") + '">' +
      "活动监测 · " + esc(activity.title || "签到状态") + " · " +
      esc(ACTIVITY_LABELS[activity.status] || ACTIVITY_LABELS.unknown) + " · " +
      esc([regionLabel, scopeLabel].filter(Boolean).join(" / ")) +
      "</div>"
    : !activity && CHECKIN_QODER.has(id) && s.latestActivity && s.latestActivity.title
      ? '<div class="ckactivity">活动监测 · ' + esc(s.latestActivity.title) + "</div>"
      : "";
  return (
    '<div class="ckrow' + (t.warn ? " warn" : "") + '" data-id="' + esc(id) + '">' +
    "<span>" + esc(t.text) + "</span>" +
    (t.btn
      ? '<button class="btn s ' + (t.cls || "") + '" data-act="' + (t.act || "") + '" data-id="' + esc(id) + '"' +
        (t.disabled ? " disabled" : "") +
        ">" + esc(t.btn) + "</button>"
      : "") +
    "</div>" + activityLine
  );
}

/** 首页的统一视图(七态 / 五桶 / 绑定窗)。
 *
 * 取数走 `fetch_shared` —— 与 `get_channels` 同一个闸门、同一份 5 秒内的近期结果,
 * 所以紧贴着调一次不会多打一次账务接口。失败时保留上一份视图,四个渲染器退回本地判断。
 */
async function loadHomeView() {
  try {
    hvApply(await invoke("home_view"));
  } catch (e) {
    // silent: 视图拿不到不该让首页变空白,四个渲染器各自退回 toneOf / sortChannels。
  }
}

/**
 * 读一次签到状态。**带序号**:两次并发读可能乱序返回 —— 启动时首轮 refresh 与
 * 后台签到的 `checkin-updated` 就会撞在一起,晚到的旧快照会把新结论盖回去。
 * 只让最后一次发出的请求落地(与 fetch_shared 的合流同一个道理)。
 */
let CK_SEQ = 0;
async function loadCheckin() {
  const seq = ++CK_SEQ;
  let list = null;
  try {
    list = await invoke("checkin_state");
  } catch {
    // silent: 读失败 ≠ 签到状态是空的(报告 L7,loadVault F3 同一条纪律):保留上次读到的那份,
    // 宁可旧一点也不把徽标 / 待签提示条整体抹掉,装作"没有待签"。
  }
  if (seq !== CK_SEQ) return; // 更晚的一次已经发出,这份结果作废
  let capabilitiesChanged = false;
  if (list) {
    const ids = list.map((x) => x.id);
    capabilitiesChanged = ids.length !== CHECKIN_ON.size || ids.some((id) => !CHECKIN_ON.has(id));
    CHECKIN_ON.clear();
    CK = {};
    for (const x of list) { CK[x.id] = x; CHECKIN_ON.add(x.id); }
  }
  ckTabUpdate();
  // 渠道卡与签到中心都跟着更新(哪个在场更新哪个)
  if (document.body.classList.contains("view-manage")) {
    if (capabilitiesChanged) renderChCards();
    renderCheckinCenter();
    document.querySelectorAll("#chList .ckwrap").forEach((el) => {
      el.innerHTML = ckRowHtml(el.dataset.id);
    });
  }
}

function activityStatusView(status) {
  return { label: ACTIVITY_LABELS[status] || ACTIVITY_LABELS.unknown, kind: status || "unknown" };
}

function renderActivityOverview() {
  const entries = [];
  const now = Math.floor(Date.now() / 1000);
  for (const id of CHECKIN_ON) {
    const state = CK[id];
    if (!state || !state.enabled) continue;
    const channel = CHANNELS.find((item) => item.id === id);
    if (channel && channel.hidden) continue;
    const region = id === "qoder" ? "国际版" : "国内版";
    const history = CHECKIN_QODER.has(id) && Array.isArray(state.activityHistory)
      ? state.activityHistory.filter((item) => state.lastActivityCheckAt
          ? item.observedAt === state.lastActivityCheckAt
          : !item.observedAt || now - item.observedAt < 24 * 3600)
      : [];
    if (history.length) {
      for (const item of history) {
        const canClaim =
          (item.actionType === "CLAIM_BENEFIT" || item.actionType === "CLAIM_BEFIT") &&
          item.claimStatus === "CLAIMABLE";
        const status = canClaim
          ? "claimable"
          : item.claimStatus === "CLAIMABLE"
            ? "manual"
            : item.claimStatus === "CLAIMED"
              ? "claimed"
              : ["EXPIRED", "ENDED"].includes(item.claimStatus)
                ? "inactive"
                : "unknown";
        entries.push({
          channel: state.name || id,
          id,
          key: item.id,
          title: item.title || "Qoder 活动",
          status,
          scope: "账号级",
          region,
          observedAt: item.observedAt,
        });
      }
      continue;
    }
    const activity = state.activityState;
    entries.push({
      channel: state.name || id,
      id,
      key: activity && activity.key ? activity.key : id,
      title: activity && activity.title ? activity.title : "等待首次状态检查",
      status: activity && activity.status ? activity.status : "unknown",
      scope: activity && activity.scope === "device" ? "设备级" : "账号级",
      region: activity && activity.region === "Global" ? "国际版" : activity && activity.region === "CN" ? "国内版" : region,
      observedAt: activity && activity.observedAt,
    });
  }
  const actionable = entries.filter((item) => ["claimable", "manual", "confirming", "unknown"].includes(item.status));
  const completed = entries.filter((item) => ["claimed", "device_used", "inactive"].includes(item.status));
  const visible = entries.filter((item) =>
    CK_ACTIVITY_FILTER === "action" ? ["claimable", "manual", "confirming", "unknown"].includes(item.status) :
    CK_ACTIVITY_FILTER === "done" ? ["claimed", "device_used", "inactive"].includes(item.status) : true
  );
  const filters = [
    ["all", "全部 " + entries.length],
    ["action", "需处理 " + actionable.length],
    ["done", "已完成 " + completed.length],
  ];
  const rows = visible.map((item) => {
    const badge = activityStatusView(item.status);
    const time = item.observedAt
      ? new Date(item.observedAt * 1000).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })
      : "尚未检查";
    const action = item.status === "claimable"
      ? '<button class="btn s p" data-act="ckrun" data-id="' + esc(item.id) + '"' +
        (CK[item.id] && CK[item.id].canTry === false ? " disabled" : "") + '>领取</button>'
      : "";
    return '<article class="ck-activity-item">' +
      '<div class="ck-activity-head"><span>' + esc(item.channel) + " · " + esc(item.region) + " / " + esc(item.scope) + '</span>' +
      '<span class="ck-activity-status ' + esc(badge.kind) + '">' + esc(badge.label) + '</span></div>' +
      '<div class="ck-activity-title" title="' + esc(item.key) + '">' + esc(item.title) + '</div>' +
      '<div class="ck-activity-foot"><span>' + esc(time) + '</span>' + action + '</div>' +
      '</article>';
  }).join("");
  return '<div class="ck-overview"><div class="ck-overview-head"><b>活动状态</b>' +
    '<span>可领取 ' + entries.filter((x) => x.status === "claimable").length +
    ' · 客户端参与 ' + entries.filter((x) => x.status === "manual").length +
    ' · 确认中 ' + entries.filter((x) => x.status === "confirming").length +
    ' · 已确认 ' + entries.filter((x) => x.status === "claimed" || x.status === "device_used").length + '</span></div>' +
    '<div class="ck-overview-filters" role="group" aria-label="筛选活动">' + filters.map(([key, label]) =>
      '<button type="button" class="ckfilter' + (CK_ACTIVITY_FILTER === key ? " on" : "") +
      '" aria-pressed="' + (CK_ACTIVITY_FILTER === key) + '" data-act="ckfilter" data-filter="' + key + '">' + esc(label) + '</button>'
    ).join("") + '</div>' +
    '<div class="ck-activity-list">' + (rows || '<div class="ck-overview-empty">当前筛选下没有活动 · 点「全部」看所有</div>') + '</div>' +
    '</div>';
}

// ───────────── 签到中心(独立页签)─────────────
// 设计见 docs/product/product-checkin-center-2026-10-29.md。五段:今日总览 + 逐渠道行 /
// 活动状态 / 近 30 天日历带 / 明细流水(90 天)/ 管理开关。首页签到格与多渠道待签
// 提示条都落到这里(格子 = 看,提示条 = 办,职责分开)。
const CK_CENTER_OPEN = new Set(); // 刷新状态时保留用户展开的分区
let CKH = null;          // checkin_history 的返回(日历带 + 明细表)
let CKH_SEQ = 0;         // 与 loadCheckin 同一个道理:晚到的旧快照不许盖新的
let CKH_ERR = false;     // 读失败 ≠ "正在读取"(报告 L7):失败要明说并给重试
let CKH_FILTER = "all";  // 明细表的渠道筛选
const CKH_DAYS = 90;     // 明细表窗口(后端 clamp 到 7~365)
const CKH_BAND = 30;     // 日历带窗口(天)
const CKH_TABLE_CAP = 120; // 明细表最多画多少行 —— 90 天 × 每小时重试能把表撑到上千行

/** 流水短码 → 结果字。与 ckText 同一口径:already 绝不说成"领到"。 */
function ckOutcomeLabel(o) {
  o = String(o || "");
  if (o.startsWith("claimed")) return "领到";
  if (o.startsWith("already")) return "平台已签";
  if (o.startsWith("inactive")) return "无活动";
  if (o.startsWith("manual")) return "需客户端";
  if (o.startsWith("confirming")) return "确认中";
  if (o.startsWith("auth")) return "凭据失效";
  if (o.startsWith("shape")) return "接口变动";
  if (o.startsWith("no_cred")) return "没读到凭据";
  if (o.startsWith("retry")) return "未成功";
  if (o.startsWith("failed")) return "失败";
  return o || "—";
}

/** 后端逐天结论 → 日历格的状态类与说明字。status 由后端给(周期日口径),
 *  前端只翻译,不算日期。 */
function ckDayCell(s) {
  return {
    claimed: ["领到", "ok"],
    already: ["平台已签", "ok"],
    inactive: ["本期无活动", "ok"],
    manual: ["有活动 · 在客户端", "info"],
    failed: ["没签成", "warn"],
    missed: ["没签", ""],
    pending: ["今天待签", "today"],
    no_data: ["无记录", "dim"],
  }[s] || [s, ""];
}

/** 读一次签到历史(签到中心页签打开时取,后台签完也要重取)。 */
async function loadCheckinHistory() {
  const seq = ++CKH_SEQ;
  let h = null;
  try {
    h = await invoke("checkin_history", { days: CKH_DAYS });
    CKH_ERR = false;
  } catch (e) {
    // 读失败如实说 + 给重试(报告 L7,F3 纪律):以前吞掉后 CKH 停在 null,
    // 渲染成一句永远不结束的"正在读取签到记录…"
    CKH_ERR = true;
  }
  if (seq !== CKH_SEQ) return;
  CKH = h;
  renderCkHist();
}

/** 打开签到中心:首页签到格 / 多渠道待签提示条的落点。 */
function openCheckinCenter() {
  openManage();
  showTab("ck"); // 流水由 showTab 统一读取,这里只补页面本身的渲染
  renderCheckinCenter();
}

/** 签到页签的可见性:有渠道开着监测才出现。全部关闭时整页签隐藏(开启入口在
 *  渠道卡),页签栏少一项比一个空页签干净 —— 与"不该提醒一件用户没要求的事"同一原则。 */
function ckTabUpdate() {
  const btn = document.querySelector('#mTabs [data-p="ck"]');
  if (!btn) return;
  let any = false;
  for (const id of CHECKIN_ON) if (CK[id] && CK[id].enabled) { any = true; break; }
  btn.hidden = !any;
  if (!any && btn.classList.contains("on")) showTab("ch");
}

/** 签到中心页签。CK 还没读到时先给一句"正在读取"(宁可晚 200ms,不先给假状态)。 */
function renderCheckinCenter() {
  const box = $("ckCenter");
  if (!box) return;
  if (!Object.keys(CK).length) {
    box.innerHTML = '<div class="cksec"><div class="ckempty">正在读取签到状态…</div></div>';
    return;
  }
  let anyOn = false;
  for (const id of CHECKIN_ON) if (CK[id] && CK[id].enabled) { anyOn = true; break; }
  if (!anyOn) {
    box.innerHTML =
      '<div class="cksec"><div class="ckempty">没有开启活动监测的渠道 —— 在「渠道」页签点开渠道卡,打开「活动监测」即可。</div></div>';
    return;
  }
  box.innerHTML =
    '<div id="ckTodayBox"></div>' +
    [['activity', '活动详情', '查看活动与领取状态', 'ckActsBox'],
     ['history', '签到记录', '近 30 天概览与 90 天明细', 'ckHistBox'],
     ['settings', '签到设置', '管理监测与自动领取', 'ckManageBox']]
      .map(([key, title, hint, id]) => '<details class="ck-fold" data-ck-section="' + key + '"' +
        (CK_CENTER_OPEN.has(key) ? ' open' : '') + '><summary><span><b>' + title +
        '</b><small>' + hint + '</small></span></summary><div class="ck-fold-body" id="' + id + '"></div></details>')
      .join('');
  box.querySelectorAll('[data-ck-section]').forEach((section) => {
    section.addEventListener('toggle', () => {
      if (section.open) CK_CENTER_OPEN.add(section.dataset.ckSection);
      else CK_CENTER_OPEN.delete(section.dataset.ckSection);
    });
  });
  renderCkToday();
  renderCkActs();
  renderCkHist();
  renderCkManage();
}

/** ① 今日总览条 + ② 今日逐渠道行(没签的排前面,签完的靠后)。 */
function renderCkToday() {
  const box = $("ckTodayBox");
  if (!box) return;
  const ids = ckActiveIds();
  if (!ids.length) {
    box.innerHTML = "";
    return;
  }
  const done = ids.filter((id) => CK[id] && CK[id].doneToday).length;
  const all = done >= ids.length;
  const month = ids.reduce((n, id) => n + ((CK[id] && CK[id].monthCredits) || 0), 0);
  const tone = all ? " ok" : ids.some((id) => CK[id] && CK[id].needsUser) ? " warn" : "";
  // 没签的排前面 —— 从别处跳进来的人,要看的正是没签的那几行
  const order = ids.slice().sort((a, b) => (CK[a].doneToday ? 1 : 0) - (CK[b].doneToday ? 1 : 0));
  box.innerHTML =
    '<div class="cksec cktoday-section"><h4>今日签到</h4>' +
    '<div class="cktoday' + tone + '"><b>' + (all ? "✓ " : "今日进度 ") + done + "/" + ids.length +
    '</b><span>本月已领 ' + points(month) + ' 分</span></div>' +
    order
      .map(
        (id) =>
          '<div class="ckch" data-ch="' + esc(id) + '"><b>' + esc((CK[id] && CK[id].name) || id) + "</b>" +
          '<div class="ckwrap" data-id="' + esc(id) + '">' + ckRowHtml(id) + "</div></div>"
      )
      .join("") +
    "</div>";
}

/** ③ 活动状态(原设置页那块原样搬来;筛选只重画这一段,不惊动别的段)。 */
function renderCkActs() {
  const box = $("ckActsBox");
  if (box) box.innerHTML = renderActivityOverview();
}

/** 日历格的 tooltip:日期 + 结果 + 数额 + 尝试次数,一格说清一天。 */
function ckCellTitle(cell, name) {
  const bits = [cell.day];
  if (cell.ts) {
    bits.push(new Date(cell.ts * 1000).toLocaleString("zh-CN", { hour12: false }));
  }
  const [label] = ckDayCell(cell.status);
  bits.push(label + (cell.status === "claimed" && cell.credits ? " +" + cell.credits + " 分" : ""));
  if (cell.attempts > 1) bits.push("尝试 " + cell.attempts + " 次");
  return (name ? name + " · " : "") + bits.join(" · ");
}

/** ④ 近 30 天日历带(每渠道一条)+ 明细流水表(90 天,渠道筛选)。 */
function renderCkHist() {
  const box = $("ckHistBox");
  if (!box) return;
  if (!CKH) {
    // 读失败要明说并给重试入口(报告 L7,F3 纪律),不画成一句永远不结束的"正在读取"
    box.innerHTML = CKH_ERR
      ? '<div class="cksec"><h4>近 30 天</h4><div class="ckempty">签到记录读不出来,可能是数据库忙了。<button class="btn s" data-act="ckhretry">重试</button></div></div>'
      : '<div class="cksec"><h4>近 30 天</h4><div class="ckempty">正在读取签到记录…</div></div>';
    return;
  }
  const active = new Set(ckActiveIds());
  // 日历带只画现役渠道(开着监测且没被隐藏):"没签"只对"该签的渠道"有意义;
  // 关掉/隐藏的渠道,流水仍全量留在下面的明细表里。
  const bands = CKH.channels
    .filter((ch) => active.has(ch.id))
    .map((ch) => {
      const days = (ch.days || []).slice(-CKH_BAND);
      if (!days.length) return "";
      const done30 = days.filter((d) => d.day !== ch.today && ["claimed", "already", "inactive", "manual"].includes(d.status)).length;
      const miss30 = days.filter((d) => d.status === "missed" || d.status === "failed").length;
      const cells = days
        .map((d) => '<i class="ckc ' + d.status + '" title="' + esc(ckCellTitle(d, ch.name)) + '"></i>')
        .join("");
      return (
        '<div class="ckband"><span class="ckband-name">' + esc(ch.name) + "</span>" +
        '<span class="ckband-cells">' + cells + "</span>" +
        '<span class="ckband-sum">完成 ' + done30 + (miss30 ? " · 漏 " + miss30 : "") + "</span></div>"
      );
    })
    .join("");
  const legend =
    '<div class="cklegend">' +
    [["claimed", "领到"], ["already", "平台已签"], ["missed", "没签"], ["failed", "没签成"], ["no_data", "无记录"]]
      .map(([s, label]) => '<i class="ckc ' + s + '"></i>' + esc(label))
      .join(" ") +
    "</div>";
  const names = {};
  for (const ch of CKH.channels) names[ch.id] = ch.name;
  const all = CKH.rows || [];
  const shown = all.filter((r) => CKH_FILTER === "all" || r.id === CKH_FILTER).slice(0, CKH_TABLE_CAP);
  const chips = [["all", "全部 " + all.length]].concat(
    CKH.channels.map((ch) => [ch.id, names[ch.id] || ch.id])
  );
  const table = shown
    .map((r) => {
      const bad = /^(auth|shape|no_cred|failed|retry|confirming)/.test(r.outcome || "");
      return (
        '<div class="cklr' + (r.outcome && r.outcome.startsWith("claimed") ? " ok" : bad ? " warn" : "") + '">' +
        '<span class="cklt">' + esc(new Date(r.ts * 1000).toLocaleString("zh-CN", { hour12: false })) + "</span>" +
        '<span class="ckln">' + esc(r.name || r.id) + "</span>" +
        '<span class="cklo">' + esc(ckOutcomeLabel(r.outcome)) + "</span>" +
        '<span class="cklv">' + (r.credits !== null && r.credits !== undefined ? "+" + points(r.credits) : "") + "</span>" +
        '<span class="ckld" title="' + esc(r.detail || "") + '">' + esc(r.detail || "") + "</span></div>"
      );
    })
    .join("");
  box.innerHTML =
    '<div class="cksec"><h4>近 30 天</h4>' +
    (bands || '<div class="ckempty">窗口内还没有签到记录</div>') +
    legend +
    "</div>" +
    '<div class="cksec"><h4>明细(近 90 天)</h4>' +
    '<div class="ck-overview-filters" role="group" aria-label="按渠道筛选">' +
    chips
      .map(
        ([k, label]) =>
          '<button type="button" class="ckfilter' + (CKH_FILTER === k ? " on" : "") +
          '" aria-pressed="' + (CKH_FILTER === k) + '" data-act="ckhfilter" data-filter="' + esc(k) + '">' +
          esc(label) + "</button>"
      )
      .join("") +
    "</div>" +
    '<div class="cktable">' +
    (table || '<div class="ckempty">当前筛选下没有流水</div>') +
    "</div>" +
    (all.length > shown.length ? '<div class="ckmore">只显示最近 ' + shown.length + " 条(共 " + all.length + " 条)</div>" : "") +
    "</div>";
}

/** ⑤ 管理区:每渠道两个开关 + 免责(从「刷新与启动」搬来;今日状态在上面第②段)。 */
function renderCkManage() {
  const box = $("ckManageBox");
  if (!box) return;
  const rows = [];
  for (const id of CHECKIN_ON) {
    const s = CK[id];
    const on = s ? s.enabled : false;
    const auto = s ? s.auto : true;
    rows.push(
      '<div class="field"><span>' + esc(s ? s.name : id) +
        '<span class="desc">' + (on ? "每小时检查一次" : "已关闭 · 不再请求") + "</span></span>" +
        '<button class="sw' + (on ? " on" : "") + '" role="switch" aria-checked="' + on +
        '" data-act="ckenable" data-id="' + esc(id) + '" aria-label="' + esc((s ? s.name : id) + " 活动监测") + '"></button></div>' +
      (on
        ? '<div class="field"><span>自动领取<span class="desc">' +
          (auto ? "可直接领的自动领" : "只提示,不代领") +
          '</span></span><button class="sw' + (auto ? " on" : "") + '" role="switch" aria-checked="' + auto +
          '" data-act="ckauto" data-id="' + esc(id) + '" aria-label="自动领取"></button></div>'
        : "")
    );
  }
  box.innerHTML =
    '<div class="cksec"><h4>管理</h4>' + rows.join("") +
    '<div class="khint">只读平台状态;自动领取单独控制。<button class="why" data-why="checkin" aria-expanded="false" aria-label="活动监测的说明"></button></div></div>';
  // 段是动态重建的,「?」的水合要跟着跑一遍(与渠道卡同一条规矩)
  hydrateWhy(box);
}

async function ckRun(id) {
  CK_LOG = true;
  try {
    const st = await invoke("checkin_now", { id });
    CK[st.id] = st;
    const t = ckText(st);
    if (st.outcome && st.outcome.startsWith("claimed")) showToast("已领到" + (st.credits ? " +" + st.credits + " 分" : ""));
    else if (st.outcome && st.outcome.startsWith("already")) showToast("平台已签");
    else showToast(t && t.text ? t.text : "没签成", true);
  } catch (e) {
    showToast(String(e), true);
  }
  CK_LOG = false;
  await loadCheckin();
  // 首页那个「领取」按完,条上要立刻变成 3/3 或"待处理",不能等下一轮轮询;
  // 签到中心开着时,流水表与日历带也要立刻带上这一次的记录
  if (manageOpen()) {
    renderChCards();
    loadCheckinHistory();
  } else if (CHANNELS.length) render({ updated: false });
}

async function ckSetEnabled(id, on, auto) {
  const cur = CFG.checkin && CFG.checkin[id];
  CFG.checkin = CFG.checkin || {};
  CFG.checkin[id] = Object.assign(
    { enabled: false, auto: true, attempts: 0 },
    cur || {},
    on === null ? {} : { enabled: on },
    auto === null || auto === undefined ? {} : { auto }
  );
  if (!(await saveCfg("签到设置"))) return;
  if (on) {
    showToast("已开启 " + (CK[id] ? CK[id].name : id) + " 活动监测");
  }
  loadCheckin();
}

