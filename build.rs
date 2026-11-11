// 从仓库内冻结的 RustDesk 端侧消息快照生成 protobuf 类型。
//
// 快照位于 protocol/rustdesk/message-v1.proto，由 protocol/sources.json 固定来源提交与摘要；
// 这里只读取快照，不依赖 references/ 下的 RustDesk 工作树，也不引入其服务端或 UI 代码。

#[path = "scripts/brand_build.rs"]
mod brand_build;
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,--exclude-libs,ALL");
        println!("cargo:rustc-cdylib-link-arg=-Wl,-z,defs");
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let logo = brand_build::configure(root);
    brand_build::configure_executable_icon(root, &logo);
    let proto_dir = root.join("protocol/rustdesk");
    let proto = proto_dir.join("message-v1.proto");
    assert!(
        proto.exists(),
        "缺少冻结的 RustDesk 消息快照: {}",
        proto.display()
    );

    let out = std::path::Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("protos");
    std::fs::create_dir_all(&out).expect("创建 protobuf 输出目录");
    protobuf_codegen::Codegen::new()
        .pure()
        .out_dir(&out)
        .inputs([proto])
        .include(&proto_dir)
        .run()
        .expect("生成 RustDesk 端侧消息类型失败");
    println!("cargo:rerun-if-changed={}", proto_dir.display());

    link_engine();
}

