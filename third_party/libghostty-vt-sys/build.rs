use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Pinned ghostty commit. Update this to pull a newer version.
const GHOSTTY_REPO: &str = "https://github.com/ghostty-org/ghostty.git";
const GHOSTTY_COMMIT: &str = "53bd14fecfd68c6c0ab64d37b5943247299e2b40";

/// Identifier for the locally-applied ghostty source patches. Bump this whenever
/// [`patch_ghostty_source`] changes so a cached clone is re-fetched and re-patched.
/// Folded into the fetch stamp alongside `GHOSTTY_COMMIT`.
const GHOSTTY_PATCH_VERSION: &str =
    "win-reflow-trim-styled-v6-grow-cursor-y-v2-kitty-screen-pos-v2-blockset-v4-base53bd14f";
const PREBUILT_ENV: &str = "NMT_USE_PREBUILT_LIBGHOSTTY";

/// Locate an LLVM binutils tool (`llvm-objcopy` / `llvm-nm`) on Windows.
///
/// Search order: `LLVM_OBJCOPY`/`LLVM_NM` env override -> `PATH` -> the LLVM
/// component bundled with a Visual Studio install. Panics if not found — the
/// Windows static link rewrites ghostty-vt's vendored simdutf symbols (see
/// [`localize_simdutf_lib`]) and cannot proceed without it.
fn find_llvm_tool(tool: &str) -> PathBuf {
    let env_key = tool.to_uppercase().replace('-', "_"); // llvm-objcopy -> LLVM_OBJCOPY
    if let Ok(p) = env::var(&env_key) {
        let pb = PathBuf::from(&p);
        assert!(pb.exists(), "{env_key}={p} does not exist");
        return pb;
    }
    if Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return PathBuf::from(tool);
    }
    for base in [
        "C:/Program Files/Microsoft Visual Studio",
        "C:/Program Files (x86)/Microsoft Visual Studio",
    ] {
        let Ok(years) = std::fs::read_dir(base) else {
            continue;
        };
        for year in years.flatten() {
            let Ok(editions) = std::fs::read_dir(year.path()) else {
                continue;
            };
            for edition in editions.flatten() {
                let cand = edition
                    .path()
                    .join("VC/Tools/Llvm/x64/bin")
                    .join(format!("{tool}.exe"));
                if cand.exists() {
                    return cand;
                }
            }
        }
    }
    panic!(
        "could not find `{tool}`. The Windows static link rewrites ghostty-vt's \
         vendored simdutf symbols and needs LLVM binutils. Install the \"C++ Clang \
         tools for Windows\" VS component, or set {env_key}=<path to {tool}.exe>."
    );
}

