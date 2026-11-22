// 端到端验签:确认 tauri.conf.json 里那把公钥,真能验证 release 上那个安装包或本地演练文件。
//
// 为什么值得单写一个脚本:
// verify-updater.js 只证明"线上 latest.json 的 signature 与线上 .sig 逐字相同" ——
// 那是**自洽**,不是**配对**。如果 secret 里那把私钥与 tauri.conf.json 里的公钥
// 不是一对(换过密钥、复制错文件),上面的检查照样全绿,而所有已装用户都会
// 更新失败 —— 那正是 RELEASE.md 里写"私钥丢了/换了就再也推不动更新"的那个坑。
//
// 唯一能终结这个疑点的是密码学本身:用配置里的公钥去验签。这里借 minisign
// crate(tauri 的依赖树里本来就有,不新增项目依赖)。
//
// 用法(需要本机有 GNU 工具链,因为要编一个小 bin):
//   node build/verify-signature.js
//   node build/verify-signature.js --local <文件> --sig <签名文件> [--pubkey <公钥文件或base64>]
//   node build/verify-signature.js --self-test
// 产物放在 %TEMP%\gt-sigverify,验完可以整个删掉。
const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFileSync, spawnSync } = require('child_process');

const root = path.join(__dirname, '..');
const conf = JSON.parse(fs.readFileSync(path.join(root, 'app', 'src-tauri', 'tauri.conf.json'), 'utf8'));
const V = conf.version;
const TAG = `v${V}`;
const EXE = `Grandettoken_${V}_x64-setup.exe`;
const BASE = `https://github.com/wacilimonster-source/Grandettoken/releases/download/${TAG}`;

const tmp = path.join(os.tmpdir(), 'gt-sigverify');
fs.mkdirSync(tmp, { recursive: true });

const SRC = `
use std::fs::File;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let pk = minisign::PublicKey::from_base64(args[1].trim()).expect("bad pubkey");
    let sig_box = minisign::SignatureBox::from_file(&args[2]).expect("bad sig file");
    let mut f = File::open(&args[3]).expect("bad data file");
    // quiet=true, output=false, allow_legacy=false
    match minisign::verify(&pk, &sig_box, &mut f, true, false, false) {
        Ok(_) => println!("VERIFY_OK"),
        Err(e) => { println!("VERIFY_FAIL: {}", e); std::process::exit(1); }
    }
}
`;

function parseArgs(argv) {
  const out = { local: null, sig: null, pubkey: null, selfTest: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--self-test') out.selfTest = true;
    else if (a === '--local') out.local = argv[++i] || null;
    else if (a === '--sig') out.sig = argv[++i] || null;
    else if (a === '--pubkey') out.pubkey = argv[++i] || null;
    else throw new Error(`未知参数: ${a}`);
  }
  if ((out.local && !out.sig) || (!out.local && out.sig)) {
    throw new Error('--local 与 --sig 必须同时提供');
  }
  return out;
}

function parsePubkeyKeyLine(pubkeySpec) {
  let rawText = pubkeySpec ? pubkeySpec.trim() : conf.plugins.updater.pubkey.trim();
  if (pubkeySpec && fs.existsSync(pubkeySpec)) {
    rawText = fs.readFileSync(pubkeySpec, 'utf8').trim();
  }
  let decoded = rawText;
  if (!rawText.startsWith('untrusted comment:')) {
    decoded = Buffer.from(rawText, 'base64').toString('utf8');
  }
  const pubLines = decoded.split(/\r?\n/);
  const keyLine = (pubLines[1] || '').trim();
  if (!keyLine) {
    throw new Error('pubkey 解析不出 minisign key 行');
  }
  return keyLine;
}

function decodeSigToFile(sigInputPath, sigBinOutPath) {
  const content = fs.readFileSync(sigInputPath, 'utf8').trim();
  if (content.startsWith('untrusted comment:')) {
    fs.writeFileSync(sigBinOutPath, content + '\n', 'utf8');
  } else {
    fs.writeFileSync(sigBinOutPath, Buffer.from(content, 'base64'));
  }
}

