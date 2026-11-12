/**
 * 形态门守卫:防止"默认藏起来的整块容器"在别的形态里被 display 出来。
 *
 * 为什么需要它:v0.1.56 的吸附竖条写成
 *     .dock-body{display:flex!important;...}
 * 这一条**没有挂在 body.docked 下**,于是盖掉了前面的 `.dock-body{display:none}` ——
 * 面板形态也显示竖条,而它 height:100%,在 flex 列里抢走 415px,
 * 列表(flex:1 1 0%)被压成 0 高,整页只剩标题栏、风险带和底栏。
 * 真机截图一眼就能看见,但 jsdom 冒烟测不出(CSS 不参与布局),所以单独立一道闸。
 *
 * 规则(只查 body 的直接子元素,避免 .row .r2 这类"父级状态门"误报):
 *   1. 找出"有 display:none 基础规则"的整块容器(吸附条 / 胶囊 / 紧凑条控件…);
 *   2. 之后再给这些容器写 display(非 none)的规则,**必须**带 body.<状态> 前缀。
 *
 * 用法:
 *   node build/check-css-forms.js              # 判定 app/src/styles.css
 *   node build/check-css-forms.js --self-test  # 证明自己会对坏样本变红
 *
 * 自检是 2026-11-12 补的:这道门此前只能用"当前文件恰好绿"证明自己,
 * 而"当前文件恰好绿"同样可以由"门禁什么都没查"造成(见 AGENTS.md 6.6)。
 */
const fs = require("fs");
const path = require("path");

const SRC = path.join(__dirname, "..", "app", "src");

/** body 的直接子元素(整块容器才可能"多占一整行") */
function collectShells(html) {
  const shells = new Set();
  const bodyStart = html.indexOf("<body");
  const bodyEnd = html.indexOf("</body>");
  if (bodyStart < 0 || bodyEnd < 0) return shells;
  const bodyHtml = html.slice(bodyStart, bodyEnd);
  let depth = 0;
  // 逐标签走一遍,只在 depth===1 时登记:body 自身的子元素
  const re = /<(\/?)([a-zA-Z][\w-]*)([^>]*?)(\/?)>/g;
  let m;
  while ((m = re.exec(bodyHtml))) {
    const [, close, tag, attrs, selfClose] = m;
    if (close) { depth = Math.max(0, depth - 1); continue; }
    if (depth === 1) {
      const id = /\bid="([^"]+)"/.exec(attrs);
      const cls = /\bclass="([^"]+)"/.exec(attrs);
      if (id) shells.add("#" + id[1]);
      if (cls) cls[1].trim().split(/\s+/).forEach((c) => shells.add("." + c));
    }
    const void_ = /^(br|hr|img|input|meta|link|source)$/i.test(tag);
    if (!selfClose && !void_) depth++;
  }
  return shells;
}

/** 拆规则(去注释、摊平 @media) */
function parseRules(css) {
  const rules = [];
  const clean = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const stack = [];
  let buf = "";
  for (const ch of clean) {
    if (ch === "{") { stack.push(buf.trim()); buf = ""; }
    else if (ch === "}") { const sel = stack.pop(); if (sel && !sel.startsWith("@")) rules.push({ sel, decls: buf }); buf = ""; }
    else buf += ch;
  }
  return rules;
}

const declOf = (decls, prop) => {
  for (const d of decls.split(";")) {
    const i = d.indexOf(":");
    if (i < 0) continue;
    if (d.slice(0, i).trim().toLowerCase() === prop) return d.slice(i + 1).trim();
  }
  return null;
};

/** 选择器列表 → 每条的"最后一段"(组合子之后的那个复合选择器) */
const lastCompounds = (sel) =>
  sel.split(",").map((s) => s.trim().split(/[\s>+~]+/).filter(Boolean).pop() || "");

function analyze(css, html) {
  const shells = collectShells(html);
  const rules = parseRules(css);

  // ── 第一遍:谁有 display:none 的"裸"基础规则 ──
  // 只看**选择器就是这个容器本身**的规则(不带 body 前缀):
  // `.tbar{display:flex}` 这类基础可见、靠 body.form-compact 才藏起来的容器不算受管,
  // 否则守卫会把"正常显示"当成违规。
  const gated = new Set();
  for (const r of rules) {
    const d = declOf(r.decls, "display");
    if (!d || !/^none$/i.test(d.replace(/!important/i, "").trim())) continue;
    for (const c of lastCompounds(r.sel)) if (shells.has(c) && r.sel.trim() === c) gated.add(c);
  }

  // ── 第二遍:这些容器的其它 display 规则必须带 body.<状态> ──
  const bad = [];
  for (const r of rules) {
    const d = declOf(r.decls, "display");
    if (!d) continue;
    if (/^none$/i.test(d.replace(/!important/i, "").trim())) continue;
    for (const c of lastCompounds(r.sel)) {
      if (!gated.has(c)) continue;
      if (!/(^|[\s>+~])body\./.test(r.sel)) bad.push({ sel: r.sel.trim(), display: d, target: c });
    }
  }
  return { gated, rules, bad };
}

function selfTest() {
  const htmlOk = `<html><body><div class="probe"><span>inner</span></div></body></html>`;
  const cases = [
    {
      name: "无状态门的 display 必须判红",
      css: `.probe{display:none}\n.probe{display:flex}`,
      wantBad: true,
    },
    {
      name: "body.<状态> 的 display 判绿",
      css: `.probe{display:none}\nbody.docked .probe{display:flex}`,
      wantBad: false,
    },
    {
      name: "子元素不算整块容器(不误报)",
      css: `.probe{display:none}\n.probe span{display:flex}`,
      wantBad: false,
    },
    {
      name: "!important 不能绕过状态门",
      css: `.probe{display:none}\n.probe{display:flex!important}`,
      wantBad: true,
    },
  ];
  let fail = 0;
  for (const c of cases) {
    const got = analyze(c.css, htmlOk).bad.length > 0;
    if (got !== c.wantBad) {
      console.error(`  ✗ 自检失败:${c.name}(期望${c.wantBad ? "红" : "绿"},实际${got ? "红" : "绿"})`);
      fail++;
    } else {
      console.log(`  ✓ ${c.name}`);
    }
  }
  return fail === 0;
}

function main() {
  if (process.argv.includes("--self-test")) {
    console.log("形态门守卫 自检:");
    process.exit(selfTest() ? 0 : 1);
  }

  const css = fs.readFileSync(path.join(SRC, "styles.css"), "utf8");
  const html = fs.readFileSync(path.join(SRC, "index.html"), "utf8");
  const { gated, rules, bad } = analyze(css, html);
  console.log(`形态门守卫:${gated.size} 个整块容器受管 (${[...gated].join(" ")}),规则 ${rules.length} 条`);
  if (bad.length) {
    for (const b of bad) {
      console.log(`  FAIL ${b.target} 被无条件显示: ${b.sel} { display:${b.display} }`);
      console.log(`       → 必须写成 body.<状态> ${b.target}{...},否则会在别的形态里抢走布局空间`);
    }
    console.log(`\n${bad.length} 处 FAIL`);
    process.exit(1);
  }
  console.log("  PASS 所有整块容器的显示都挂在 body 状态门之下");
}

main();
