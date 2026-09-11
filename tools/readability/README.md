# Rust readability checks

Run these commands from the repository root:

```sh
cargo readability --check
cargo readability --check crates/terminal/src
cargo readability --fix crates/terminal/src/presentation/mod.rs
cargo readability --staged
```

`--check` reports missing blank lines. `--fix` adds them to working files without
staging anything. Explicit paths may name files or directories. Without paths,
both modes inspect tracked and unignored Rust sources under `crates/`.

The pre-commit hook runs `--staged`. It reads index blobs, so unstaged fixes cannot
hide staged problems and unstaged mistakes cannot fail this check. Only boundaries
next to added or changed lines, or joined by a deletion, can block the commit.
Existing issues elsewhere in the same file are left for an explicit full check.
Added files and renamed destinations are checked in full. Deleted files and
sources outside `crates/` are excluded. The hook never rewrites or stages files.
The existing rustfmt and Clippy checks retain their working-tree behavior.

Output includes the path, line, column, and rule:

```text
crates/demo/src/lib.rs:3:1: spacing/binding-and-action: missing blank line
```

For example, this input:

```rust
let bytes = read()?;
send(bytes);
Ok(())
```

becomes:

```rust
let bytes = read()?;

send(bytes);

Ok(())
```

## Rules

The parser checks boundaries around control flow, bindings followed by actions,
multiline statements, returned values, and assertion groups. It also separates
items, multiline match arms, documented fields and enum variants, and UI
notification calls after other actions.

Consecutive short declarations, imports, assertions, and single-line mapping
arms stay compact. Existing blank lines are accepted. Comments remain attached
to the following statement; boundaries inside trailing block comments are skipped.
Function, method, module, and other item bodies marked `#[rustfmt::skip]` are
excluded, allowing deliberately compact code to retain its layout.

The tool parses source without expanding macros or evaluating configuration, so
platform-specific Rust is covered. It recognizes braced `select!` handlers and
`bitflags!` constants; other macro input is left alone. Strings, comments, and
code placed on a single line are not reformatted. Use rustfmt for conventional
Rust layout. This first version does not enforce function length, nesting depth,
or import and module organization.

Fixes preserve existing bytes and newline style, adding only blank lines. Before
replacing a file, the tool reparses it and compares token kinds, groups,
punctuation, and literal values. A syntax error or token change stops that file
from being written. Replacement uses a temporary file in the same directory.

Exit status is `0` when clean or successfully fixed, `1` for spacing issues, and
`2` for invalid arguments, syntax errors, or tool failures. The first invocation
builds a small standalone Rust tool; subsequent runs reuse it. Dependencies are
locked separately from the application workspace.

## Validation

```sh
cargo test --locked --manifest-path tools/readability/Cargo.toml
cargo fmt --manifest-path tools/readability/Cargo.toml -- --check
cargo clippy --locked --manifest-path tools/readability/Cargo.toml --all-targets -- -D warnings
sh .githooks/pre-commit-tests.sh
```

The integration tests use temporary repositories to exercise partial staging,
deleted blank lines, old issues outside changed boundaries, filenames with spaces,
renames, new repositories, and the separation between fixes and the index.
