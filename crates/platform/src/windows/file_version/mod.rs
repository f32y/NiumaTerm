//! Reading a named string out of a file's Windows version resource.
//!
//! Every binary this workspace links carries one, and an updater uses it to
//! decide whether a file on disk is already the one it was about to install.

use std::ffi::{OsStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::{ptr, slice};

#[cfg(test)]
mod tests;

use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};

/// One entry of `\VarFileInfo\Translation`, which is the language and code page
/// that the resource's string block is named after.
#[repr(C)]
#[derive(Clone, Copy)]
struct Translation {
    language: u16,
    code_page: u16,
}

/// The value of `key` in `path`'s version resource, or `None` when the file has
/// no version resource, no string block, or no such key in it.
///
/// Which string block holds the key is declared by the resource itself, so the
/// language and code page naming it are read from the translation table rather
/// than assumed to be the resource compiler's default: a file built by another
/// toolchain, such as a vendored Microsoft binary, names its block differently.
pub fn version_string(path: &Path, key: &str) -> Option<String> {
    let path = wide(path.as_os_str());
    let block = read_block(&path)?;

    translations(&block)?.iter().find_map(|translation| {
        string(
            &block,
            &format!(
                "\\StringFileInfo\\{:04x}{:04x}\\{key}",
                translation.language, translation.code_page
            ),
        )
    })
}

fn read_block(path: &[u16]) -> Option<Vec<u8>> {
    let mut unused_handle = 0u32;
    // SAFETY: `path` is NUL-terminated. The handle out-parameter is documented
    // as unused and is only required to be writable.
    let size = unsafe { GetFileVersionInfoSizeW(path.as_ptr(), &mut unused_handle) };

    if size == 0 {
        return None;
    }

    let mut block = vec![0u8; size as usize];
    // SAFETY: the buffer is exactly the size the call above asked for.
    let read = unsafe { GetFileVersionInfoW(path.as_ptr(), 0, size, block.as_mut_ptr().cast()) };

    (read != 0).then_some(block)
}

fn translations(block: &[u8]) -> Option<&[Translation]> {
    let (value, bytes) = query(block, "\\VarFileInfo\\Translation")?;
    let count = bytes as usize / size_of::<Translation>();

    // SAFETY: the pointer addresses `block`, whose borrow the returned slice
    // inherits, and the translation table's reported length is a byte count.
    (count > 0).then(|| unsafe { slice::from_raw_parts(value.cast::<Translation>(), count) })
}

fn string(block: &[u8], sub_block: &str) -> Option<String> {
    let (value, characters) = query(block, sub_block)?;

    // SAFETY: the pointer addresses `block`, which outlives this borrow. A
    // string value's reported length is a character count, not a byte count.
    let text = unsafe { slice::from_raw_parts(value.cast::<u16>(), characters as usize) };
    // That count includes the terminator, which must not become part of the
    // text; a value written without one is accepted as it stands.
    let text = text.strip_suffix(&[0]).unwrap_or(text);

    (!text.is_empty()).then(|| String::from_utf16_lossy(text))
}

/// The raw value `sub_block` names, and the length reported for it. The unit of
/// that length differs per sub-block, so the callers above interpret it. The
/// pointer addresses `block` and stays valid for as long as it does.
fn query(block: &[u8], sub_block: &str) -> Option<(*const c_void, u32)> {
    let sub_block = wide(OsStr::new(sub_block));
    let mut value = ptr::null_mut();
    let mut length = 0u32;

    // SAFETY: both wide strings are NUL-terminated, and `block` was filled by
    // GetFileVersionInfoW, which is what this call is documented to parse.
    let found = unsafe {
        VerQueryValueW(
            block.as_ptr().cast(),
            sub_block.as_ptr(),
            &mut value,
            &mut length,
        )
    };

    (found != 0 && !value.is_null() && length != 0).then_some((value.cast_const(), length))
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}
