use std::collections::HashSet;
use std::{mem, slice, str};

use tree_sitter_language::LanguageFn;
use tree_sitter_runtime::{Language, Parser};

use crate::{
    LANGUAGE_COUNT, LanguageDescriptor, RawSlice, nmt_tree_sitter_abi_version,
    nmt_tree_sitter_language, nmt_tree_sitter_language_count,
};

fn text(value: RawSlice) -> &'static str {
    if value.len == 0 {
        return "";
    }

    // Descriptors expose static strings owned by this crate, so the pointer is
    // valid for the process lifetime and contains exactly `len` UTF-8 bytes.
    unsafe { str::from_utf8(slice::from_raw_parts(value.data, value.len)).unwrap() }
}

#[test]
fn every_exported_language_has_a_unique_name_and_usable_parser() {
    assert_eq!(nmt_tree_sitter_abi_version(), 1);
    assert_eq!(nmt_tree_sitter_language_count(), LANGUAGE_COUNT);

    let mut names = HashSet::new();
    for index in 0..LANGUAGE_COUNT {
        let mut raw = mem::MaybeUninit::<LanguageDescriptor>::uninit();
        // The output points to aligned storage for the exact exported type.
        assert_eq!(
            unsafe { nmt_tree_sitter_language(index, raw.as_mut_ptr()) },
            1
        );
        // A successful call initializes the complete descriptor.
        let raw = unsafe { raw.assume_init() };

        let name = text(raw.name);
        assert!(names.insert(name), "duplicate language {name}");

        let builder = raw.language.expect("language builder");
        // The builder comes from a linked grammar crate and returns its static
        // parser tables using the Tree-sitter language ABI.
        let language = Language::new(unsafe { LanguageFn::from_raw(builder) });
        Parser::new().set_language(&language).unwrap();
    }

    let mut unused = mem::MaybeUninit::<LanguageDescriptor>::uninit();
    // An out-of-range index must leave caller storage untouched.
    assert_eq!(
        unsafe { nmt_tree_sitter_language(LANGUAGE_COUNT, unused.as_mut_ptr()) },
        0
    );
}