/// Rewrite ghostty-vt's vendored `simdutf::*` symbols to a `gvt_`-prefixed
/// namespace inside a copy of `lib_file` placed in `OUT_DIR`, returning the
/// directory holding the rewritten library.
///
/// terminal links the Rust `simdutf` crate, and ghostty-vt bundles its own
/// simdutf; both export the same C++ symbols, which multiply-define in a cdylib
/// link (LNK1169). Renaming ghostty's copy (definitions *and* the cross-object
/// refs in `vt.obj`/`base64.obj`) keeps ghostty self-consistent while leaving the
/// Rust crate as the only plain `simdutf`, so a single static link has no
/// collision and needs no extra runtime DLL. Panics on any failure (missing
/// tools, missing library, objcopy error) — there is no silent fallback.
fn localize_simdutf_lib(search_dirs: &[PathBuf], lib_file: &str) -> PathBuf {
    let src = search_dirs
        .iter()
        .map(|d| d.join(lib_file))
        .find(|p| p.exists())
        .unwrap_or_else(|| {
            panic!("expected {lib_file} under one of {search_dirs:?} for simdutf rewrite")
        });

    let nm = find_llvm_tool("llvm-nm");
    let objcopy = find_llvm_tool("llvm-objcopy");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR set by cargo"));
    let dst_dir = out_dir.join("simdutf-localized");
    std::fs::create_dir_all(&dst_dir).expect("create simdutf-localized dir");
    let dst = dst_dir.join(lib_file);
    std::fs::copy(&src, &dst)
        .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", src.display(), dst.display()));

    let nm_out = Command::new(&nm)
        .arg(&dst)
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", nm.display()));
    assert!(
        nm_out.status.success(),
        "llvm-nm failed on {}",
        dst.display()
    );
    let listing = String::from_utf8_lossy(&nm_out.stdout);
    let mut syms = std::collections::BTreeSet::new();
    for line in listing.lines() {
        let Some(sym) = line.split_whitespace().last() else {
            continue;
        };
        // Mangled C++ symbols contain `simdutf` but no path separators; skip
        // archive-member header tokens (object/library file paths).
        if sym.contains("simdutf")
            && !sym.contains('\\')
            && !sym.contains('/')
            && !sym.ends_with(".obj")
            && !sym.ends_with(".lib")
        {
            syms.insert(sym.to_string());
        }
    }
    assert!(
        !syms.is_empty(),
        "no simdutf symbols found in {} — ghostty-vt layout changed; revisit the rewrite",
        dst.display()
    );

    let map_path = dst_dir.join(format!("{lib_file}.redefine.txt"));
    let mut map = String::with_capacity(syms.len() * 64);
    for s in &syms {
        map.push_str(s);
        map.push(' ');
        map.push_str("gvt_");
        map.push_str(s);
        map.push('\n');
    }
    std::fs::write(&map_path, map).expect("write redefine map");

    // Rewrite per member, not whole-archive: zig's COFF writer emits the big
    // zig-compilation-unit object (`*_zcu.obj`) in a shape LLVM's COFF reader
    // rejects ("SymbolTableIndex out of range"), but that member carries no
    // simdutf symbols — only the simdutf/vt/base64 objects do. Extract the
    // archive, objcopy only the members that actually mention a mapped
    // symbol, and re-archive; members we don't touch are never parsed by
    // objcopy at all.
    let ar = find_llvm_tool("llvm-ar");
    let members_dir = dst_dir.join(format!("{lib_file}.members"));
    if members_dir.exists() {
        std::fs::remove_dir_all(&members_dir).expect("clear members dir");
    }
    std::fs::create_dir_all(&members_dir).expect("create members dir");

    let order_out = Command::new(&ar)
        .arg("t")
        .arg(&dst)
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", ar.display()));
    assert!(
        order_out.status.success(),
        "llvm-ar t failed on {}",
        dst.display()
    );
    let member_order: Vec<String> = String::from_utf8_lossy(&order_out.stdout)
        .lines()
        .map(|l| {
            Path::new(l.trim())
                .file_name()
                .expect("archive member has a file name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    {
        let unique: std::collections::BTreeSet<&String> = member_order.iter().collect();
        assert_eq!(
            unique.len(),
            member_order.len(),
            "duplicate member basenames in {} — per-member rewrite needs unique names",
            dst.display()
        );
    }

    let extract = Command::new(&ar)
        .arg("x")
        .arg(&dst)
        .current_dir(&members_dir)
        .status()
        .unwrap_or_else(|e| panic!("run {}: {e}", ar.display()));
    assert!(extract.success(), "llvm-ar x failed on {}", dst.display());

    for member in &member_order {
        let member_path = members_dir.join(member);
        let nm_member = Command::new(&nm)
            .arg(&member_path)
            .output()
            .unwrap_or_else(|e| panic!("run {}: {e}", nm.display()));
        let needs_rewrite = nm_member.status.success()
            && String::from_utf8_lossy(&nm_member.stdout)
                .lines()
                .filter_map(|l| l.split_whitespace().last())
                .any(|sym| syms.contains(sym));
        if !needs_rewrite {
            continue;
        }
        let status = Command::new(&objcopy)
            .arg(format!("--redefine-syms={}", map_path.display()))
            .arg(&member_path)
            .status()
            .unwrap_or_else(|e| panic!("run {}: {e}", objcopy.display()));
        assert!(
            status.success(),
            "llvm-objcopy --redefine-syms failed on member {member} of {lib_file}"
        );
    }

    std::fs::remove_file(&dst).expect("remove pre-rewrite archive");
    let mut rebuild = Command::new(&ar);
    rebuild.arg("rcs").arg(&dst).current_dir(&members_dir);
    for member in &member_order {
        rebuild.arg(member);
    }
    let status = rebuild
        .status()
        .unwrap_or_else(|e| panic!("run {}: {e}", ar.display()));
    assert!(
        status.success(),
        "llvm-ar rcs failed rebuilding {}",
        dst.display()
    );
    std::fs::remove_dir_all(&members_dir).ok();

    println!("cargo:rerun-if-env-changed=LLVM_OBJCOPY");
    println!("cargo:rerun-if-env-changed=LLVM_NM");
    dst_dir
}

#[derive(Clone, Copy)]
enum LinkMode {
    Dynamic,
    Static,
}

impl LinkMode {
    fn current() -> Self {
        if cfg!(feature = "link-dynamic") {
            Self::Dynamic
        } else {
            Self::Static
        }
    }

    fn artifact_kind(self) -> &'static str {
        match self {
            Self::Dynamic => "shared library",
            Self::Static => "static library",
        }
    }

    fn matches_library(self, target: &str, file_name: &str) -> bool {
        match self {
            Self::Dynamic => {
                if target.contains("darwin") {
                    file_name.starts_with("libghostty-vt") && file_name.ends_with(".dylib")
                } else if target.contains("windows") {
                    file_name == "ghostty-vt.lib"
                        || file_name == "ghostty-vt.dll"
                        || file_name == "libghostty-vt.dll.lib"
                        || file_name == "libghostty-vt.dll.a"
                } else {
                    file_name == "libghostty-vt.so" || file_name.starts_with("libghostty-vt.so.")
                }
            }
            Self::Static => {
                if target.contains("windows") {
                    file_name == "ghostty-vt-static.lib"
                } else {
                    file_name == "libghostty-vt.a"
                }
            }
        }
    }

    #[cfg(feature = "pkg-config")]
    fn pkg_config_name(self) -> &'static str {
        match self {
            Self::Dynamic => "libghostty-vt",
            Self::Static => "libghostty-vt-static",
        }
    }
}