function buildCargoEnv() {
  const env = { ...process.env };
  const cargoBin = path.join(process.env.USERPROFILE, '.cargo', 'bin');
  const mingwBin = path.join(process.env.USERPROFILE, 'mingw64', 'bin');
  env.Path = `${cargoBin};${mingwBin};${env.Path || env.PATH || ''}`;
  env.CARGO_HOME = cargoBin.replace(/bin$/, '');
  env.RUSTUP_HOME = path.join(process.env.USERPROFILE, '.rustup');
  env.RUSTUP_TOOLCHAIN = 'stable-x86_64-pc-windows-gnu';
  env.CC = path.join(mingwBin, 'gcc.exe');
  const cargoExe = process.platform === 'win32' ? path.join(cargoBin, 'cargo.exe') : 'cargo';
  const cargoTauriExe = process.platform === 'win32' ? path.join(cargoBin, 'cargo-tauri.exe') : 'cargo-tauri';
  return { env, cargoExe, cargoTauriExe };
}

function ensureVerifierProj() {
  const proj = path.join(tmp, 'miniverify');
  fs.mkdirSync(path.join(proj, 'src'), { recursive: true });
  fs.writeFileSync(
    path.join(proj, 'Cargo.toml'),
    '[package]\nname = "miniverify"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\nminisign = "0.7"\n'
  );
  fs.writeFileSync(path.join(proj, 'src', 'main.rs'), SRC);
  return proj;
}

function runSelfTest() {
  const { env, cargoTauriExe } = buildCargoEnv();
  const testDir = fs.mkdtempSync(path.join(os.tmpdir(), 'gt-sig-selftest-'));
  const keyPath = path.join(testDir, 'temp.key');
  const pubPath = `${keyPath}.pub`;
  const dataPath = path.join(testDir, 'payload.bin');
  const sigPath = `${dataPath}.sig`;

  console.log('本地验签门禁 自检:');
  try {
    execFileSync(cargoTauriExe, ['signer', 'generate', '--ci', '-w', keyPath, '-f'], {
      env,
      stdio: 'pipe',
    });
    if (!fs.existsSync(keyPath) || !fs.existsSync(pubPath)) {
      throw new Error('cargo tauri signer generate 未生成临时密钥对');
    }
    const origBytes = Buffer.from('grandettoken-local-sig-verify-selftest-payload\n', 'utf8');
    fs.writeFileSync(dataPath, origBytes);
    execFileSync(cargoTauriExe, ['signer', 'sign', '-f', keyPath, '-p', '', dataPath], {
      env,
      stdio: 'pipe',
    });
    if (!fs.existsSync(sigPath)) {
      throw new Error('cargo tauri signer sign 未生成 .sig 文件');
    }

    // 1. 临时公钥验证临时签名必须通过
    const okRes = spawnSync(
      process.execPath,
      [__filename, '--local', dataPath, '--sig', sigPath, '--pubkey', pubPath],
      { env, encoding: 'utf8' }
    );
    if (okRes.status !== 0 || !okRes.stdout.includes('RESULT: PASS')) {
      throw new Error(`正样本验签失败: ${okRes.stderr || okRes.stdout}`);
    }
    console.log('  ✓ 临时测试密钥签名 + 临时公钥本地验签通过');

    // 2. 改动文件一个字节后再验必须失败
    const tampered = Buffer.from(origBytes);
    tampered[0] ^= 0xff;
    fs.writeFileSync(dataPath, tampered);
    const tamperRes = spawnSync(
      process.execPath,
      [__filename, '--local', dataPath, '--sig', sigPath, '--pubkey', pubPath],
      { env, encoding: 'utf8' }
    );
    if (tamperRes.status === 0) {
      throw new Error('篡改文件一个字节后验签居然通过');
    }
    console.log('  ✓ 改动文件一个字节后本地验签被判红');

    // 3. 恢复原文件,用正式公钥(tauri.conf.json)验证临时签名必须失败
    fs.writeFileSync(dataPath, origBytes);
    const wrongKeyRes = spawnSync(
      process.execPath,
      [__filename, '--local', dataPath, '--sig', sigPath],
      { env, encoding: 'utf8' }
    );
    if (wrongKeyRes.status === 0) {
      throw new Error('用正式公钥验证临时密钥签名居然通过');
    }
    console.log('  ✓ 用正式公钥验证临时签名被判红');
  } finally {
    fs.rmSync(testDir, { recursive: true, force: true });
    if (fs.existsSync(testDir)) {
      throw new Error(`临时密钥目录未清理干净: ${testDir}`);
    }
    console.log('  ✓ 临时密钥与测试文件已清理');
  }
  console.log('RESULT: PASS  verify-signature.js --self-test 全部通过');
}

