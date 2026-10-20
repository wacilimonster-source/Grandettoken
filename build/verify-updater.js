// 只回答一个问题:**已装用户真的能收到这一版吗?**
//
// 更新插件(tauri-plugin-updater)的判定链条只有两步:
//   ① 从更新端点拿 latest.json,读 platforms['windows-x86_64'].signature
//   ② 下载 release 上那个 .sig,与 ① 逐字比对
// 相同 → 认可这个安装包;不同 → 拒绝更新。
//
// 所以这里只核这两样 + 顺带确认安装包真的下得着。
//
// **为什么下载走 PowerShell 而不是 node 的 fetch**:这台机器的网络必须走
// 127.0.0.1:7897 的系统代理(注册表 ProxyEnable=1)。node 24 的原生 fetch
// 既不读系统代理、也不认 HTTPS_PROXY 环境变量,直连 github.com 十秒超时 ——
// 脚本会随机地"有时能跑有时不能跑",那种核验比没有更糟。
// Invoke-WebRequest 自动读系统代理,稳定。
// (CI runner 上没有这个约束,但脚本要本机也能原样跑。)
//
// 用法:node build/verify-updater.js
const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFileSync } = require('child_process');

const root = path.join(__dirname, '..');
const conf = JSON.parse(fs.readFileSync(path.join(root, 'app', 'src-tauri', 'tauri.conf.json'), 'utf8'));
const V = conf.version;
const TAG = `v${V}`;
const EXE = `Grandettoken_${V}_x64-setup.exe`;
const BASE = `https://github.com/wacilimonster-source/Grandettoken/releases/download/${TAG}`;
const ENDPOINT = 'https://github.com/wacilimonster-source/Grandettoken/releases/latest/download/latest.json';

const tmp = path.join(os.tmpdir(), 'gt-verify');
fs.mkdirSync(tmp, { recursive: true });

let failed = false;
const ok = (n, d = '') => { console.log(`PASS  ${n}${d ? '  ' + d : ''}`); };
const bad = (n, d = '') => { failed = true; console.log(`FAIL  ${n}${d ? '  ' + d : ''}`); };

// 用 PowerShell 下载(走系统代理),返回本地路径
function download(url, outName) {
  const out = path.join(tmp, outName);
  const ps = `
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -Uri '${url}' -OutFile '${out}' -UseBasicParsing -TimeoutSec 180
  `.trim();
  execFileSync('powershell', ['-NoProfile', '-Command', ps], { stdio: 'pipe' });
  if (!fs.existsSync(out) || fs.statSync(out).size === 0) throw new Error(`download produced nothing: ${url}`);
  return out;
}

// 更新端点的内容。用 -OutFile 落盘再读,而不是把 .Content 接到 stdout ——
// PowerShell 会把多行字符串拆成数组再逐行输出,直接 JSON.parse 会撞上
// "Unexpected non-whitespace character after JSON"(实测踩过)。
function getText(url) {
  const out = path.join(tmp, 'endpoint.json');
  const ps = `
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -Uri '${url}' -OutFile '${out}' -UseBasicParsing -TimeoutSec 120
  `.trim();
  execFileSync('powershell', ['-NoProfile', '-Command', ps], { stdio: 'pipe' });
  if (!fs.existsSync(out)) throw new Error('endpoint download produced nothing');
  return fs.readFileSync(out, 'utf8');
}

(async () => {
  // 1. 更新端点拿得到,且是本 tag 的清单
  let live;
  try {
    const raw = getText(ENDPOINT);
    // Invoke-WebRequest 的 .Content 按字符串读,中文不会坏(它按 charset 解码);
    // 但 BOM / 前后空白要清掉,否则 JSON.parse 报错。
    live = JSON.parse(raw.replace(/^\uFEFF/, '').trim());
    ok('update endpoint reachable', `latest.json ${raw.length} chars`);
  } catch (e) {
    bad('update endpoint reachable', e.message);
    return report();
  }
  if (live.version === V) ok('latest.json is this version', `= ${V}`);
  else { bad('latest.json is this version', `${live.version} != ${V}`); return report(); }

  // 2. 更新插件真正比的两份
  const plat = (live.platforms || {})['windows-x86_64'];
  if (!plat || !plat.signature) { bad('manifest carries platforms.windows-x86_64.signature'); return report(); }
  ok('manifest carries the signature field');

  const sigPath = download(`${BASE}/${EXE}.sig`, `${EXE}.sig`);
  const fileSig = fs.readFileSync(sigPath, 'utf8').trim();
  const manSig = String(plat.signature).trim();
  if (manSig === fileSig && manSig.length > 0) {
    ok('signature: latest.json === release .sig', `byte-identical, ${manSig.length} chars`);
  } else {
    bad('signature: latest.json === release .sig', `manifest=${manSig.length} file=${fileSig.length} equal=${manSig === fileSig}`);
    return report();
  }

  // 3. 清单里的 url 指向本 tag,且装得下来(用户真会去下这个文件)
  const wantUrl = `${BASE}/${EXE}`;
  if (plat.url === wantUrl) ok('manifest url points at this tag', plat.url);
  else bad('manifest url points at this tag', `${plat.url} != ${wantUrl}`);

  const exePath = download(wantUrl, EXE);
  const size = fs.statSync(exePath).size;
  ok('installer downloadable', `${size} bytes`);

  report();

  function report() {
    console.log(failed ? `RESULT: FAIL  (updater chain broken for ${TAG})` : `RESULT: PASS  (installed users can receive ${TAG})`);
    process.exit(failed ? 1 : 0);
  }
})().catch((e) => {
  console.error(`RESULT: FAIL  ${e.message || e}`);
  process.exit(1);
});
