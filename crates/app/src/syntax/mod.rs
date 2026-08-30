use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt as _;
use std::path::PathBuf;
use std::{io, mem, slice, str};

use anyhow::{Context as _, Result, anyhow, bail};
use gpui::SharedString;
use gpui_component::highlighter::{LanguageConfig, LanguageRegistry};
use tree_sitter::Parser;
use tree_sitter_language::LanguageFn;
use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::s;

use crate::utils::get_exe_dir;

const ABI_VERSION: u32 = 1;
const MAX_LANGUAGES: u32 = 128;

type LanguageBuilder = unsafe extern "C" fn() -> *const ();
type AbiVersionFn = unsafe extern "system" fn() -> u32;
type LanguageCountFn = unsafe extern "system" fn() -> u32;
type LanguageAtFn = unsafe extern "system" fn(u32, *mut RawLanguageDescriptor) -> u32;
type LoadedFn = unsafe extern "system" fn() -> isize;

#[derive(Clone, Copy)]
#[repr(C)]
struct RawSlice {
    data: *const u8,
    len: usize,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct RawLanguageDescriptor {
    name: RawSlice,
    aliases: RawSlice,
    injection_languages: RawSlice,
    language: Option<LanguageBuilder>,
    highlights: RawSlice,
    injections: RawSlice,
    locals: RawSlice,
}

pub(crate) fn register_languages() -> Result<usize> {
    let (module, path) = load_library()?;

    // LoadLibrary keeps the module mapped until FreeLibrary is called. No
    // matching call is made because every registered Language retains pointers
    // into the parser tables for the remainder of the process.
    let abi_version: AbiVersionFn = unsafe {
        mem::transmute::<LoadedFn, AbiVersionFn>(
            GetProcAddress(module, s!("nmt_tree_sitter_abi_version"))
                .context("tree_sitter.dll has no ABI version export")?,
        )
    };
    let language_count: LanguageCountFn = unsafe {
        mem::transmute::<LoadedFn, LanguageCountFn>(
            GetProcAddress(module, s!("nmt_tree_sitter_language_count"))
                .context("tree_sitter.dll has no language count export")?,
        )
    };
    let language_at: LanguageAtFn = unsafe {
        mem::transmute::<LoadedFn, LanguageAtFn>(
            GetProcAddress(module, s!("nmt_tree_sitter_language"))
                .context("tree_sitter.dll has no language export")?,
        )
    };

    let actual_abi = unsafe { abi_version() };
    if actual_abi != ABI_VERSION {
        bail!(
            "{} uses ABI version {actual_abi}, expected {ABI_VERSION}",
            path.display()
        );
    }

    let count = unsafe { language_count() };
    if count > MAX_LANGUAGES {
        bail!(
            "{} reports an invalid language count of {count}",
            path.display()
        );
    }

    let mut languages = Vec::with_capacity(count as usize);
    for index in 0..count {
        let mut raw = mem::MaybeUninit::<RawLanguageDescriptor>::uninit();
        if unsafe { language_at(index, raw.as_mut_ptr()) } == 0 {
            bail!("{} rejected language index {index}", path.display());
        }
        // A successful call initializes every field in the descriptor.
        let raw = unsafe { raw.assume_init() };
        languages.push(config(raw).with_context(|| {
            format!(
                "{} returned an invalid language at index {index}",
                path.display()
            )
        })?);
    }

    let registry = LanguageRegistry::singleton();
    let mut registered = 0;
    for (aliases, config) in languages {
        registry.register(&config.name, &config);
        registered += 1;
        for alias in aliases {
            registry.register(&alias, &config);
        }
    }

    Ok(registered)
}

fn load_library() -> Result<(HMODULE, PathBuf)> {
    let path = get_exe_dir().join("tree_sitter.dll");
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let module = unsafe { LoadLibraryW(wide.as_ptr()) };
    if module.is_null() {
        return Err(anyhow!(
            "cannot load tree_sitter.dll beside NiumaTerm.exe: {}",
            io::Error::last_os_error()
        ));
    }

    Ok((module, path))
}

fn config(raw: RawLanguageDescriptor) -> Result<(Vec<SharedString>, LanguageConfig)> {
    let name = text(raw.name)?;
    if name.is_empty() {
        bail!("empty language name");
    }

    let builder = raw.language.context("null language builder")?;
    let language = tree_sitter::Language::new(unsafe { LanguageFn::from_raw(builder) });
    Parser::new()
        .set_language(&language)
        .with_context(|| format!("unsupported Tree-sitter ABI for {name}"))?;

    let aliases = list(raw.aliases)?;
    let injection_languages = list(raw.injection_languages)?;
    let config = LanguageConfig::new(
        name,
        language,
        injection_languages,
        text(raw.highlights)?,
        text(raw.injections)?,
        text(raw.locals)?,
    );

    Ok((aliases, config))
}

fn list(raw: RawSlice) -> Result<Vec<SharedString>> {
    Ok(text(raw)?
        .split('\0')
        .filter(|value| !value.is_empty())
        .map(SharedString::from)
        .collect())
}

fn text(raw: RawSlice) -> Result<&'static str> {
    if raw.len == 0 {
        return Ok("");
    }
    if raw.data.is_null() {
        bail!("null string pointer with non-zero length");
    }

    // The checked DLL version defines every slice as immutable static storage,
    // and the module remains loaded for as long as any returned string can live.
    let bytes = unsafe { slice::from_raw_parts(raw.data, raw.len) };
    str::from_utf8(bytes).context("language data is not UTF-8")
}

const _: () = {
    assert!(mem::size_of::<*const c_void>() == mem::size_of::<LanguageBuilder>());
};