fn main() {
    // docs.rs has no Zig toolchain. The checked-in bindings in src/bindings.rs
    // are enough for generating documentation, so skip the entire native
    // build when running under docs.rs.
    if env::var("DOCS_RS").is_ok() {
        return;
    }

    let link_mode = LinkMode::current();

    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_SYS_OPTIMIZE");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_INSTALL_DIR");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_STATIC_DEPS_DIR");
    println!("cargo:rerun-if-env-changed=GHOSTTY_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=GHOSTTY_ZIG_SYSTEM_DIR");
    println!("cargo:rerun-if-env-changed={PREBUILT_ENV}");
    println!("cargo:rerun-if-env-changed=TARGET");
    println!("cargo:rerun-if-env-changed=HOST");
    println!("cargo:rerun-if-env-changed=OPT_LEVEL");
    println!("cargo:rerun-if-changed=build.rs");

    if let Ok(dir) = env::var("LIBGHOSTTY_VT_INSTALL_DIR") {
        assert!(
            !dir.is_empty(),
            "LIBGHOSTTY_VT_INSTALL_DIR must not be empty when set"
        );
        link_install_prefix(link_mode, PathBuf::from(dir));
        return;
    }

    // An explicit source override should stay authoritative even when the
    // pkg-config feature is enabled, so local Ghostty checkouts remain easy to
    // test against.
    if env::var_os("GHOSTTY_SOURCE_DIR").is_some() {
        build_vendored(link_mode);
        return;
    }

    if should_link_prebuilt(env_flag_enabled(PREBUILT_ENV)) {
        link_prebuilt(link_mode);
        return;
    }

    // When the pkg-config feature is enabled, prefer an installed library over
    // fetching Ghostty. libghostty is pre-1.0, so this crate intentionally does
    // not promise compatibility with every installed C API revision.
    #[cfg(feature = "pkg-config")]
    if try_pkg_config(link_mode) {
        return;
    }

    build_vendored(link_mode);
}

