/** 设置页「渠道」页签:一卡一渠道 —— 显隐、排序、密钥、额度申请全在卡里。
    (旧版把这三件事拆在「密钥 / 渠道顺序 / 额度申请」三个分区,是"乱"的根源) */
const cardOpen = new Set(); // 展开的卡片 id;轮询重建时保持,不让用户白收起

/** 积分获取记录(只有 WorkBuddy AI 这类"只在余额上留痕"的渠道有)。
 *  `grantOpen` 是展开状态,`GRANTS` 是明细缓存 —— 打开时才查,不在每轮轮询里读库。 */
const grantOpen = new Set();
const GRANTS = {};

function chDot(c) {
  return c.hidden ? "off" : dotCls(c);
}

/** 未保存的密钥输入草稿:重绘 #chList 前存、重建后回填(报告 P2-28)。 */
const keyDraft = {};

/** 明文「显示」的 8 秒自动隐。计时器挂在盒子自己身上 —— 渠道卡与模型弹层各有一个
    .reveal-box,两者共用一个变量时,后显示的那个会把前一个的倒计时清掉,
    于是另一处明文永远留在屏幕上(F5)。 */
const REVEAL_MS = 8000;
function revealBoxHide(box) {
  if (!box) return;
  clearTimeout(box._revealTimer);
  box._revealTimer = null;
  box.dataset.on = "0";
  box.innerHTML = "";
}
function revealBoxShow(box, key) {
  if (!box) return;
  clearTimeout(box._revealTimer);
  box.dataset.on = "1";
  box.innerHTML = `<div class="reveal" style="margin-top:4px">${esc(key)}</div>
    <div class="note">8 秒后自动隐;切页签/刷新也会收回</div>`;
  box._revealTimer = setTimeout(() => revealBoxHide(box), REVEAL_MS);
}

/** 设置页的「按需说明」(2026-10-23)。
 *
 *  背景:设置页的说明文字曾堆到 936 个汉字(静态),折合约 570px —— 超过一整个面板的高度;
 *  「密钥只进 Windows 凭据管理器」这一句在界面里出现了 10 次。问题不是"解释太多",而是
 *  **必读和可选平铺在一起**。所以分三层:
 *    L1 常驻 —— 一句后果,写在控件旁边(.desc),只在"说错会让人做错事"时出现;
 *    L2 按需 —— 本表,点「?」原地展开,同一句话全应用只写一份;
 *    L3 归档 —— 关于页的 <details>,读一次就够的长段落。
 *  判据一句话:这句话如果说错了,用户会不会做错事?会 → L1;不会 → L2/L3。
 *
 *  静态模板用 `<button class="why" data-why="key">` 引用,hydrateWhy() 负责补出说明块;
 *  动态渲染(渠道卡)也走同一套,重绘后直接再调一次 hydrateWhy 即可(幂等)。
 */