/// 链接 `desk-engine` 子 crate 产出的静态库（feature `desk-engine`）。
///
/// 子 crate 有自己的 workspace 与锁文件，Cargo 无法把它当作普通的 path 依赖
/// （RustDesk 的 fork 依赖会与主 crate 冲突）。因此先由 `scripts/build_engine.py`
/// 单独构建它，这里只负责把产物和它的原生依赖接进链接行。
fn link_engine() {
    if std::env::var_os("CARGO_FEATURE_DESK_ENGINE").is_none() {
        return;
    }
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-env-changed=YOODESK_DESK_ENGINE_LIB_DIR");
    let lib_dir = std::env::var_os("YOODESK_DESK_ENGINE_LIB_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| manifest.join("desk-engine").join("target").join("release"));
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    let library = lib_dir.join(if target_os == "windows" {
        "yoodesk_desk_engine.lib"
    } else {
        "libyoodesk_desk_engine.a"
    });
    assert!(
        library.exists(),
        "desk-engine 静态库不存在：{}\n先运行：python -I scripts/build_engine.py",
        library.display()
    );
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=yoodesk_desk_engine");
    println!("cargo:rerun-if-changed={}", library.display());
    if target_os == "macos" {
        let dependencies = lib_dir.join("native-static-libs.txt");
        println!("cargo:rerun-if-changed={}", dependencies.display());
        let flags = std::fs::read_to_string(&dependencies)
            .expect("先运行 build_engine.py 生成 macOS Desk 链接依赖");
        let mut flags = flags.split_whitespace();
        while let Some(flag) = flags.next() {
            if flag == "-framework" {
                let name = flags.next().expect("缺少 framework 名称");
                println!("cargo:rustc-link-lib=framework={name}");
            } else if let Some(name) = flag.strip_prefix("-l") {
                println!("cargo:rustc-link-lib={name}");
            } else {
                panic!("未知 macOS Desk 链接参数：{flag}");
            }
        }
        return;
    }
    if target_os == "linux" {
        const CODECS: &[&str] = &["yuv", "aom", "vpx", "opus", "sodium"];
        const FFMPEG: &[&str] = &[
            "avformat",
            "avcodec",
            "swresample",
            "avutil",
            "x264",
            "x265",
            "z",
        ];
        println!("cargo:rerun-if-env-changed=YOODESK_STATIC_CODEC_DIR");
        println!("cargo:rerun-if-env-changed=YOODESK_STATIC_FFMPEG_DIR");
        let static_codecs = std::env::var_os("YOODESK_STATIC_CODEC_DIR");
        let static_ffmpeg = std::env::var_os("YOODESK_STATIC_FFMPEG_DIR");
        println!("cargo:rerun-if-env-changed=YOODESK_STATIC_AOM_DIR");
        println!("cargo:rerun-if-env-changed=YOODESK_STATIC_YUV_DIR");
        let static_aom = std::env::var_os("YOODESK_STATIC_AOM_DIR");
        let static_yuv = std::env::var_os("YOODESK_STATIC_YUV_DIR");
        let static_search =
            std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("static-native");
        if static_ffmpeg.is_some() || static_codecs.is_some() || static_aom.is_some() || static_yuv.is_some() {
            std::fs::create_dir_all(&static_search).expect("Create static archive directory");
            println!("cargo:rustc-link-search=native={}", static_search.display());
        }
        for (dir, libraries) in [(&static_ffmpeg, FFMPEG), (&static_codecs, CODECS), (&static_aom, &["aom"][..]), (&static_yuv, &["yuv"][..])] {
            if let Some(dir) = dir {
                let dir = std::path::Path::new(dir);
                for name in libraries {
                    let archive = dir.join(format!("lib{name}.a"));
                    assert!(
                        archive.is_file(),
                        "Missing static archive: {}",
                        archive.display()
                    );
                    println!("cargo:rerun-if-changed={}", archive.display());
                    // The sysroot can also contain distro archives with the same names.
                    std::fs::copy(&archive, static_search.join(format!("libyoodesk_{name}.a")))
                        .expect("Stage selected static archive");
                }
            }
        }
        let link_static = |name: &str| {
            (static_codecs.is_some() && CODECS.contains(&name))
                || (static_ffmpeg.is_some() && FFMPEG.contains(&name))
                || (static_aom.is_some() && name == "aom")
                || (static_yuv.is_some() && name == "yuv")
        };
        let flags = std::process::Command::new("pkg-config")
            .args([
                "--libs",
                "gstreamer-app-1.0",
                "gstreamer-video-1.0",
                "gio-2.0",
                "gtk+-3.0",
                "libudev",
                "libyuv",
                "aom",
                "libsodium",
            ])
            .output()
            .expect("Linux desktop build requires pkg-config");
        assert!(
            flags.status.success(),
            "Linux desktop development libraries unavailable"
        );
        for flag in String::from_utf8(flags.stdout).unwrap().split_whitespace() {
            if let Some(name) = flag.strip_prefix("-l") {
                if link_static(name) {
                    continue;
                }
                println!("cargo:rustc-link-lib={name}");
            } else if let Some(path) = flag.strip_prefix("-L") {
                println!("cargo:rustc-link-search=native={path}");
            }
        }
        // Linux 使用系统多媒体/X11 库，不能传入 Windows 导入库或静态 CRT 参数。
        for name in [
            "avformat",
            "avcodec",
            "swresample",
            "avutil",
            "yuv",
            "vpx",
            "aom",
            "opus",
            "sodium",
            "X11",
            "Xext",
            "Xfixes",
            "Xrandr",
            "Xtst",
            "xkbcommon",
            "xcb",
            "xcb-shm",
            "xcb-randr",
            "xcb-xfixes",
            "drm",
            "gbm",
            "asound",
            "pulse",
            "pulse-simple",
            "dbus-1",
            "udev",
            "stdc++",
            "dl",
            "m",
            "pthread",
        ] {
            if link_static(name) {
                println!("cargo:rustc-link-lib=static=yoodesk_{name}");
            } else {
                println!("cargo:rustc-link-lib={name}");
            }
        }
        if static_ffmpeg.is_some() {
            for name in ["x264", "x265", "z"] {
                println!("cargo:rustc-link-lib=static=yoodesk_{name}");
            }
            // Drivers remain provided by the host; FFmpeg's VAAPI glue needs their ABI.
            for name in ["va-drm", "va", "atomic"] {
                println!("cargo:rustc-link-lib={name}");
            }
        }
        return;
    }
    // 引擎用静态 CRT：ffmpeg/libx264/libx265/libmfx 都是 /MT 构建。
    println!("cargo:rerun-if-env-changed=VCPKG_INSTALLED_ROOT");
    if let Ok(vcpkg) = std::env::var("VCPKG_INSTALLED_ROOT") {
        let triplet = match std::env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
            "x86" => "x86-windows-static",
            "x86_64" => "x64-windows-static",
            "aarch64" => "arm64-windows-static",
            arch => panic!("unsupported Windows Desk architecture: {arch}"),
        };
        println!(
            "cargo:rustc-link-search=native={}",
            std::path::Path::new(&vcpkg)
                .join(triplet)
                .join("lib")
                .display()
        );
    }
    for name in [
        "avcodec",
        "avutil",
        "avformat",
        "libx264",
        "x265-static",
        "yuv",
        "vpx",
        "aom",
        "opus",
    ] {
        println!("cargo:rustc-link-lib=static={name}");
    }
    if matches!(
        std::env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str(),
        "x86" | "x86_64"
    ) {
        println!("cargo:rustc-link-lib=static=libmfx");
    }
    // 独立静态库的原生依赖必须在宿主链接时明确提供。
    // 引擎不链接官方 GUI 框架；系统库用于采集/编码/输入/网络及本进程隐私窗口。
    for name in [
        "User32",
        "bcrypt",
        "ole32",
        "advapi32",
        "d3d11",
        "dxgi",
        "mfuuid",
        "strmiids",
        "WtsApi32",
        "sas",
        "Newdev",
        "cfgmgr32",
        "comctl32",
        "credui",
        "crypt32",
        "cryptnet",
        "dbghelp",
        "fwpuclnt",
        "gdi32",
        "iphlpapi",
        "msimg32",
        "ncrypt",
        "ntdll",
        "opengl32",
        "pdh",
        "secur32",
        "setupapi",
        "shcore",
        "shell32",
        "shlwapi",
        "synchronization",
        "userenv",
        "winspool",
        "ws2_32",
        "legacy_stdio_definitions",
        "psapi",
        "netapi32",
        "powrprof",
        "oleaut32",
        "uuid",
        "winhttp",
        "winmm",
        "version",
        "wtsapi32",
        // VARIANT 转换（系统属性 API，非窗口系统）。
        "propsys",
    ] {
        println!("cargo:rustc-link-lib={name}");
    }
    println!("cargo:rerun-if-changed={}", library.display());
}
