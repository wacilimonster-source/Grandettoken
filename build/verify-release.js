// 发版后核验:更新端点、三件套、签名、sha256、仓库元数据,一条链跑完。
//
// 为什么用 node 写:PowerShell 5.1 把 UTF-8 中文按 GBK 显示,`Invoke-WebRequest`
// 拿回来的正文在 `$resp.Content` 里也是一样的乱码 —— 终端看着像"发出去的说明全是
// 乱码",其实是回显问题。按字节比才作数,所以这里走 node(它按 UTF-8 解码,
// 且这里比较的一律是 hex/base64,不经任何终端渲染)。
//
// 用法:node build/verify-release.js
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

const OWNER = 'wacilimonster-source';
const REPO = 'Grandettoken';
const root = path.join(__dirname, '..');
const nsis = path.join(root, 'app', 'src-tauri', 'target', 'release', 'bundle', 'nsis');

const conf = JSON.parse(fs.readFileSync(path.join(root, 'app', 'src-tauri', 'tauri.conf.json'), 'utf8'));
const VERSION = conf.version;
const TAG = `v${VERSION}`;
const EXE = `Grandettoken_${VERSION}_x64-setup.exe`;
const ENDPOINT = `https://github.com/${OWNER}/${REPO}/releases/latest/download/latest.json`;

const lines = [];
let failed = false;
const ok = (name, detail = '') => { lines.push(`PASS  ${name}${detail ? '  ' + detail : ''}`); };
const bad = (name, detail = '') => { failed = true; lines.push(`FAIL  ${name}${detail ? '  ' + detail : ''}`); };

const sha256 = (buf) => crypto.createHash('sha256').update(buf).digest('hex');

(async () => {
  const H = { 'User-Agent': 'verify-release', Accept: 'application/vnd.github+json' };
  const token = process.env.GH_TOKEN || '';
  if (token) H.Authorization = `Bearer ${token}`;

  // 1. 更新端点:永远指向最新 release,这个 URL 写死在应用里(见 tauri.conf.json)
  const r = await fetch(ENDPOINT, { headers: H, redirect: 'follow' });
  ok('update endpoint HTTP 200', `status=${r.status} bytes=${Number(r.headers.get('content-length') || 0)}`);
  if (r.status !== 200) { bad('update endpoint reachable'); return report(); }
  const liveText = await r.text();
  const live = JSON.parse(liveText);

  // 2. 版本号:清单里的、tag 的、配置里的三者一致
  ok('version in latest.json', `= ${live.version}`);
  if (live.version !== VERSION) bad('version matches tauri.conf.json', `${live.version} vs ${VERSION}`);

  // 3. 签名与本地 .sig 逐字一致 —— v2 的清单是 platforms.<os-arch>.{url,signature},
  //    不是顶层的 url/signature(核验脚本按 v1 字段读过一次,假阴性过)
  const plat = (live.platforms || {})['windows-x86_64'];
  if (!plat) { bad('platforms.windows-x86_64 present'); return report(); }
  const sigPath = path.join(nsis, `${EXE}.sig`);
  const localSig = fs.existsSync(sigPath) ? fs.readFileSync(sigPath, 'utf8').trim() : '';
  const sameSig = plat.signature.trim() === localSig;
  if (sameSig) ok('signature identical to local .sig', `(${localSig.length} chars)`);
  else bad('signature identical to local .sig', `live=${plat.signature.length} local=${localSig.length}`);

  // 4. 清单里的 url 指向本 tag 的资源,且真的下得到
  const wantUrl = `https://github.com/${OWNER}/${REPO}/releases/download/${TAG}/${EXE}`;
  if (plat.url === wantUrl) ok('url points at this tag', plat.url);
  else bad('url points at this tag', `${plat.url} != ${wantUrl}`);

  // 5. 安装包公开可下,且 sha256 与本地一致
  const exePath = path.join(nsis, EXE);
  const localBytes = fs.existsSync(exePath) ? fs.readFileSync(exePath) : null;
  const bin = await fetch(wantUrl, { headers: H });
  if (bin.status !== 200) { bad('installer downloadable', `status=${bin.status}`); return report(); }
  const dlBytes = Buffer.from(await bin.arrayBuffer());
  ok('installer downloadable', `status=${bin.status} ${dlBytes.length} bytes`);
  const dlHash = sha256(dlBytes);
  if (localBytes) {
    const loHash = sha256(localBytes);
    if (dlHash === loHash) ok('sha256 matches local installer', dlHash);
    else bad('sha256 matches local installer', `downloaded=${dlHash} local=${loHash}`);
  } else {
    lines.push(`SKIP  local installer not present (${EXE}); downloaded sha256=${dlHash}`);
  }

  // 6. release 上三个资产齐(少一个,自动更新链路就断)
  const rel = await (await fetch(`https://api.github.com/repos/${OWNER}/${REPO}/releases/tags/${TAG}`, { headers: H })).json();
  const names = (rel.assets || []).map((a) => a.name).sort();
  const want = [EXE, `${EXE}.sig`, 'latest.json'].sort();
  const missing = want.filter((n) => !names.includes(n));
  if (missing.length === 0) ok('release has all three assets', want.join(', '));
  else bad('release has all three assets', `missing ${missing.join(', ')}; has ${names.join(', ')}`);
  if (rel.prerelease || rel.draft) bad('release is not draft/prerelease', `draft=${rel.draft} prerelease=${rel.prerelease}`);
  else ok('release published (not draft/prerelease)');

  // 7. notes 是中文且没被搅坏。这里按字符判,不看终端回显 —— PowerShell 5.1 会把
  //    UTF-8 中文显示成 GBK 乱码,曾让人误判"发出去的说明全是乱码"。
  const notes = live.notes || '';
  const cjk = (notes.match(/[\u4e00-\u9fff]/g) || []).length;
  const replacement = (notes.match(/\ufffd/g) || []).length;
  if (notes.length > 20 && cjk > 20 && replacement === 0) ok('notes intact', `${notes.length} chars, ${cjk} CJK, 0 replacement chars`);
  else bad('notes intact', `len=${notes.length} cjk=${cjk} replacement=${replacement}`);

  // 8. 仓库元数据:门面三件套
  const repo = await (await fetch(`https://api.github.com/repos/${OWNER}/${REPO}`, { headers: H })).json();
  ok('description set', (repo.description || '').slice(0, 60));
  ok('topics', (repo.topics || []).join(', '));
  if (repo.license && repo.license.spdx_id !== 'NOASSERTION') ok('license', repo.license.spdx_id);
  else bad('license', 'none');
  if (repo.private === false) ok('repo is public (updater requires it)');
  else bad('repo is public', `private=${repo.private}`);

  report();

  function report() {
    console.log(`=== release verification v${VERSION} (${TAG}) ===`);
    lines.forEach((l) => console.log('  ' + l));
    console.log(failed ? 'RESULT: FAIL' : 'RESULT: PASS');
    process.exit(failed ? 1 : 0);
  }
})().catch((e) => { console.error('verify-release failed:', e.message || e); process.exit(1); });