fn env_flag_enabled(name: &str) -> bool {
    env::var(name).is_ok_and(|value| env_flag_value_enabled(&value))
}

fn env_flag_value_enabled(value: &str) -> bool {
    !matches!(
        value,
        "" | "0" | "false" | "False" | "FALSE" | "no" | "No" | "NO" | "off" | "Off" | "OFF"
    )
}

fn should_link_prebuilt(use_prebuilt: bool) -> bool {
    use_prebuilt
}

/// Build libghostty-vt from source via zig. The zig build itself generates
/// shared and static artifacts plus pkg-config files in `share/pkgconfig/`.
fn build_vendored(link_mode: LinkMode) {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR must be set"));
    let target = env::var("TARGET").expect("TARGET must be set");
    let host = env::var("HOST").expect("HOST must be set");

    // Locate ghostty source: env override > fetch into OUT_DIR.
    let ghostty_dir = match env::var("GHOSTTY_SOURCE_DIR") {
        Ok(dir) => {
            let p = PathBuf::from(dir);
            assert!(
                p.join("build.zig").exists(),
                "GHOSTTY_SOURCE_DIR does not contain build.zig: {}",
                p.display()
            );
            p
        }
        Err(_) => fetch_ghostty(&out_dir),
    };

    // Build libghostty-vt via zig.
    let install_prefix = out_dir.join("ghostty-install");
    let zig_cache_dir = out_dir.join("zig-cache");
    let zig_global_cache_dir = out_dir.join("zig-global-cache");

    let optimize = zig_optimize_mode();

    let mut build = Command::new("zig");
    // Zig's std.http proxy support mangles CONNECT-style HTTPS proxying (the
    // dep CDN answers 400 through a local proxy while direct fetches succeed),
    // so package fetching must bypass any ambient proxy configuration.
    build
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .arg("build")
        .arg("-Demit-lib-vt")
        .arg(format!("-Doptimize={optimize}"))
        .arg("-Demit-xcframework=false")
        .arg("-Dapp-runtime=none")
        .arg("--prefix")
        .arg(&install_prefix)
        .arg("--cache-dir")
        .arg(&zig_cache_dir)
        .current_dir(&ghostty_dir);

    // Package managers can provide Ghostty's Zig package cache ahead of time
    // and ask Zig to resolve packages from that immutable store path instead
    // of fetching during this Cargo build script.
    if let Ok(dir) = env::var("GHOSTTY_ZIG_SYSTEM_DIR") {
        assert!(
            !dir.is_empty(),
            "GHOSTTY_ZIG_SYSTEM_DIR must not be empty when set"
        );
        let zig_system_dir = PathBuf::from(dir);
        assert!(
            zig_system_dir.exists(),
            "GHOSTTY_ZIG_SYSTEM_DIR does not exist: {}",
            zig_system_dir.display()
        );
        build
            .arg("--system")
            .arg(&zig_system_dir)
            .arg("--global-cache-dir")
            .arg(&zig_global_cache_dir);
    }

    // Only pass -Dtarget when cross-compiling. For native builds, let zig
    // auto-detect the host (matches how ghostty's own CMakeLists.txt works).
    if target != host {
        let zig_target = zig_target(&target);
        build.arg(format!("-Dtarget={zig_target}"));
    }

    run(build, "zig build");

    let lib_dir = install_prefix.join("lib");
    let include_dir = install_prefix.join("include");
    let search_dirs = library_search_dirs(&target, &install_prefix);
    warn_unused_xcframework(&lib_dir);

    let has_requested_library = search_dirs.iter().any(|dir| {
        std::fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", dir.display()))
            .any(|entry| {
                let entry = entry.unwrap_or_else(|error| {
                    panic!("failed to read entry from {}: {error}", dir.display())
                });
                let file_name = entry.file_name();
                let Some(file_name) = file_name.to_str() else {
                    return false;
                };

                link_mode.matches_library(&target, file_name)
            })
    });
    assert!(
        has_requested_library,
        "expected libghostty-vt {} in one of {:?}",
        link_mode.artifact_kind(),
        search_dirs
    );
    assert!(
        include_dir.join("ghostty").join("vt.h").exists(),
        "expected header at {}",
        include_dir.join("ghostty").join("vt.h").display()
    );

    emit_link_metadata(link_mode, &target, &search_dirs, true);
    emit_windows_static_dependency_links(link_mode, &target, &[zig_cache_dir.join("o")], true);
    emit_include_metadata(&[include_dir]);
}

