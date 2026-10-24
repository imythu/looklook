use std::env;
use std::path::PathBuf;

#[cfg(all(feature = "vendored", feature = "vendored-libdatachannel"))]
compile_error!(
    "Features: 'vendored' and 'vendored-libdatachannel' cannot be enabled at the same time!"
);

#[cfg(feature = "vendored")]
use once_cell::sync::OnceCell;

#[allow(dead_code)]
fn env_var_rerun(name: &str) -> Result<String, env::VarError> {
    println!("cargo:rerun-if-env-changed={}", name);
    env::var(name)
}

#[cfg(feature = "vendored")]
pub fn openssl_artifacts() -> &'static openssl_src::Artifacts {
    static INSTANCE: OnceCell<openssl_src::Artifacts> = OnceCell::new();
    INSTANCE.get_or_init(|| openssl_src::Build::new().build())
}

#[cfg(any(feature = "vendored", feature = "vendored-libdatachannel"))]
fn link_static_libdatachannel(out_dir: &str, profile: &str) {
    // Link static libc++
    cpp_build::Config::new()
        .include(format!("{}/lib", out_dir))
        .build("src/lib.rs");

    // Link static libjuice
    if cfg!(target_env = "msvc") {
        println!(
            "cargo:rustc-link-search=native={}/build/deps/libjuice/{}",
            out_dir, profile
        );
    } else {
        println!(
            "cargo:rustc-link-search=native={}/build/deps/libjuice",
            out_dir
        );
    }
    println!("cargo:rustc-link-lib=static=juice-static");

    // Link static usrsctplib
    if cfg!(target_env = "msvc") {
        println!(
            "cargo:rustc-link-search=native={}/build/deps/usrsctp/usrsctplib/{}",
            out_dir, profile
        );
    } else {
        println!(
            "cargo:rustc-link-search=native={}/build/deps/usrsctp/usrsctplib",
            out_dir
        );
    }
    println!("cargo:rustc-link-lib=static=usrsctp");

    if cfg!(feature = "media") {
        // Link static libsrtp
        if cfg!(target_env = "msvc") {
            println!(
                "cargo:rustc-link-search=native={}/build/deps/libsrtp/{}",
                out_dir, profile
            );
        } else {
            println!(
                "cargo:rustc-link-search=native={}/build/deps/libsrtp",
                out_dir
            );
        }
        println!("cargo:rustc-link-lib=static=srtp2");
    }

    // Link static libdatachannel
    if cfg!(target_env = "msvc") {
        println!(
            "cargo:rustc-link-search=native={}/build/{}",
            out_dir, profile
        );
    } else {
        println!("cargo:rustc-link-search=native={}/build", out_dir);
    }
    println!("cargo:rustc-link-lib=static=datachannel-static");
}