const WHY = {
  hide: "不再请求接口,也不再读取本机登录凭据;已记录的历史快照不删,点「恢复」立即回到取数。",
  custom: "地址与密钥只发往你填的那个域名;字段路径决定从返回 JSON 的哪一层取余额,换算倍数把它折成元。探活模式只验证可达,不解析金额。",
  wblogin: "点「登录」开一个独立窗口走网页登录(微信扫码/手机号),成功后窗口自动关掉,挂件用这份会话取数 —— 不复制任何东西,也不动桌面客户端的登录。会话约 7 天过期;国内版 5.6.2 起令牌加密存储,已无法自动读取。",
  dblogin: "点「登录」开一个独立窗口走豆包网页登录,成功后窗口自动关掉,挂件用这份会话查体验额度 —— 会话留在挂件自己的浏览器 profile 里,不复制、不落盘;粘贴 Cookie 是登录窗口不通时的自救路径。",
  qoderweb: "逐笔到期明细只有网页会话能拿到,桌面端凭据只有汇总(平台两套接口的差别)。显示「已登录」= 本轮已用网页会话取到逐笔明细;平台会话会过期,失效后状态会自己退回「需登录」,届时点「重新登录」重开窗口刷新即可,历史快照不丢。未登录时点「登录网页版」开独立窗口,完成后自动关窗并刷新;这套会话只读明细,签到与余额照旧走桌面端。换过账号或令牌时点「切换账号」——旧网页会话属于上一本账,不先退出就会把旧账号的已到期记录挂到新账号下。",
  appauth: "凭据来自本机已登录的客户端;失效时打开一次客户端即可。隐藏本渠道后连读取也会停止。",
  agy: "通过官方 agy statusLine 回调读取当前账号与配额，每次临时会话结束后清理进程树。成功和失败均有至少 120 秒采集冷却。账号身份保存为指纹，历史账号显示最后记录。分组隐藏后停止采集，单账号隐藏在识别当前账号后跳过更新。第三方查询仍受 Google 服务条款约束。",
  compact: "开启后可切成 380×46 的横向条;顶栏固定四枚入口不再显示「紧凑」,用 Ctrl+2 切换紧凑条,Esc 在面板与胶囊之间切换。",
  trayNum: "托盘显示缩写(74 / 1.2k / 82),悬停可看完整数值;关闭后回到品牌样式。",
  pulse: "只有红色状态点会 2 秒一次红晕。低位持续期间不重复播报,免得常驻桌面一直闪。",
  notify: "通知本身不支持点击回跳(插件限制),所以窗口藏着时改为闪一下任务栏。",
  checkinAlert: "签到成功时静默;只有凭据失效或接口结构变了才提示,且同一天同一个问题只提示一次。",
  sound: "WebAudio 合成,不新增资源文件;默认关。",
  pill: "选中的渠道每 8 秒轮播一次,鼠标停在胶囊上时放慢到 24 秒;渠道隐藏后自动从轮播里剔除。",
  checkin: "走平台内部接口查活动并签到;规则一变就自动暂停,余额显示不受影响。签到走直连不用系统代理 —— 平台按出口 IP 限流,代理 IP 更容易被拒。",
  freq: "间隔只受对方限流礼貌性约束;连续失败会自动退避,面板展开与折叠可以分别设。",
  vault: "加密存放在 Windows 凭据管理器(绑当前 Windows 账户);复制走系统剪贴板;不导出、不自动填到任何地方。",
  diag: "本机检查含快照库可写、开机自启、系统代理;报告里只有域名、状态与数值,没有任何密钥。",
  log: "令牌只记「长度 + 指纹前 8 位」。出问题时点「导出诊断包」,里面已经有环境、体检与最近事件,不用再截图。",
  used: "大数字看「还剩多少」还是「已用多少」;金额型永远显示余额。",
  floor: "算不出比例的渠道(充值型)也据此排序。",
  hotkey: "如 Alt+G;Esc 清除即不启用。",
};

/** 把 [data-why] 按钮补成「按钮 + 可展开说明」。幂等,重绘后可直接再调。
    文案缺失时把按钮一起摘掉 —— 点开一片空白比没有入口更糟。 */
let whySeq = 0;
function hydrateWhy(root) {
  (root || document).querySelectorAll("button.why[data-why]").forEach((btn) => {
    if (btn.dataset.for) return;
    const text = WHY[btn.dataset.why];
    if (!text) { btn.remove(); return; }
    const box = document.createElement("span");
    box.className = "whybox";
    box.dataset.for = btn.dataset.for = "w" + (++whySeq);
    box.innerHTML = `<span class="whybox-in">${text}</span>`;
    // 说明块落在**这一行的最后**:副标题(desc)是 L1,必须紧贴标签;
    // 按钮在横向按钮行(.ccbtns)里时插到整行之后 —— 塞进 flex 行会白占一段宽度。
    const next = btn.nextElementSibling;
    const p = btn.parentElement;
    // 蜷在按钮行里的说明块必须落在整行之后,否则会成为 flex 行的窄列(登录入口两处都这样)
    if (
      p &&
      (p.classList.contains("ccbtns") ||
        p.classList.contains("ccquick") ||
        p.classList.contains("qoder-login-bar"))
    )
      p.after(box);
    else if (next && next.classList.contains("desc")) p.appendChild(box);
    else btn.after(box);
  });
}

function toggleWhy(btn) {
  const on = btn.getAttribute("aria-expanded") === "true";
  btn.setAttribute("aria-expanded", String(!on));
  // 说明块不一定紧跟在按钮后面(见上),按 data-for 找
  const box = document.querySelector(`.whybox[data-for="${btn.dataset.for}"]`);
  if (box) box.classList.toggle("on", !on);
}

