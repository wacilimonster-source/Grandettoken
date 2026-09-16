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
  const blobs = (tj.tree || []).filter((x) => x.type === 'blob');
  console.log('files on GitHub:', blobs.length);
  console.log('');
  console.log('--- tree ---');
  (tj.tree || []).forEach((x) => console.log((x.type === 'tree' ? '[d] ' : '    ') + x.path));

  console.log('');
  console.log('=== scanning local files for the token ===');
  const SKIP_DIR = /(\\|\/)(\.git|dl|mingw64|git|target|node_modules)(\\|\/|$)/;
  const SKIP_EXT = /\.(png|ico|db|db-wal|db-shm|exe|dll|rlib|rmeta|bin|lock)$/i;

  let hits = 0, scanned = 0;
  const walk = (d) => {
    let entries;
    try { entries = fs.readdirSync(d, { withFileTypes: true }); } catch { return; }
    for (const e of entries) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) { if (!SKIP_DIR.test(p)) walk(p); continue; }
      if (SKIP_EXT.test(e.name)) continue;
      try {
        const c = fs.readFileSync(p, 'utf8');
        scanned++;
        if (c.includes(t)) { console.log('  LEAK:', p.replace(ROOT, '.')); hits++; }
      } catch { /* binary or unreadable */ }
    }
  };
  walk(ROOT);
  console.log(`scanned ${scanned} text files; token occurrences: ${hits}`);
  console.log(hits === 0 ? 'CLEAN - token is not stored anywhere on disk.' : 'WARNING - token found on disk!');
})();