fn main() {
    // looklook: 只写了 rerun-if-env-changed 时 cargo 不再看源码，改了 libjuice / libdatachannel 的补丁不会重新编译
    println!("cargo:rerun-if-changed=libdatachannel");
    let out_dir = env::var("OUT_DIR").unwrap();

    #[cfg(feature = "vendored-libdatachannel")]
    {
        let mut cmake_conf = cmake::Config::new("libdatachannel");
        cmake_conf.build_target("datachannel-static");
        cmake_conf.out_dir(&out_dir);

        cmake_conf.define("CMAKE_POLICY_VERSION_MINIMUM", "3.5");
        cmake_conf.define("NO_WEBSOCKET", "ON");
        cmake_conf.define("NO_EXAMPLES", "ON");
        if !cfg!(feature = "media") {
            cmake_conf.define("NO_MEDIA", "ON");
        }

        if let Ok(openssl_root_dir) = env_var_rerun("OPENSSL_ROOT_DIR") {
            cmake_conf.define("OPENSSL_ROOT_DIR", openssl_root_dir);
        }
        if let Ok(openssl_libraries) = env_var_rerun("OPENSSL_LIBRARIES") {
            cmake_conf.define("OPENSSL_LIBRARIES", openssl_libraries);
        }

        cmake_conf.build();

        // Link dynamic openssl

        if cfg!(target_env = "msvc") {
            println!("cargo:rustc-link-lib=dylib=libssl");
            println!("cargo:rustc-link-lib=dylib=libcrypto");
        } else {
            println!("cargo:rustc-link-lib=dylib=ssl");
            println!("cargo:rustc-link-lib=dylib=crypto");
        }

        let profile = cmake_conf.get_profile();

        link_static_libdatachannel(&out_dir, profile);
    }

    #[cfg(feature = "vendored")]
    {
        let mut cmake_conf = cmake::Config::new("libdatachannel");
        cmake_conf.build_target("datachannel-static");
        cmake_conf.out_dir(&out_dir);

        cmake_conf.define("CMAKE_POLICY_VERSION_MINIMUM", "3.5");
        cmake_conf.define("NO_WEBSOCKET", "ON");
        cmake_conf.define("NO_EXAMPLES", "ON");
        // looklook: 副本里删掉了 test/ 和各依赖的测试目录，不关掉的话 CMake 配置阶段就找不到源文件
        cmake_conf.define("NO_TESTS", "ON");
        if !cfg!(feature = "media") {
            cmake_conf.define("NO_MEDIA", "ON");
        }

        let openssl_root_dir = openssl_artifacts().lib_dir().parent().unwrap();
        cmake_conf.define("OPENSSL_ROOT_DIR", openssl_root_dir.to_path_buf());
        cmake_conf.define(
            "OPENSSL_INCLUDE_DIR",
            openssl_root_dir.to_path_buf().join("include"),
        );
        cmake_conf.define(
            "OPENSSL_CRYPTO_LIBRARY",
            openssl_root_dir.to_path_buf().join("lib/libcrypto.a"),
        );
        cmake_conf.define(
            "OPENSSL_SSL_LIBRARY",
            openssl_root_dir.to_path_buf().join("lib/libssl.a"),
        );
        cmake_conf.define("OPENSSL_USE_STATIC_LIBS", "TRUE");
        // looklook: 交叉编译时别用宿主机 pkg-config 里的 openssl.pc（会带进 -lz），路径上面已经显式给了。
        // 不能用 CMAKE_DISABLE_FIND_PACKAGE_PkgConfig：CMake 3.28 的 FindOpenSSL 不管它照样调 pkg_check_modules，
        // 配置阶段直接报 “Unknown CMake command”。改成让 pkg-config 只在一个空目录里找，什么都找不到。
        let no_pc = std::path::Path::new(&out_dir).join("no-pkgconfig");
        std::fs::create_dir_all(&no_pc).unwrap();
        cmake_conf.env("PKG_CONFIG_LIBDIR", &no_pc);
        cmake_conf.env("PKG_CONFIG_PATH", "");

        cmake_conf.build();

        let profile = cmake_conf.get_profile();

        // Link static openssl
        println!(
            "cargo:rustc-link-search=native={}",
            openssl_artifacts().lib_dir().to_path_buf().display()
        );
        if cfg!(target_env = "msvc") {
            println!("cargo:rustc-link-lib=static=libcrypto");
            println!("cargo:rustc-link-lib=static=libssl");
        } else {
            println!("cargo:rustc-link-lib=static=crypto");
            println!("cargo:rustc-link-lib=static=ssl");
        }
        // looklook: 静态 OpenSSL（随机数、证书库）与 libjuice（网卡枚举）依赖的 Windows 系统库；按目标判断，交叉编译时 cfg! 看的是宿主
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            for lib in ["ws2_32", "bcrypt", "iphlpapi", "crypt32", "user32", "advapi32"] {
                println!("cargo:rustc-link-lib=dylib={lib}");
            }
        }

        link_static_libdatachannel(&out_dir, profile);
    }

    #[cfg(not(any(feature = "vendored", feature = "vendored-libdatachannel")))]
    {
        let mut cmake_conf = cmake::Config::new("libdatachannel");
        cmake_conf.out_dir(&out_dir);

        cmake_conf.define("CMAKE_POLICY_VERSION_MINIMUM", "3.5");
        cmake_conf.define("NO_WEBSOCKET", "ON");
        cmake_conf.define("NO_EXAMPLES", "ON");
        if !cfg!(feature = "media") {
            cmake_conf.define("NO_MEDIA", "ON");
        }

        if let Ok(openssl_root_dir) = env_var_rerun("OPENSSL_ROOT_DIR") {
            cmake_conf.define("OPENSSL_ROOT_DIR", openssl_root_dir);
        }
        if let Ok(openssl_libraries) = env_var_rerun("OPENSSL_LIBRARIES") {
            cmake_conf.define("OPENSSL_LIBRARIES", openssl_libraries);
        }

        cmake_conf.build();

        // Link dynamic libdatachannel
        println!("cargo:rustc-link-search=native={}/lib", out_dir);
        println!("cargo:rustc-link-lib=dylib=datachannel");
    }

    let bindings = bindgen::Builder::default()
        .header("libdatachannel/include/rtc/rtc.h")
        .generate()
        .expect("Unable to generate bindings");

    let out_path = PathBuf::from(out_dir);
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings");
}