/**
 * 设置页 Qoder 网页会话入口(常驻在折叠体外面,别再藏进 .ccbody)。
 *
 * 失败时展示取数层的原因；沿用旧明细不能当成本轮会话仍然有效。
 */
function qoderQuickHtml(c) {
  if (c.id !== "qoder" && c.id !== "qoder-cn") return "";
  const notice = window.ExpiryUI.qoderNotice(c);
  const dis = QODER_LOGGING ? " disabled" : "";
  const loginBtn = QODER_LOGGING ? "登录中…" : notice.button;
  return `<div class="ccquick"><span${notice.fresh ? ' class="ccquick-ok"' : ""}>${esc(notice.text)}</span>
      <button class="btn s" data-act="qoderweblogin" data-id="${c.id}"${dis} title="${
    notice.fresh ? "平台会话会过期;重新确认并刷新逐笔明细" : "打开独立窗口登录网页账号"
  }">${loginBtn}</button>
      <button class="btn s" data-act="qoderwebswitch" data-id="${c.id}"${dis} title="先退出当前网页登录,再用另一个账号登录(换号后用这个)">切换账号</button>
      <button class="why" data-why="qoderweb" aria-expanded="false" aria-label="网页会话的说明"></button></div>`;
}

function renderChCards() {
  const chBox = $("chList");
  // 设置页真正的滚动容器是 #chList 所在的 .mbody(不是 #chList 自己)。
  // 重绘会整块换掉 #chList 的子节点:滚动值、阅读锚点、输入焦点都要先记下来。
  const scroller = chBox ? chBox.closest(".mbody") : null;
  const keepTop = scroller ? scroller.scrollTop : 0;
  let anchor = null;
  if (scroller && chBox) {
    const sr = scroller.getBoundingClientRect();
    for (const card of chBox.querySelectorAll(".ccard[data-id]")) {
      const r = card.getBoundingClientRect();
      if (r.bottom > sr.top + 4) { anchor = { id: card.dataset.id, delta: r.top - sr.top }; break; }
    }
  }
  const ae = document.activeElement;
  const focusIn = !!(ae && chBox && chBox.contains(ae));
  const focusId = focusIn && ae.id ? ae.id : null;
  const focusAct = focusIn ? ae.getAttribute("data-act") : null;
  const focusDataId = focusIn ? ae.getAttribute("data-id") || "" : "";
  const focusSel = focusIn && ae.selectionStart !== undefined && ae.selectionStart !== null
    ? { start: ae.selectionStart, end: ae.selectionEnd } : null;
  // 重绘前把已显示的密钥收回:明文不许留在 DOM 里过夜(盒子自己的计时器一起清)
  document.querySelectorAll(".reveal-box").forEach((b) => revealBoxHide(b));
  // 点别的卡上的「恢复」眼睛也会整块重建 #chList,新建输入框的 value 是空的 ——
  // 用户刚粘进去、还没点保存的那串密钥就这么没了(报告 P2-28)
  document.querySelectorAll('#chList [id^="key-"]').forEach((el) => {
    keyDraft[el.id.slice(4)] = el.value;
  });
  const ids = currentOrderIds();
  const ordered = ids.map((id) => CHANNELS.find((c) => c.id === id)).filter(Boolean);
  const list = window.AntigravityUI ? window.AntigravityUI.managementList(ordered) : ordered;
  const map = (CFG && CFG.claimChannels) || {};
  // 顶部恢复条:从**配置**里算,不依赖渠道视图 —— 万一某条卡片被渲染分支吞掉,
  // 这里仍然给得出恢复入口(兜底);点一下全部恢复。
  const hidIds = ((CFG && CFG.hiddenChannels) || []).filter((x) => CHANNELS.some((c) => c.id === x));
  const hidNames = hidIds.map((x) => (CHANNELS.find((c) => c.id === x) || {}).name || x);
  const hiddenHint = hidIds.some(id => id !== "antigravity" && isAntigravity({ id }))
    ? "Antigravity 账号卡冻结记录并保留会话识别，其余隐藏渠道停止取数" : "隐藏期间不取数,历史快照保留";
  const restoreBar = hidIds.length
    ? `<div class="restorebar" data-id="__all"><span><b>已隐藏 ${hidIds.length} 个</b>(${esc(
        hidNames.join("、")
      )})—— ${hiddenHint}</span><span class="btn s" data-act="chshowall">全部恢复</span></div>`
    : "";
  $("chList").innerHTML = restoreBar + list
    .map((c, i) => {
      const isApp = c.authSource === "app";
      // web = 挂件内网页登录(WorkBuddy):桌面客户端把令牌加密了,只能登录网页账号拿
      const isWeb = c.authSource === "web";
      const cc = map[c.id];
      const on = !!(cc && cc.enabled);
      let st;
      if (c.hidden)
        st = c.autoHidden
          ? // 系统藏的,不是用户藏的 —— 必须说清区别,否则用户会去设置里找"我什么时候隐藏的"
            "未检测到登录 · 已自动隐藏(登录后自动出现)"
          : "已隐藏 · 不再取数(历史快照保留)";
      else if (!c.hasKey) st = isApp ? "未检测到登录" : isWeb ? "未登录" : "未配置";
      else if (c.valid)
        st = isApp
          ? `已读取本机登录 · ${esc(c.authLabel)}`
          : isWeb
          ? `已登录 · ${esc(c.authLabel)}<span class="kh" data-hint="${c.id}"></span>`
          : `已配置<span class="kh" data-hint="${c.id}"></span>`;
      else st = (isApp ? "已读取本机登录" : isWeb ? "已登录" : "已配置") + " · 取数失败" + (isWeb ? "(令牌可能已过期)" : "");

      const keyBlock = c.id === "google-cloud" && window.GoogleCloudUI
        ? window.GoogleCloudUI.controls(c) + window.GoogleCloudUI.records(c)
        : isWeb
        ? `<div class="ccbtns">
            ${
          c.id === "doubao"
            ? `<button class="btn p" data-act="dblogin" data-id="${c.id}"${DB_LOGGING ? " disabled" : ""}>${
                c.hasKey ? "重新登录豆包账号" : "登录豆包账号"
              }</button>
            <button class="why" data-why="dblogin" aria-expanded="false" aria-label="登录方式的说明"></button>`
            : `<button class="btn p" data-act="wblogin" data-id="${c.id}"${WB_LOGGING ? " disabled" : ""}>${
                c.hasKey ? "重新登录 WorkBuddy 账号" : "登录 WorkBuddy 账号"
              }</button>
            <button class="why" data-why="wblogin" aria-expanded="false" aria-label="登录方式的说明"></button>`
        }
          </div>
          <div class="ccbtns">
            <input type="password" id="key-${c.id}" autocomplete="off" spellcheck="false"
              value="${esc(keyDraft[c.id] || '')}"
              placeholder="${c.hasKey ? "也可粘贴访问密钥 · 留空则不修改" : "或粘贴访问密钥(登录窗口不通时自救)"}">
            <button class="btn" data-act="savekey" data-id="${c.id}">保存</button>
            ${c.hasKey ? `<button class="btn danger" data-act="delkey" data-id="${c.id}">清除登录</button>` : ""}
          </div>`
        : c.id === "antigravity" && window.AntigravityUI
        ? window.AntigravityUI.controls(c)
        : isApp
        ? `<div class="mini">${
            // 国际版这条链路**会续期**(refreshToken 换 accessToken,成果存在我们自己的库里、
            // 不写回客户端文件),照抄别的 app 型渠道写"不刷新"就是错的(设计稿 §6 记过这一条)
            c.id === "workbuddy-ai" ? "只读复用本机登录,自动续期" : "只读复用本机登录,不写回、不刷新"
          }<button class="why" data-why="appauth" aria-expanded="false" aria-label="凭据来源的说明"></button></div>`
        : `<div class="ksrc" data-src="${c.id}">${srcRowHtml(c.id)}</div>
          <div class="ccbtns">
            <input type="password" id="key-${c.id}" autocomplete="off" spellcheck="false"
              value="${esc(keyDraft[c.id] || '')}"
              placeholder="${c.hasKey ? "已保存 · 留空则不修改" : "粘贴 API Key"}">
            <button class="btn p" data-act="savekey" data-id="${c.id}">保存</button>
            ${c.hasKey ? `<button class="btn danger" data-act="delkey" data-id="${c.id}">删除</button>` : ""}
          </div>`;

      const claimBlock =
        c.kind === "amount" && c.id !== "google-cloud"
          ? `<div class="claim-in">
              <div class="field"><span>额度申请制<span class="desc">余额补到固定上限;申请时间自动检测</span></span>
                <button class="sw${on ? " on" : ""}" role="switch" aria-checked="${on}" aria-label="额度申请制" data-act="claimtoggle" data-id="${c.id}"></button></div>
              ${on
                ? `<div class="claim-grid">
                    <label>单次额度<input type="number" min="1" step="10" value="${esc(cc.amount)}" data-act="claimamount" data-id="${c.id}">元</label>
                    <label>最短间隔<input type="number" min="1" step="1" value="${esc(cc.minIntervalDays)}" data-act="claimdays" data-id="${c.id}">天</label>
                    <label>上次申请<input type="date" value="${toDateInput(c.claim && c.claim.lastClaimAt)}" data-act="claimdate" data-id="${c.id}"></label>
                    <div class="ccbtns">
                      <button class="btn" data-act="claimnow" data-id="${c.id}">记一次申请=今天</button>
                      <button class="btn" data-act="claimclear" data-id="${c.id}">清除手动值</button>
                    </div>
                  </div>`
                : ""}
            </div>`
          : "";

      // 积分获取(只有记到账的渠道):今日一句 + 近 7 天汇总,「记录」展开 30 天明细。
      // 记录是**懒加载**的 —— 每轮轮询都查一次 30 天明细纯属浪费。
      const grantBlock = c.grantToday
        ? `<div class="krow"><span class="k1" style="flex:1">积分获取
             <span class="desc">${esc(grantLine(c))}${
            c.grantWeek && c.grantWeek.days ? ` · 近 7 天 ${c.grantWeek.days} 天有获取` : ""
          }</span></span>
             <button class="btn s" data-act="grantlog" data-id="${c.id}">${
            grantOpen.has(c.id) ? "收起" : "记录"
          }</button></div>
           ${grantOpen.has(c.id) ? grantListHtml(c.id) : ""}`
        : "";

      return `<div class="ccard${c.hidden ? " off" : ""}${cardOpen.has(c.id) ? " open" : ""}" data-id="${c.id}">
        <div class="cchead" data-act="chfold" data-id="${c.id}">
          ${iconHtml(c, "ico", !c.hasKey || c.hidden)}
          <div class="ccmeta">
            <div class="ccname">${esc(c.name)}<span class="dot ${chDot(c)}"></span>${c.hidden ? '<span class="tagn">已隐藏</span>' : ""}</div>
            <div class="ccst">${st}</div>
          </div>
          <button class="eye${c.hidden ? " off" : ""}" data-act="chvis" data-id="${c.id}"
            title="${
              c.hidden
                ? c.autoHidden
                  ? "仍然显示(当前没检测到登录,登录后会自动恢复取数)"
                  : "恢复显示(重新开始取数)"
                : "隐藏(停止取数,历史快照保留)"
            }">${c.hidden ? (c.autoHidden ? "显示" : "恢复") : "👁"}</button>
          <span class="ccaret">&#9654;</span>
        </div>
        ${
          // 逐包到期明细的入口**必须放在折叠体外面**(2026-11-08 用户反馈):
          // .ccbody 默认 display:none,藏进去等于让用户翻两层折叠。
          // 国际版/国内版共用同一个入口,Cookie 与会话键在 Rust 侧按渠道隔离。
          qoderQuickHtml(c)
        }
        <div class="ccbody">
          ${keyBlock}
          ${
        c.hasKey && c.authSource === "keyring"
          ? `<div class="krow"><span class="k1">密钥 · <span class="khint" data-hint="${c.id}">••••</span></span>
               <button class="btn s" data-act="keycopy" data-id="${c.id}" title="复制整把密钥(明文不回界面)">复制</button>
               <button class="btn s" data-act="keyreveal" data-id="${c.id}" title="显示明文,8 秒后自动隐">显示</button></div>
             <div class="reveal-box" data-rev="${c.id}"></div>`
          : ""
      }
      ${claimBlock}
      ${grantBlock}
      ${
        c.hasKey && !c.hidden && c.id !== "google-cloud"
          ? `<div class="krow"><span class="k1" style="flex:1">连接自检<span class="desc">真打一次接口,不落快照</span></span>
               <button class="btn s" data-act="probe" data-id="${c.id}">测试连接</button></div>
             <div class="prb" data-prb="${c.id}"></div>`
          : ""
      }
      ${
        String(c.id).startsWith("custom-")
          ? `<div class="krow"><span class="k1" style="flex:1">自定义渠道<span class="desc">删除只清定义与密钥,历史快照保留</span></span>
               <button class="btn s danger" data-act="cdel" data-id="${c.id}">删除</button></div>`
          : ""
      }
      ${CHECKIN_ON.has(c.id) ? '<div class="ckwrap" data-id="' + esc(c.id) + '">' + ckRowHtml(c.id) + "</div>" : ""}
          ${c.id === "antigravity" && window.AntigravityUI ? "" : `<div class="ccord"><span class="mini">在列表中的位置</span>
            <span class="ord">
              <button class="btn ord" data-act="ordermove" data-id="${c.id}" data-dir="-1"${i === 0 ? " disabled" : ""} title="上移">&#9650;</button>
              <button class="btn ord" data-act="ordermove" data-id="${c.id}" data-dir="1"${i === list.length - 1 ? " disabled" : ""} title="下移">&#9660;</button>
            </span>
          </div>`}
        </div>
      </div>`;
    })
    .join("");
  // 卡片里的「?」是动态生成的,补出说明块(幂等;轮询重绘后也不会丢)
  hydrateWhy($("chList"));
  // 重绘把 .kh / .ksrc 清空了:有缓存就同步贴回去(不产生 RPC)
  paintCachedKeyMeta();
  // 滚动位置 + 可见渠道锚点:卡片展开/收起导致高度变化时,仍停在用户正在读的那张卡
  if (scroller) {
    scroller.scrollTop = keepTop;
    if (anchor) {
      const sr2 = scroller.getBoundingClientRect();
      for (const card of chBox.querySelectorAll(".ccard[data-id]")) {
        if (card.dataset.id !== anchor.id) continue;
        scroller.scrollTop += card.getBoundingClientRect().top - sr2.top - anchor.delta;
        break;
      }
    }
  }
  // 焦点还给同一个控件(输入值由 keyDraft 回填),光标位置一并带上。
  // 没有 id 的按钮用 data-act + data-id 找回来 —— 键盘用户按完按钮后轮询重绘不该丢焦点。
  if (focusIn) {
    let el = focusId ? document.getElementById(focusId) : null;
    if ((!el || !chBox.contains(el)) && focusAct) {
      el = Array.from(chBox.querySelectorAll("[data-act]")).find(
        (x) => x.getAttribute("data-act") === focusAct &&
          (x.getAttribute("data-id") || "") === focusDataId
      ) || null;
    }
    if (el && chBox.contains(el)) {
      el.focus({ preventScroll: true });
      if (focusSel && el.setSelectionRange) {
        try { el.setSelectionRange(focusSel.start, focusSel.end); } catch (_) {}
      }
    }
  }
}

