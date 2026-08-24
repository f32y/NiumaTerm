use std::{io, ptr, slice};

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData,
};

pub fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    call(data, |input, output| unsafe {
        CryptProtectData(
            input,
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            0,
            output,
        )
    })
}

pub fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    call(data, |input, output| unsafe {
        CryptUnprotectData(
            input,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            0,
            output,
        )
    })
}

fn call(
    data: &[u8],
    operation: impl Fn(*const CRYPT_INTEGER_BLOB, *mut CRYPT_INTEGER_BLOB) -> i32,
) -> io::Result<Vec<u8>> {
    let length = u32::try_from(data.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "input is too large"))?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: length,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };

    if operation(&input, &mut output) == 0 {
        return Err(io::Error::last_os_error());
    }

    let result = if output.cbData == 0 {
        Vec::new()
    } else {
        unsafe { slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec()
    };
    unsafe { LocalFree(output.pbData.cast()) };
    Ok(result)
}