fn prebuilt_install_prefix(target: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("prebuilt")
        .join(target)
}

fn link_prebuilt(link_mode: LinkMode) {
    assert!(
        matches!(link_mode, LinkMode::Static),
        "prebuilt libghostty-vt only supports the default static link mode; unset {PREBUILT_ENV} to build from source"
    );

    let target = env::var("TARGET").expect("TARGET must be set");
    let install_prefix = prebuilt_install_prefix(&target);
    println!("cargo:rerun-if-changed={}", install_prefix.display());
    assert!(
        install_prefix.exists(),
        "prebuilt libghostty-vt for {target} not found at {}; unset {PREBUILT_ENV} to build from source or generate the prebuilt package",
        install_prefix.display()
    );

    let static_deps_dir = install_prefix.join("lib");
    link_install_prefix_impl(link_mode, install_prefix, false, &[static_deps_dir]);
}

fn link_install_prefix(link_mode: LinkMode, install_prefix: PathBuf) {
    link_install_prefix_impl(link_mode, install_prefix, true, &[]);
}

fn link_install_prefix_impl(
    link_mode: LinkMode,
    install_prefix: PathBuf,
    localize_windows_simdutf: bool,
    extra_dependency_roots: &[PathBuf],
) {
    let target = env::var("TARGET").expect("TARGET must be set");
    let include_dir = install_prefix.join("include");
    let search_dirs = library_search_dirs(&target, &install_prefix);

    assert!(
        include_dir.join("ghostty").join("vt.h").exists(),
        "expected header at {}",
        include_dir.join("ghostty").join("vt.h").display()
    );
    assert!(
        search_dirs
            .iter()
            .any(|dir| has_matching_library(link_mode, &target, dir)),
        "expected libghostty-vt {} in one of {:?}",
        link_mode.artifact_kind(),
        search_dirs
    );

    let mut dependency_roots = Vec::new();
    if let Some(parent) = install_prefix.parent() {
        dependency_roots.push(parent.join(".zig-cache").join("o"));
    }
    dependency_roots.extend(extra_dependency_roots.iter().cloned());
    if let Ok(dir) = env::var("LIBGHOSTTY_VT_STATIC_DEPS_DIR") {
        dependency_roots.push(PathBuf::from(dir));
    }

    emit_link_metadata(link_mode, &target, &search_dirs, localize_windows_simdutf);
    emit_windows_static_dependency_links(
        link_mode,
        &target,
        &dependency_roots,
        localize_windows_simdutf,
    );
    emit_include_metadata(&[include_dir]);
}

fn emit_link_metadata(
    link_mode: LinkMode,
    target: &str,
    search_dirs: &[PathBuf],
    localize_windows_simdutf: bool,
) {
    // Windows static: link a simdutf-rewritten copy of the main archive (its
    // bundled simdutf.obj is the LNK1169 source against the Rust simdutf crate).
    // Emit the rewritten copy's dir FIRST so the linker prefers it over the
    // original under `search_dirs`.
    if localize_windows_simdutf
        && matches!(link_mode, LinkMode::Static)
        && target.contains("windows")
    {
        let localized = localize_simdutf_lib(search_dirs, "ghostty-vt-static.lib");
        println!("cargo:rustc-link-search=native={}", localized.display());
    }
    for dir in search_dirs {
        println!("cargo:rustc-link-search=native={}", dir.display());
    }
    match link_mode {
        LinkMode::Dynamic => println!("cargo:rustc-link-lib=dylib=ghostty-vt"),
        LinkMode::Static if target.contains("windows") => {
            println!("cargo:rustc-link-lib=static=ghostty-vt-static")
        }
        LinkMode::Static => println!("cargo:rustc-link-lib=static=ghostty-vt"),
    }
}