/** 轮询刷新时更新渠道卡;正在输入就不重建,免得把输入内容冲掉。 */function refreshChCards() {
  if (!manageOpen()) return;
  const ae = document.activeElement;
  if (ae && ae.closest && ae.closest("#chList") && (ae.tagName === "INPUT" || ae.tagName === "SELECT")) return;
  renderChCards();
  fillKeyHints();
}

/**
 * 补每把 key 的尾号:重绘后先用缓存**同步**回填,缺失/过期才异步读一次。
 * 失败仍把"凭据库读取失败"写出来 —— 凭据管理器坏了 ≠ 没配密钥(报告 P2-15)。
 */
function fillKeyHints(force = false) {
  paintCachedKeyMeta();
  refreshKeyMeta(force);
}

// ───────────── 渠道密钥的来源(直接填 / 引用密钥箱)─────────────
// 设计见 docs/product/product-vault-channel-bridge-2026-10-20.md(裁决 1–7 已批)。
// 主线是**引用**:同一把钥匙不存两份,改箱子渠道立刻跟着变。
const KEYSRC = {};        // 渠道 id → key_source 的结果
let VAULTREF = [];        // [[渠道 id, 箱子条目 id], ...] 反查用

// 密钥元数据缓存(P2)。打开设置页时,以前每一轮轮询都会读:
//   key_hint × N + key_source × N + 一次引用表 —— 余额刷新顺带读了一堆
// 与余额无关的秘密元数据。现在重绘时**同步**用缓存回填,只有缺失或超过 TTL
// 才异步读一次;保存/删除/引用变化/openManage 时显式失效。
const KEY_META = {};          // 渠道 id → { hint, hintErr, hintAt, source, sourceAt }
const KEY_META_TTL = 5 * 60 * 1000;
let KEY_META_BUSY = null;
let KEY_META_PENDING = false;

