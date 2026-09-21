use std::path::PathBuf;

/// 单 exe 交付:让 `-lWebView2Loader.dll` 命中自建的静态归档。
///
/// 背景:Tauri 的 webview2-com-sys 在 Windows-GNU 上写的是
/// `#[link(name = "WebView2Loader.dll")]`,exe 会留下加载期 DLL 依赖
/// (只有 MSVC 工具链那条分支才是静态链接)。GNU ld 解析 `-lWebView2Loader.dll`
/// 时只找 `libWebView2Loader.dll.a`,所以把 MSVC 的 WebView2LoaderStatic.lib
/// 连同 MSVC CRT 垫片重打成同名归档(build/make-webview2-static.ps1),
/// 在这里挂进链接搜索路径。
///
/// 注意:crate 自己的 build script 也会把含真 DLL 的目录加进搜索路径,
/// 谁先被搜到取决于 -L 顺序、不可靠 —— 所以 build/build.ps1 每次构建前会把
/// 那份 WebView2Loader.dll 删掉,只留下这个归档可被命中。
/// 删掉 crate 复制出来的那份真 DLL。
/// 它和我们的归档都能满足 `-lWebView2Loader.dll`,谁先被搜到取决于 -L 顺序;
/// 删掉它,搜索路径上就只剩静态归档可命中。
/// OUT_DIR = <target>/<profile>/build/<pkg>-<hash>/out,往上第三层就是 build 目录。
fn strip_crate_dll() {
    let Ok(out) = std::env::var("OUT_DIR") else { return };
    let Some(build_dir) = PathBuf::from(out).ancestors().nth(2).map(PathBuf::from) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&build_dir) else { return };
    for e in entries.flatten() {
        if !e.file_name().to_string_lossy().starts_with("webview2-com-sys-") {
            continue;
        }
        // 要删两个:`WebView2Loader.dll` 本身,以及 `WebView2Loader.dll.lib`。
        // ld 解析 `-lWebView2Loader.dll` 时候选后缀包含 `.lib`,留着那个导入库
        // 它就会命中导入库、照样生成 DLL 依赖(实测踩过)。
        for name in ["WebView2Loader.dll", "WebView2Loader.dll.lib"] {
            let f = e.path().join("out").join("x64").join(name);
            if f.exists() {
                match std::fs::remove_file(&f) {
                    Ok(()) => println!("cargo:warning=已删除 {} (改静态链接,单 exe 交付)", f.display()),
                    Err(err) => println!("cargo:warning=删除 {} 失败: {}", f.display(), err),
                }
            }
        }
    }
}

/// 版本号一致性:发版链里 Cargo.toml、tauri.conf.json、git tag、latest.json 四处
/// 都要写同一个版本号,漏改任何一处都会打出「装的是 0.1.14、清单说自己 0.1.13」
/// 这种最难查的包(报告里的构建断言项)。编译期直接失败,不留给人肉核对。
fn assert_versions_match() {
    let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") else { return };
    let conf = PathBuf::from(&manifest_dir).join("tauri.conf.json");
    println!("cargo:rerun-if-changed={}", conf.display());
    let raw = match std::fs::read_to_string(&conf) {
        Ok(s) => s,
        // 配置文件读不到时 tauri_build 自己会报,这里不重复报错
        Err(_) => return,
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else { return };
    let Some(conf_version) = json.get("version").and_then(|v| v.as_str()) else { return };
    let crate_version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        conf_version, crate_version,
        "tauri.conf.json 的 version({conf_version})与 Cargo.toml 的 version({crate_version})不一致"
    );
}

fn main() {
    assert_versions_match();
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
        strip_crate_dll();
        let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let out_dir = manifest
            .parent()
            .and_then(|p| p.parent())
            .map(|root| root.join("build").join("webview2-static").join("out"));
        match out_dir {
            Some(dir) if dir.join("libWebView2Loader.dll.a").exists() => {
                println!("cargo:rustc-link-search=native={}", dir.display());
                // 静态加载器内部用到 COM 任务内存与 GUID,这两个导入要补上
                println!("cargo:rustc-link-lib=ole32");
                println!("cargo:rustc-link-lib=uuid");
                println!(
                    "cargo:rerun-if-changed={}",
                    dir.join("libWebView2Loader.dll.a").display()
                );
            }
            Some(dir) => println!(
                "cargo:warning=静态加载器归档缺失({}),先跑 build/make-webview2-static.ps1;否则会退回 exe+DLL 交付",
                dir.display()
            ),
            None => {}
        }
    }
    tauri_build::build()
}
