# Rust readability checks

Run these commands from the repository root:

```sh
cargo readability --check
cargo readability --check crates/terminal/src
cargo readability --fix crates/terminal/src/presentation/mod.rs
cargo readability --staged
```

`--check` reports missing or unexpected blank lines, declaration organization issues,
and forbidden expression patterns.
`--fix` adds or removes blank lines in working files without staging anything. Declaration
position, order, visibility, attribute, and expression issues remain errors until
corrected manually.
Explicit paths may name files or directories. Without paths, both modes inspect
tracked and unignored Rust sources under `crates/`.

The pre-commit hook runs `--staged`. It reads index blobs, so unstaged fixes cannot
hide staged problems and unstaged mistakes cannot fail this check. Only boundaries
next to added or changed lines, or joined by a deletion, can block the commit.
Existing issues elsewhere in the same file are left for an explicit full check.
Declaration issues are checked when the declaration or the earlier item that
causes the violation intersects a changed boundary. Changes inside a function
body do not expose an old declaration below that function.
Expression issues are checked when the offending expression intersects a changed
boundary, including edits inside an immediately called closure's body.
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
items, documented fields, and UI notification calls after other actions.

Two rules are enabled by default in all modes, including the pre-commit check:

- `spacing/match-arm-blank-lines` forbids blank lines between adjacent match arms.
- `spacing/enum-variant-blank-lines` forbids blank lines between adjacent enum variants.

Both rules apply to single-line and multiline entries, including documented or
attributed entries. Each unwanted blank line produces an `unexpected blank line`
diagnostic. `--fix` removes these lines, including whitespace-only lines and blank
lines around intervening comments. Comment text, blank lines inside block comments,
and literal contents are preserved. Checks inside arm bodies and variant fields
still apply. These rules only inspect gaps between entries; they do not remove
blank lines before the first entry or after the last entry.

The optional `spacing/match-arms` and `spacing/enum-variants` rules remain disabled
by default. Enabling either explicitly replaces the corresponding default rule
for that invocation, so adding and removing blank lines never compete at the same
position. Repeat `--enable` to select both optional rules:

```sh
cargo readability --check --enable spacing/match-arms crates/terminal/src
cargo readability --fix --enable spacing/match-arms --enable spacing/enum-variants
```

When enabled, `spacing/match-arms` separates adjacent arms if either spans multiple
lines. `spacing/enum-variants` separates adjacent variants if either has documentation,
an `#[error(...)]` attribute, or fields spanning multiple lines. These options also
work with `--staged`.

Consecutive short declarations, imports, assertions, and single-line mapping
arms stay compact within their groups. Other spacing rules accept existing blank lines.
Comments remain attached to the following statement; boundaries inside trailing
block comments are skipped.
Function, method, module, and other item bodies marked `#[rustfmt::skip]` are
excluded, allowing deliberately compact code to retain its layout.

### Binding mutability

`spacing/binding-mutability` requires a blank line between adjacent `let` and
`let mut` declarations, in either order. Consecutive short declarations with the
same mutability can stay together:

```rust
let transcript = TranscriptIndex::read(reader);

let mut tasks: Vec<RestoredTask> = Vec::new();
let mut index: HashMap<String, usize> = HashMap::new();
```

Typed and destructured bindings are supported. A pattern containing any `mut`
binding, including `ref mut`, belongs to the mutable group. Mutable references
in a type, reference pattern, or initializer do not make the binding mutable.
The rule is enabled in all modes, including `--staged`; `--fix` inserts the
missing blank lines without reordering declarations.

### Calls and other statements

`spacing/call-and-statement` classifies standalone statements as function calls,
method calls, or non-call statements and requires a blank line when the kind
changes. Consecutive single-line function calls can stay together, as can
consecutive single-line method calls or simple assignments. Method calls share a
kind even when their
receivers differ, such as `self.update()` and `self.tasks.insert(key, value)`.
Existing blank lines between calls are preserved so authors can separate distinct
steps. Parentheses, `?`, and `.await` preserve the underlying call kind.
Associated calls such as `Store::flush(&mut store)` count as function calls.
An assignment whose right-hand side contains a call remains an assignment.
Macro invocations count as non-call statements.

For example, `--fix` separates function calls, method calls, and assignments:

```rust
merge_update(&mut summary, &update, sequence);

self.merge_update(&mut summary, &update, sequence);
self.tasks.insert(key, summary);

self.activity += 1;
```

Existing rules retain their diagnostics for bindings, control flow, assertions,
multiline statements, returned values, and UI notifications. Comments stay with
the following statement. The rule is enabled in all modes, including `--staged`.

### Assertion groups

`spacing/assertions` requires a blank line between an assertion and any other
kind of statement, in either order. Consecutive assertions can stay together,
including multiline assertions and a final assertion without a semicolon.
This covers macro names starting with `assert` or `debug_assert`, including
qualified names such as `std::assert_eq!` and calls wrapped in parentheses.
The rule is enabled in all modes, and `--fix` separates assertions from other
statements while preserving spacing inside assertion groups.

### Immediately called closures

`expressions/immediate-closure-call` forbids declaring an anonymous closure and
calling it directly in the same expression:

```rust
let result = (|| -> Result<(), String> {
    write_records()?;

    publish_file()
})();
```

The rule covers closures with parameters, `move` and `async` closures, and extra
parentheses around the callee. Ordinary callbacks, stored closures, and named
function calls remain allowed. Macro input is not expanded or inspected.
The check applies in all modes, including inside items marked `#[rustfmt::skip]`.

Extract the work into a named function. `--fix` reports this error without rewriting
the closure because changing its scope can change `?`, `return`, and capture behavior.
Expression issues still produce exit status `1` after spacing has been fixed.

### Fixed Option returns

`expressions/fixed-option-return` is disabled by default in every mode, including
the pre-commit check. Enable it explicitly with
`--enable expressions/fixed-option-return`. Trait implementations and platform
stubs can require an `Option` signature even when their return variant is fixed.

When enabled, the rule rejects functions and methods whose bodies end
directly in `Some(...)` or `None` and whose visible return paths only produce that
same variant. Explicit final `return` statements are also checked. The return
type must be spelled `Option<T>`, `std::option::Option<T>`, or
`core::option::Option<T>`; type aliases and custom qualified types are not resolved.

The rule checks early `return` statements and `?` in the function's own scope.
For example, `Some(home_dir()?.join("projects"))` remains allowed because `?` can
return `None`. A function ending in `None` with only other `None` returns, including
`?` propagation, still has a fixed return variant. Nested closures, async blocks,
and functions have separate return scopes. A `try` block captures `?` but does not
capture an explicit `return` from the enclosing function.

This is a conservative syntax check. Unknown return expressions, macro expansion,
and bodies ending in branching expressions are left alone. It does not prove
arbitrary control flow or resolve names and types. Explicit enablement works in
all modes, including inside `#[rustfmt::skip]` items. `--fix` reports the error without
changing the function signature, body, or callers.

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
declarations before other items, except for private inline modules and inline
test modules described below, in this order:

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

use serde::Serialize;

use crate::api::Internal;

mod implementation {
    // Private inline modules can also appear between other body items.
}
```

Each change of declaration group requires a blank line. Groups may be omitted;
module names within a group do not need sorting, and extra blank lines are accepted.
Attributes and documentation stay attached to their declarations. File-level
documentation and inner attributes may precede the header. Non-private inline
modules whose names do not contain `test` count as `mod` declarations at their
enclosing level. Every inline module has its own declaration header.

Private inline modules (`mod name { ... }`) are body items. They must follow
all private `use` imports, but may appear between functions, types, or other body
items. Bodyless private modules (`mod name;`) keep their place in the header
before private imports. An import after a private inline module produces
`declarations/header`.

Within each import visibility group, declarations must appear in this order,
with a blank line between source categories:

1. Standard library: paths starting with `std`, `core`, or `alloc`.
2. Third-party crates: paths starting with other crate names, including other
   workspace crates.
3. Current crate: paths starting with `crate` (`self` and `super` also belong to
   this category if present).

Source order restarts for each visibility group, so a `pub use crate::...` may
precede a private `use std::...`. Imports within each source category must also
be alphabetically ordered by path. Extra blank lines are accepted but do not
restart the alphabetical order. Attributes, aliases, leading `::`,
and braces do not change a path's category. A single declaration such as
`use {std::fmt, crate::api::Public};` must be split because it mixes categories.

Alphabetical checks also cover every nested braced import list. Ordering follows
Rust 2024 identifier sorting: underscores come first, uppercase letters precede
lowercase letters, and digit runs compare numerically (`item2` before `item10`).
Equal numeric values use their spelling to break ties (`item02` before `item2`).
Original paths determine order; aliases, leading `::`, and raw identifier prefixes
do not affect it. At the same path depth, `self`, `super`, and `crate` precede
ordinary names, named entries precede globs and braced lists, and shorter paths
precede their children. Identical paths may repeat under different attributes or
aliases. Comments and attributes stay attached to their declarations.

The diagnostics are `declarations/import-order`, `declarations/mixed-imports`,
`declarations/import-alphabetical`, and `spacing/import-groups`.
Different categories on one line produce
`declarations/group-spacing`. `--fix` inserts missing blank lines between
declarations; reordering or splitting imports requires manual edits.

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
manually. `--fix` only changes blank lines: moving declarations can affect macro
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

Fixes add or remove blank lines while preserving other bytes and newline style. Before
replacing a file, the tool reparses it and compares token kinds, groups,
punctuation, and literal values. A syntax error or token change stops that file
from being written. Replacement uses a temporary file in the same directory.

Exit status is `0` when clean or successfully fixed, `1` for remaining readability
issues, and `2` for invalid arguments, syntax errors, or tool failures. The first invocation
builds the `nmt-readability` workspace member; subsequent runs reuse it.

## Validation

```sh
cargo test --locked -p nmt-readability
cargo fmt -p nmt-readability -- --check
cargo clippy --locked -p nmt-readability --all-targets -- -D warnings
sh .githooks/pre-commit-tests.sh
```

The integration tests use temporary repositories to exercise partial staging,
deleted blank lines, old issues outside changed boundaries, filenames with spaces,
renames, new repositories, and the separation between fixes and the index.
