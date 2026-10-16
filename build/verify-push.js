// 验证推送结果,并确认 token 没有落到任何本地文件。
// token 从环境变量读,不写进本文件。
const fs = require('fs');
const path = require('path');

const t = process.env.GH_TOKEN;
const ROOT = path.resolve(__dirname, '..');
const OWNER = 'wacilimonster-source';
const REPO = 'Grandettoken';

(async () => {
  if (!t) { console.log('GH_TOKEN not set'); process.exit(1); }
  const H = { Authorization: 'Bearer ' + t, 'User-Agent': 'node', Accept: 'application/vnd.github+json' };

  const cr = await fetch(`https://api.github.com/repos/${OWNER}/${REPO}/commits/main`, { headers: H });
  if (cr.status === 200) {
    const j = await cr.json();
    console.log('GitHub HEAD :', j.sha.slice(0, 8), '|', j.commit.message.split('\n')[0]);
    console.log('committed   :', j.commit.author.date);
  } else {
    console.log('commit check ->', cr.status);
  }

  const tr = await fetch(`https://api.github.com/repos/${OWNER}/${REPO}/git/trees/main?recursive=1`, { headers: H });
  const tj = await tr.json();
  const blobs = (tr.status === 200 && Array.isArray(tj.tree) ? tj.tree : []).filter((x) => x.type === 'blob');
  console.log('files on GitHub:', blobs.length);
  console.log('');
  console.log('--- tree ---');
  (tj.tree || []).forEach((x) => console.log((x.type === 'tree' ? '[d] ' : '    ') + x.path));

  console.log('');
  console.log('=== scanning local files for the token ===');
  // .git 整体跳过(objects/pack 又大又是二进制),但 token 恰好最容易落在 .git 里
  // 的纯文本文件上:
  //   .git/config    <- `git remote set-url https://user:TOKEN@github.com/...`,这是和
  //                     RELEASE.md 里 `git credential fill` 并列的常见做法
  //   .git/logs/**   <- reflog 会把远端 URL 原样写进去(head 与 refs/remotes 都有)
  //   .git/FETCH_HEAD<- 按 URL fetch 时同样会写进完整 URL
  // 所以这几处单独扫。以前只扫工作区却输出"token is not stored anywhere on disk",
  // 结论比证据宽 —— 现在只说扫过哪里。
  const SKIP_DIR = /(\\|\/)(\.git|dl|mingw64|git|target|node_modules)(\\|\/|$)/;
  const SKIP_EXT = /\.(png|ico|db|db-wal|db-shm|exe|dll|rlib|rmeta|bin|lock)$/i;

  let hits = 0, scanned = 0;
  const scanText = (p) => {
    if (SKIP_EXT.test(path.basename(p))) return;
    let c;
    try { c = fs.readFileSync(p, 'utf8'); } catch { return; } // binary or unreadable
    scanned++;
    if (c.includes(t)) { console.log('  LEAK:', p.replace(ROOT, '.')); hits++; }
  };
  const walk = (d) => {
    let entries;
    try { entries = fs.readdirSync(d, { withFileTypes: true }); } catch { return; }
    for (const e of entries) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) { if (!SKIP_DIR.test(p)) walk(p); continue; }
      scanText(p);
    }
  };
  walk(ROOT);

  // 上面那个 walk 把 .git 跳过了:这里补上 .git 里的纯文本落点(远端 URL 就在 config 里)
  const gitDir = path.join(ROOT, '.git');
  const gitTargets = [path.join(gitDir, 'config'), path.join(gitDir, 'FETCH_HEAD')];
  const collectLogs = (d) => {
    let entries;
    try { entries = fs.readdirSync(d, { withFileTypes: true }); } catch { return; }
    for (const e of entries) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) collectLogs(p);
      else gitTargets.push(p);
    }
  };
  collectLogs(path.join(gitDir, 'logs'));
  for (const p of gitTargets) scanText(p);

  const scope = 'the repo tree (without .git objects/pack, dl, mingw64, node_modules, target) plus .git/config, .git/logs and .git/FETCH_HEAD';
  console.log(`scanned ${scanned} text files; token occurrences: ${hits}`);
  console.log(`scope: ${scope}`);
  console.log(hits === 0 ? 'CLEAN - token not stored in any scanned location.' : 'WARNING - token found on disk!');
  // 这个脚本是用来"确认没泄漏、确实推上去了"的,发现泄漏却以 0 退出等于反向造假(报告 G-2)。
  const ok = hits === 0 && cr.status === 200 && tr.status === 200 && blobs.length > 0;
  if (!ok) {
    console.log('FAIL - ' + [
      hits > 0 ? 'token found on disk (' + hits + ')' : '',
      cr.status !== 200 ? 'commit lookup HTTP ' + cr.status : '',
      tr.status !== 200 ? 'tree lookup HTTP ' + tr.status : '',
      blobs.length === 0 ? 'no files on GitHub' : '',
    ].filter(Boolean).join('; '));
  }
  process.exit(ok ? 0 : 1);
})();