function metaFresh(at) {
  return !!at && Date.now() - at < KEY_META_TTL;
}

/** 同步把缓存贴回 DOM:重绘后立刻恢复尾号/来源,不产生任何 RPC。 */
function paintCachedKeyMeta() {
  document.querySelectorAll("[data-hint]").forEach((el) => {
    const m = KEY_META[el.dataset.hint];
    if (!m || !m.hintAt) return;
    if (m.hintErr) {
      el.textContent = " · 凭据库读取失败:" + m.hintErr;
      el.classList.add("err");
    } else {
      el.classList.remove("err");
      el.textContent = m.hint ? " · " + m.hint : "";
    }
  });
  document.querySelectorAll(".ksrc[data-src]").forEach((box) => {
    const id = box.dataset.src;
    const m = KEY_META[id];
    if (!m || !m.sourceAt || !m.source) return;
    KEYSRC[id] = m.source;
    box.innerHTML = srcRowHtml(id);
  });
}

/** 显式失效。`kind` = both | hint | source;`id = null` 表示全部渠道。 */
function invalidateKeyMeta(id = null, kind = "both") {
  const drop = (cid) => {
    const m = KEY_META[cid];
    if (!m) return;
    if (kind !== "source") {
      m.hint = null;
      m.hintErr = null;
      m.hintAt = 0;
    }
    if (kind !== "hint") {
      m.source = null;
      m.sourceAt = 0;
      delete KEYSRC[cid]; // 重绘时先回到占位,别再读旧来源
    }
  };
  if (id) drop(id);
  else Object.keys(KEY_META).forEach(drop);
}