fn has_matching_library(link_mode: LinkMode, target: &str, dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", dir.display()))
        .any(|entry| {
            let entry = entry.unwrap_or_else(|error| {
                panic!("failed to read entry from {}: {error}", dir.display())
            });
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                return false;
            };

            link_mode.matches_library(target, file_name)
        })
}

fn emit_windows_static_dependency_links(
    link_mode: LinkMode,
    target: &str,
    roots: &[PathBuf],
    localize_windows_simdutf: bool,
) {
    if !matches!(link_mode, LinkMode::Static) || !target.contains("windows") {
        return;
    }

    for dependency in ["simdutf", "highway"] {
        let library =
            find_newest_library(roots, &format!("{dependency}.lib")).unwrap_or_else(|| {
                panic!(
                    "expected {dependency}.lib for static Windows linking under one of {roots:?}"
                )
            });
        let library_dir = library
            .parent()
            .unwrap_or_else(|| panic!("{} has no parent directory", library.display()));
        // Rewrite the standalone simdutf archive to the same `gvt_` namespace as
        // the bundled copy in ghostty-vt-static.lib, and search the rewritten
        // copy first, so no plain `simdutf` symbol reaches the final link.
        if dependency == "simdutf" && localize_windows_simdutf {
            let localized = localize_simdutf_lib(
                std::slice::from_ref(&library_dir.to_path_buf()),
                "simdutf.lib",
            );
            println!("cargo:rustc-link-search=native={}", localized.display());
        }
        println!("cargo:rustc-link-search=native={}", library_dir.display());
        println!("cargo:rustc-link-lib=static={dependency}");
    }
}

fn find_newest_library(roots: &[PathBuf], file_name: &str) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for root in roots.iter().filter(|root| root.exists()) {
        find_library_recursive(root, file_name, &mut newest);
    }
    newest.map(|(_, path)| path)
}

fn find_library_recursive(
    dir: &Path,
    file_name: &str,
    newest: &mut Option<(std::time::SystemTime, PathBuf)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            find_library_recursive(&path, file_name, newest);
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) != Some(file_name) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if newest
            .as_ref()
            .is_none_or(|(current, _)| modified > *current)
        {
            *newest = Some((modified, path));
        }
    }
}

fn warn_unused_xcframework(lib_dir: &Path) {
    let xcframework = lib_dir.join("ghostty-vt.xcframework");
    if xcframework.exists() {
        println!(
            "cargo:warning=unused libghostty-vt XCFramework emitted at {}; Cargo links the dylib or archive directly",
            xcframework.display()
        );
    }
}

#[cfg(feature = "pkg-config")]
fn try_pkg_config(link_mode: LinkMode) -> bool {
    let mut config = pkg_config::Config::new();
    let lib = match link_mode {
        LinkMode::Dynamic => config.probe(link_mode.pkg_config_name()),
        LinkMode::Static => config
            .statik(true)
            .cargo_metadata(false)
            .probe(link_mode.pkg_config_name()),
    };
    let lib = match lib {
        Ok(lib) => lib,
        Err(_) => return false,
    };

    if let LinkMode::Static = link_mode {
        emit_static_pkg_config_metadata(&lib);
    }
    emit_include_metadata(&lib.include_paths);
    true
}

#[cfg(feature = "pkg-config")]
fn emit_static_pkg_config_metadata(lib: &pkg_config::Library) {
    for path in &lib.link_paths {
        println!("cargo:rustc-link-search=native={}", path.display());
    }
    for path in &lib.link_files {
        if let Some(parent) = path.parent() {
            println!("cargo:rustc-link-search=native={}", parent.display());
        }
    }
    for path in &lib.framework_paths {
        println!("cargo:rustc-link-search=framework={}", path.display());
    }
    for framework in &lib.frameworks {
        println!("cargo:rustc-link-lib=framework={framework}");
    }

    println!("cargo:rustc-link-lib=static=ghostty-vt");
    for library in &lib.libs {
        if library != "ghostty-vt" {
            println!("cargo:rustc-link-lib={library}");
        }
    }
    for args in &lib.ld_args {
        if !args.is_empty() {
            println!("cargo:rustc-link-arg=-Wl,{}", args.join(","));
        }
    }
}

