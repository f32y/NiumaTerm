# NiumaTerm

A high performance multi-tab, multi-workspace terminal application.

## Features

- Feature-rich terminal based on [rioterm](https://github.com/raphamorim/rio)
- High performance VT parser with [libghostty-vt](https://github.com/ghostty-org/ghostty) and [its Rust binding](https://github.com/uzaaft/libghostty-rs)
- GPU-accelerated UI with [Zed editor's GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)
- UI components backed by [gpui-component](https://github.com/longbridge/gpui-component)

## Build

Currently only Windows is supported.

> As libghostty-vt is written in Zig but most people do not have Zig toolchain installed on their machines,
> this repo bundles a prebuilt libghostty.a which is **opt-in** by default. If you don't want to build it by yourself,
> set `NMT_USE_PREBUILT_LIBGHOSTTY` environment variable to `1`.

```powershell
# PowerShell

$env:NMT_USE_PREBUILT_LIBGHOSTTY="1"
cargo run --bin NiumaTerm
```

If you want to build libghostty-vt on your machine:

1. Install Zig toolchain `v0.15.2` (https://ziglang.org/download/) and add zig compiler to your PATH environment variable.
2. Make sure both `llvm-objcopy` and `llvm-nm` are in your PATH environment variable because compiling libghostty-vt requires them. Currently libghostty-vt's Zig simdutf dependency conflicts with rioterm's Rust dependency. This problem will be solved in future version.
3. Perform a normal `cargo run --bin NiumaTerm`.

## Development

Setup git hooks before committing anything:

```
git config core.hooksPath .githooks
```