async function doRefreshKeyMeta(force) {
  const wantedHints = CHANNELS.filter(
    (c) => c.hasKey && c.id !== "google-cloud" && (force || !metaFresh((KEY_META[c.id] || {}).hintAt))
  );
  const wantedSources = CHANNELS.filter(
    (c) => c.authSource === "keyring" && (force || !metaFresh((KEY_META[c.id] || {}).sourceAt))
  );
  if (!wantedHints.length && !wantedSources.length) return;

  // 「或从密钥箱选一把」要用到箱子列表;用户可能没打开过密钥箱页签,这里按需补一次。
  if (wantedSources.length) {
    if (!VAULT.length) {
      try { VAULT = await invoke("vault_list"); } catch { /* 读不出来就不显示下拉 */ }
    }
    await loadVaultRefs();
  }

  const now = Date.now();
  await Promise.all([
    ...wantedHints.map(async (c) => {
      const m = KEY_META[c.id] || (KEY_META[c.id] = {});
      try {
        m.hint = await invoke("key_hint", { id: c.id });
        m.hintErr = null;
      } catch (e) {
        // 凭据管理器坏了 ≠ 没配密钥:把故障如实写出来,否则用户会反复重填
        m.hint = null;
        m.hintErr = String(e);
      }
      m.hintAt = now;
    }),
    ...wantedSources.map(async (c) => {
      const m = KEY_META[c.id] || (KEY_META[c.id] = {});
      try {
        m.source = await invoke("key_source", { id: c.id });
      } catch (e) {
        // 读不到来源时退回"直接填"的样子 —— 不显示未经验证的引用状态
        m.source = { mode: "inline" };
      }
      m.sourceAt = now;
    }),
  ]);
  paintCachedKeyMeta();
}