fn emit_include_metadata(include_paths: &[PathBuf]) {
    if include_paths.is_empty() {
        return;
    }

    let joined = env::join_paths(include_paths)
        .unwrap_or_else(|error| panic!("failed to join include paths for cargo metadata: {error}"));
    println!("cargo:include={}", joined.to_string_lossy());
}

/// Decide which Zig `OptimizeMode` to pass to `zig build`.
///
/// The `LIBGHOSTTY_VT_SYS_OPTIMIZE` environment variable overrides this unconditionally; accepted
/// values are the four Zig `OptimizeMode` names (`Debug`, `ReleaseSafe`, `ReleaseFast`,
/// `ReleaseSmall`).
///
/// Defaults to `ReleaseFast` for optimized builds. If `OPT_LEVEL` is `0` (the `dev` profile),
/// `Debug` mode is used; `s`/`z` map to `ReleaseSmall`. The decision keys off `OPT_LEVEL`
/// because cargo's `DEBUG` env var reflects debug-*info* — this workspace ships
/// `[profile.release] debug = "full"`, and keying off `DEBUG` compiled the VT engine
/// unoptimized in release builds (a ~2000× slower parse path).
fn zig_optimize_mode() -> &'static str {
    if let Ok(override_mode) = env::var("LIBGHOSTTY_VT_SYS_OPTIMIZE") {
        return match override_mode.as_str() {
            "Debug" => "Debug",
            "ReleaseSafe" => "ReleaseSafe",
            "ReleaseFast" => "ReleaseFast",
            "ReleaseSmall" => "ReleaseSmall",
            other => panic!(
                "LIBGHOSTTY_VT_SYS_OPTIMIZE must be one of Debug, ReleaseSafe, ReleaseFast, ReleaseSmall (got '{other}')"
            ),
        };
    }

    match env::var("OPT_LEVEL").as_deref() {
        // Windows: never Zig Debug. Zig 0.15's self-hosted x86_64 backend
        // (the Debug-mode default) emits a COFF for the grown ghostty zcu
        // object that every MSVC-side reader rejects (llvm-objcopy
        // "SymbolTableIndex out of range"; lib.exe/dumpbin LNK1106 seek past
        // EOF), so the archive cannot be indexed or linked at all.
        // ReleaseSafe keeps assertions while forcing the LLVM backend.
        // Revisit when a Zig release fixes the self-hosted COFF writer.
        Ok("0") if env::var("TARGET").is_ok_and(|t| t.contains("windows")) => "ReleaseSafe",
        Ok("0") => "Debug",
        Ok("s") | Ok("z") => "ReleaseSmall",
        _ => "ReleaseFast",
    }
}

/// Clone ghostty at the pinned commit into OUT_DIR/ghostty-src.
/// Reuses an existing clone if the commit matches.
fn fetch_ghostty(out_dir: &Path) -> PathBuf {
    let src_dir = out_dir.join("ghostty-src");
    let stamp = src_dir.join(".ghostty-commit");
    // The stamp couples the upstream commit with the local patch revision so that
    // bumping either re-fetches and re-patches a clean tree.
    let stamp_id = format!("{GHOSTTY_COMMIT}:{GHOSTTY_PATCH_VERSION}");

    // Skip fetch if we already have the right commit + patch revision.
    if stamp.exists()
        && let Ok(existing) = std::fs::read_to_string(&stamp)
        && existing.trim() == stamp_id
    {
        return src_dir;
    }

    // Clean and clone fresh.
    if src_dir.exists() {
        std::fs::remove_dir_all(&src_dir)
            .unwrap_or_else(|e| panic!("failed to remove {}: {e}", src_dir.display()));
    }

    eprintln!("Fetching ghostty {GHOSTTY_COMMIT} ...");

    let mut clone = Command::new("git");
    clone
        .arg("clone")
        .arg("--filter=blob:none")
        .arg("--no-checkout")
        .arg(GHOSTTY_REPO)
        .arg(&src_dir);
    run(clone, "git clone ghostty");

    let mut checkout = Command::new("git");
    checkout
        .arg("checkout")
        .arg(GHOSTTY_COMMIT)
        .current_dir(&src_dir);
    run(checkout, "git checkout ghostty commit");

    patch_ghostty_source(&src_dir);

    std::fs::write(&stamp, &stamp_id).unwrap_or_else(|e| panic!("failed to write stamp: {e}"));

    src_dir
}