(async () => {
  const opts = parseArgs(process.argv.slice(2));
  if (opts.selfTest) {
    runSelfTest();
    return;
  }

  // 1. 公钥文件(minisign 的两行格式:untrusted comment + key 行)
  const keyLine = parsePubkeyKeyLine(opts.pubkey);
  console.log(`PASS  pubkey parsed (${opts.pubkey ? 'custom' : 'tauri.conf.json'})`);

  let targetFile;
  let sigBin;

  if (opts.local) {
    targetFile = path.resolve(opts.local);
    const sigFile = path.resolve(opts.sig);
    if (!fs.existsSync(targetFile)) throw new Error(`本地文件不存在: ${targetFile}`);
    if (!fs.existsSync(sigFile)) throw new Error(`签名文件不存在: ${sigFile}`);
    sigBin = path.join(tmp, `local-${process.pid}.minisig`);
    decodeSigToFile(sigFile, sigBin);
    console.log(`PASS  loaded local file (${fs.statSync(targetFile).size} bytes) + decoded .sig`);
  } else {
    // 2. 下载 release 上的安装包与 .sig。
    //    用 PowerShell 下载:这台机器必须走系统代理(127.0.0.1:7897),node 24 的
    //    原生 fetch 不读系统代理也不认 HTTPS_PROXY,直连十秒超时(详见
    //    verify-updater.js 顶部注释)。
    const exePath = path.join(tmp, EXE);
    const sigB64 = path.join(tmp, `${EXE}.sig`);
    sigBin = path.join(tmp, `${EXE}.minisig`);
    const dl = (url, out) => {
      const ps = `
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -Uri '${url}' -OutFile '${out}' -UseBasicParsing -TimeoutSec 300
      `.trim();
      execFileSync('powershell', ['-NoProfile', '-Command', ps], { stdio: 'pipe' });
      if (!fs.existsSync(out) || fs.statSync(out).size === 0) throw new Error(`download produced nothing: ${url}`);
    };
    dl(`${BASE}/${EXE}`, exePath);
    dl(`${BASE}/${EXE}.sig`, sigB64);
    console.log(`PASS  downloaded installer (${fs.statSync(exePath).size} bytes) + .sig`);

    // 3. Tauri 的 .sig 是"整个 minisign 文件的 base64",先解回二进制
    decodeSigToFile(sigB64, sigBin);
    console.log('PASS  decoded .sig into minisign form');
    targetFile = exePath;
  }

  // 4. 编一个一次性小 bin 来验(用 registry 里已有的 minisign,不碰项目 Cargo.toml)
  const proj = ensureVerifierProj();
  const { env, cargoExe } = buildCargoEnv();

  console.log('...  compiling the one-off verifier (first run takes ~30s)');
  execFileSync(cargoExe, ['run', '--quiet', '--', keyLine, sigBin, targetFile], {
    cwd: proj,
    env,
    stdio: 'inherit',
  });
  if (opts.local) {
    console.log(`RESULT: PASS  公钥成功验签本地文件 ${path.basename(targetFile)}`);
  } else {
    console.log(`RESULT: PASS  tauri.conf.json 的公钥能验签 ${TAG} 的安装包 —— 已装用户收得到这一版`);
  }
})().catch((e) => {
  console.error(`RESULT: FAIL  ${e.message || e}`);
  process.exit(1);
});