/** 读一轮元数据(同一时刻只有一个在飞;期间收到 force 会在结束后补跑)。 */
function refreshKeyMeta(force = false) {
  if (!manageOpen()) return Promise.resolve();
  if (KEY_META_BUSY) {
    KEY_META_PENDING = KEY_META_PENDING || force;
    return KEY_META_BUSY;
  }
  KEY_META_BUSY = doRefreshKeyMeta(force).finally(() => {
    KEY_META_BUSY = null;
    if (KEY_META_PENDING) {
      KEY_META_PENDING = false;
      refreshKeyMeta(true);
    }
  });
  return KEY_META_BUSY;
}

/** 一次问出全部引用关系(箱子列表要标"正被谁用")。 */
async function loadVaultRefs() {
  try { VAULTREF = await invoke("vault_ref_map"); } catch { VAULTREF = []; }
}

/**
 * 哪些渠道正引用这条箱子密钥 —— **只用于列表显示**。
 *
 * 读的是索引(VAULTREF),不是凭据管理器里的真实指针。这个分工是刻意的:
 *   · **显示**用索引:一行一次 SQL,列表几十条也不卡;而且索引在启动时与每次删除后
 *     都会按真实指针重抄一遍(product-vault-ref-reconcile),所以它现在是可信的。
 *   · **删除决策**用真实指针:那是破坏性操作,一律由后端 vault_delete 扫指针判定,
 *     不信索引 —— 索引读坏了也不该让人误删。
 * 换句话说:索引可以拿来**看**,不能拿来**决定删不删**。
 */
