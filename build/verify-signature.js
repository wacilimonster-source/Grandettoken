// 端到端验签:确认 tauri.conf.json 里那把公钥,真能验证 release 上那个安装包。
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
// 产物放在 %TEMP%\gt-sigverify,验完可以整个删掉。
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

(async () => {
  // 1. 公钥文件(minisign 的两行格式:untrusted comment + key 行)
  const pubRaw = Buffer.from(conf.plugins.updater.pubkey, 'base64');
  const pubLines = pubRaw.toString('utf8').split('\n');
  const keyLine = (pubLines[1] || '').trim();
  if (!keyLine) { console.log('FAIL  tauri.conf.json 里的 pubkey 解析不出 key 行'); process.exit(1); }
  console.log(`PASS  pubkey parsed from tauri.conf.json`);

  // 2. 下载 release 上的安装包与 .sig。
  //    用 PowerShell 下载:这台机器必须走系统代理(127.0.0.1:7897),node 24 的
  //    原生 fetch 不读系统代理也不认 HTTPS_PROXY,直连十秒超时(详见
  //    verify-updater.js 顶部注释)。
  const exePath = path.join(tmp, EXE);
  const sigB64 = path.join(tmp, `${EXE}.sig`);
  const sigBin = path.join(tmp, `${EXE}.minisig`);
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
  fs.writeFileSync(sigBin, Buffer.from(fs.readFileSync(sigB64, 'utf8').trim(), 'base64'));
  console.log('PASS  decoded .sig into minisign form');

  // 4. 编一个一次性小 bin 来验(用 registry 里已有的 minisign,不碰项目 Cargo.toml)
  const proj = path.join(tmp, 'miniverify');
  fs.mkdirSync(path.join(proj, 'src'), { recursive: true });
  fs.writeFileSync(path.join(proj, 'Cargo.toml'), '[package]\nname = "miniverify"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\nminisign = "0.7"\n');
  fs.writeFileSync(path.join(proj, 'src', 'main.rs'), SRC);

  const env = { ...process.env };
  const cargoBin = path.join(process.env.USERPROFILE, '.cargo', 'bin');
  const mingwBin = path.join(process.env.USERPROFILE, 'mingw64', 'bin');
  env.Path = `${cargoBin};${mingwBin};${env.Path}`;
  env.CARGO_HOME = cargoBin.replace(/bin$/, '');
  env.RUSTUP_HOME = path.join(process.env.USERPROFILE, '.rustup');
  env.RUSTUP_TOOLCHAIN = 'stable-x86_64-pc-windows-gnu';
  env.CC = path.join(mingwBin, 'gcc.exe');

  console.log('...  compiling the one-off verifier (first run takes ~30s)');
  // 不用 shell:true —— 那会把参数拼成命令行,既触发 DEP0190 警告,也让 keyLine
  // 里可能出现的 shell 元字符有解释空间。Windows 上直接调 cargo.exe 即可,
  // PATH 已经过 env.Path 传进子进程(env 是 {...process.env} 的副本)。
  // 但**别**只写 "cargo.exe" 靠 PATH 解析:在受限环境里(代理沙箱、某些 CI 的
  // 精简 PATH)子进程会直接 ENOENT,看起来像"验签失败"其实是找不到编译器。
  // cargoBin 上面已经算好了,用绝对路径最稳。
  const cargoExe =
    process.platform === 'win32' ? path.join(cargoBin, 'cargo.exe') : 'cargo';
  execFileSync(cargoExe, ['run', '--quiet', '--', keyLine, sigBin, exePath], {
    cwd: proj, env, stdio: 'inherit',
  });
  console.log(`RESULT: PASS  tauri.conf.json 的公钥能验签 ${TAG} 的安装包 —— 已装用户收得到这一版`);
})().catch((e) => {
  console.error(`RESULT: FAIL  ${e.message || e}`);
  process.exit(1);
});
