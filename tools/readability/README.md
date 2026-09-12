# Rust readability checks

Run these commands from the repository root:

```sh
cargo readability --check
cargo readability --check crates/terminal/src
cargo readability --fix crates/terminal/src/presentation/mod.rs
cargo readability --staged
```

`--check` reports missing blank lines and declaration organization issues.
`--fix` adds blank lines to working files without staging anything. Declaration
position, order, visibility, and attribute issues remain errors until corrected manually.
Explicit paths may name files or directories. Without paths, both modes inspect
tracked and unignored Rust sources under `crates/`.

The pre-commit hook runs `--staged`. It reads index blobs, so unstaged fixes cannot
hide staged problems and unstaged mistakes cannot fail this check. Only boundaries
next to added or changed lines, or joined by a deletion, can block the commit.
Existing issues elsewhere in the same file are left for an explicit full check.
Declaration issues are checked when the declaration or the earlier item that
causes the violation intersects a changed boundary. Changes inside a function
body do not expose an old declaration below that function.
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
arms stay compact within their groups. Existing blank lines are accepted.
Comments remain attached to the following statement; boundaries inside trailing
block comments are skipped.
Function, method, module, and other item bodies marked `#[rustfmt::skip]` are
excluded, allowing deliberately compact code to retain its layout.

### Module documentation

A module's `//!` documentation must be separated from the following code or
attributes by a blank line. This applies at the beginning of a file and inside
inline modules. Consecutive documentation lines stay together, and modules
containing only documentation need no trailing blank line.

The diagnostic is `spacing/module-docs`. `--fix` inserts the missing blank line
without changing documentation text or the attributes on the following item.
Outer item documentation (`///`) remains attached to its item.

### Module and import declarations

Every file and inline module must place its module-level `mod` and `use`
declarations before other items, except for inline test modules described below,
in this order:

```rust
pub use crate::api::Public;

pub(crate) use crate::api::Internal;

pub(super) use crate::api::Parent;

pub mod api;

pub(crate) mod shared;

pub(super) mod sibling;

mod private;

#[cfg(test)]
mod tests;

use std::fmt;
```

Each change of declaration group requires a blank line. Groups may be omitted;
names within a group do not need sorting, and extra blank lines are accepted.
Attributes and documentation stay attached to their declarations. File-level
documentation and inner attributes may precede the header. Inline modules whose
names do not contain `test` count as `mod` declarations at their enclosing level.
Every inline module has its own declaration header.

Bodyless module declarations whose names contain the lowercase substring `test`,
such as `mod tests;`, `mod parser_tests;`, and `mod test_support;`, form one group
regardless of visibility.
Place this group after every other `mod` group and before plain `use` imports,
with blank lines at the group boundaries. Each matching module must directly
carry `#[cfg(test)]`, including declarations nested in a test module. An
inherited condition, `cfg_attr`, or a compound condition such as
`#[cfg(all(test, windows))]` does not replace that explicit attribute; other
conditions can be added as separate attributes.

The exact module name `test_support` is exempt from the `#[cfg(test)]`
requirement so shared helpers can remain available through a feature flag.
It still belongs to the final module group and follows the same spacing rules.

Inline modules with `test` in their names, such as `mod tests { ... }`, are
treated as body items. They are exempt from the enclosing header order, test
group spacing, and explicit `#[cfg(test)]` requirement, so they can remain at
the end of the file. Their visibility and the declarations inside them are
still checked. This allows Clippy's `items_after_test_module` lint to remain
enabled.

`pub(super) use` and `pub(super) mod` each form a separate group immediately after
the corresponding `pub(crate)` group. Header position, order, grouping, and test
module requirements apply to module-level `mod` and `use` declarations. Function,
method, closure, and other block-local declarations are exempt from those rules,
including modules nested inside those blocks.

`pub(in ...)` and `pub(self)` are forbidden on every parsed declaration with
visibility, including functions, methods, types, traits, fields, constants,
statics, associated items, foreign items, modules, and imports. These checks also
apply inside local blocks and items marked `#[rustfmt::skip]`. The
`declarations/visibility` diagnostic suggests `pub(crate)` for `pub(in ...)` and
omitting visibility for `pub(self)`. Private declarations, `pub(super)`,
`pub(crate)`, and `pub` remain allowed. Macro input is not expanded or inspected
for visibility.

Module file overrides using `#[path = ...]` are allowed only on modules that
directly carry `#[cfg(test)]`. This exception also permits paths inside
`cfg_attr` on the same module. An inherited test condition, `cfg_attr` adding
`cfg(test)`, or a compound condition such as `#[cfg(all(test, unix))]` does not
grant the exception. Nested modules need their own direct `#[cfg(test)]`.
Visibility and other declaration rules still apply to these test modules.

Other declarations must use the standard module file layout. The
`declarations/path-attribute` diagnostic covers paths inside nested `cfg_attr`
conditions, function-local modules, and items marked `#[rustfmt::skip]`.
Removing a module's test condition is checked by `--staged` even when other
attributes separate it from the path attribute. `--fix` reports forbidden
attributes without removing them or moving files.

The diagnostic rules are `declarations/header`, `declarations/order`,
`declarations/visibility`, `declarations/test-module-cfg`,
`declarations/path-attribute`, and
`spacing/declaration-groups`. Different groups placed on the same line produce
`declarations/group-spacing` and must be split
manually. `--fix` only inserts blank lines: moving declarations can affect macro
scope, broadening visibility changes which callers can access an item, and
adding `#[cfg(test)]` excludes that module from normal builds.
After fixing spacing, remaining declaration issues still produce exit status `1`.
`#[rustfmt::skip]` retains its spacing exemption inside item bodies; module-level
declaration position, order, and test-module attributes are still checked, as are
visibility and module file overrides on all parsed declarations.

The tool parses source without expanding macros or evaluating configuration, so
platform-specific Rust is covered. It recognizes braced `select!` handlers and
`bitflags!` constants; other macro input is left alone. Strings, comments, and
code placed on a single line are not reformatted. Use rustfmt for conventional
Rust layout. The tool does not enforce function length or nesting depth.

Fixes preserve existing bytes and newline style, adding only blank lines. Before
replacing a file, the tool reparses it and compares token kinds, groups,
punctuation, and literal values. A syntax error or token change stops that file
from being written. Replacement uses a temporary file in the same directory.

Exit status is `0` when clean or successfully fixed, `1` for remaining readability
issues, and `2` for invalid arguments, syntax errors, or tool failures. The first invocation
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