/// Apply the sorted local patches to a fresh Ghostty checkout via `git apply`.
/// Platform-specific changes self-gate in Zig so one patch series can build on
/// every supported target.
///
/// A patch that no longer applies (after a `GHOSTTY_COMMIT` bump) fails the build with
/// a clear message; regenerate it and bump `GHOSTTY_PATCH_VERSION`.
fn patch_ghostty_source(src_dir: &Path) {
    let patch_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("patches");
    println!("cargo:rerun-if-changed={}", patch_dir.display());

    let mut patches: Vec<PathBuf> = std::fs::read_dir(&patch_dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", patch_dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("patch"))
        .collect();
    patches.sort();

    for patch in &patches {
        println!("cargo:rerun-if-changed={}", patch.display());
        let mut apply = Command::new("git");
        apply
            .arg("apply")
            .arg("--whitespace=nowarn")
            .arg(patch)
            .current_dir(src_dir);
        run(apply, &format!("git apply {}", patch.display()));
    }
}

fn run(mut command: Command, context: &str) {
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("failed to execute {context}: {error}"));
    assert!(status.success(), "{context} failed with status {status}");
}

/// Returns directories to search for the built library artifact.
/// On Windows, Zig may place the DLL in `bin/` and the import lib in `lib/`,
/// so both are included.
fn library_search_dirs(target: &str, install_prefix: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![install_prefix.join("lib")];
    if target.contains("windows") {
        dirs.push(install_prefix.join("bin"));
    }
    dirs
}

fn zig_target(target: &str) -> String {
    let value = match target {
        "x86_64-unknown-linux-gnu" => "x86_64-linux-gnu",
        "x86_64-unknown-linux-musl" => "x86_64-linux-musl",
        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",
        "aarch64-unknown-linux-musl" => "aarch64-linux-musl",
        "aarch64-apple-darwin" => "aarch64-macos-none",
        "x86_64-apple-darwin" => "x86_64-macos-none",
        "x86_64-pc-windows-gnu" => "x86_64-windows-gnu",
        "aarch64-pc-windows-gnullvm" => "aarch64-windows-gnu",
        "x86_64-pc-windows-msvc" => "x86_64-windows-msvc",
        "aarch64-pc-windows-msvc" => "aarch64-windows-msvc",
        other => panic!("unsupported Rust target for vendored build: {other}"),
    };
    value.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_flag_value_accepts_common_false_values() {
        for value in ["", "0", "false", "False", "FALSE", "no", "off"] {
            assert!(!env_flag_value_enabled(value), "{value:?}");
        }
        assert!(env_flag_value_enabled("1"));
        assert!(env_flag_value_enabled("true"));
    }

    #[test]
    fn prebuilt_env_name_is_contract() {
        assert_eq!(PREBUILT_ENV, "NMT_USE_PREBUILT_LIBGHOSTTY");
    }

    #[test]
    fn prebuilt_link_is_opt_in() {
        assert!(!should_link_prebuilt(false));
        assert!(should_link_prebuilt(true));
    }

    #[test]
    fn prebuilt_install_prefix_is_target_specific() {
        let prefix = prebuilt_install_prefix("x86_64-pc-windows-msvc");
        assert!(prefix.ends_with(Path::new("prebuilt").join("x86_64-pc-windows-msvc")));
    }
}