function refChannelsOf(vaultId) {
  return VAULTREF.filter(([, v]) => v === vaultId).map(([c]) => c);
}

/** 渠道 id → 中文名(反查结果显示用)。 */
function channelName(id) {
  const c = CHANNELS.find((x) => x.id === id);
  return c ? c.name : id;
}

/**
 * 渠道卡上的「密钥来源」一行。
 *
 * 三种情形必须说清区别(裁决 4):
 *   · 引用正常 → 显示引用的条目 + 尾号,给「改回手填」
 *   · 断链     → 说"引用的条目已删除",**不能**含糊成"未配置"(该去箱子看,不是去重填)
 *   · 直接填   → 什么都不用额外说,输入框本身就是来源
 */
function srcRowHtml(cid) {
  const s = KEYSRC[cid];
  if (!s || s.mode === "inline" || s.mode === "none") return vaultPickHtml(cid);
  if (s.mode === "dangling") {
    return `<div class="krow warn"><span class="k1" style="flex:1">引用的密钥箱条目已删除
      <span class="desc">去密钥箱看看,或改回在这里直接填</span></span>
      <button class="btn s" data-act="unvault" data-id="${esc(cid)}">改回手填</button></div>`;
  }
  // vault:把"用的是哪一条"直接写出来,别让用户猜
  const which = s.vaultPlatform || s.vaultLabel
    ? `${esc(s.vaultPlatform || "")}${s.vaultLabel ? " / " + esc(s.vaultLabel) : ""}`
    : "箱子条目";
  return `<div class="krow"><span class="k1" style="flex:1">密钥来源
    <span class="desc">引用密钥箱 · ${which}${s.hint ? " · " + esc(s.hint) : ""}</span></span>
    <button class="btn s" data-act="unvault" data-id="${esc(cid)}">改回手填</button></div>`;
}

/** "或从密钥箱选一把"——把箱子里的条目平铺成下拉(按平台分组)。 */
function vaultPickHtml(cid) {
  if (!VAULT.length) return "";
  const opts = VAULT.map(
    (it) => `<option value="${esc(it.id)}">${esc(it.platform)} / ${esc(it.label)} · ${esc(it.hint)}</option>`
  ).join("");
  return `<div class="ccbtns">
      <select id="vref-${cid}" class="kref">${opts}</select>
      <button class="btn" data-act="usevault" data-id="${cid}" title="让这个渠道改用箱子里那把(会清掉这里手填的值)">改用这把</button>
    </div>`;
}

/** 补每张卡上的来源一行:缓存同步回填,缺失/过期才异步读(失败仍按原提示回退)。 */
async function fillKeySources(force = false) {
  paintCachedKeyMeta();
  await refreshKeyMeta(force);
}


/** 管理页里的胶囊渠道选择:点一下加入/移出轮播列表。 */
function renderPillPick() {
  const selection = (CFG && CFG.pillChannels) || [];
  const sel = window.AntigravityUI ? window.AntigravityUI.expandSelection(selection) : selection;
  qReconcile($("cfgPill"), CHANNELS.filter((c) => !c.hidden).map(
    (c) =>
      ({ key: c.id, html: `<button class="pick${sel.includes(c.id) ? " on" : ""}" data-act="pillpick" data-id="${c.id}">${esc(c.name)}</button>` })
  ));
}

