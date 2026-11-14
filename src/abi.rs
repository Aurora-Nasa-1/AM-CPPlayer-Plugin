//! C ABI v1 for GraalVM Native Image hosts.
//!
//! Five symbols are mandatory: `cpp_abi_version`, `cpp_create`, `cpp_invoke`,
//! `cpp_cancel`, `cpp_close`. All buffers are host-owned; no allocation crosses
//! the library boundary.

use std::ffi::{c_char, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use crate::session::{self, Handle};

pub const ABI_VERSION: i32 = 1;

const INVOKE_OK: i32 = 0;
const INVOKE_TOO_SMALL: i32 = 1;
const INVOKE_FAILED: i32 = 2;

unsafe fn read_cstr(value: *const c_char) -> Option<String> {
    if value.is_null() {
        return None;
    }
    CStr::from_ptr(value).to_str().ok().map(str::to_string)
}

unsafe fn write_error(err: *mut c_char, err_cap: i32, message: &str) {
    if err.is_null() || err_cap <= 0 {
        return;
    }
    let capacity = (err_cap as usize).saturating_sub(1);
    let bytes = message.as_bytes();
    let length = bytes.len().min(capacity);
    ptr::copy_nonoverlapping(bytes.as_ptr(), err as *mut u8, length);
    *((err as *mut u8).add(length)) = 0;
}

#[no_mangle]
pub extern "C" fn cpp_abi_version() -> i32 {
    ABI_VERSION
}

/// # Safety
///
/// String pointers must be NUL-terminated UTF-8 or null. `err` must point to
/// at least `err_cap` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn cpp_create(
    provider_id: *const c_char,
    account_id: *const c_char,
    credential: *const c_char,
    settings_json: *const c_char,
    err: *mut c_char,
    err_cap: i32,
) -> i64 {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        session::create(
            read_cstr(provider_id).unwrap_or_else(|| "apple-music".to_string()),
            read_cstr(account_id).unwrap_or_else(|| "default".to_string()),
            read_cstr(credential),
            read_cstr(settings_json),
        )
    }));
    match outcome {
        Ok(Ok(handle)) => handle,
        Ok(Err(failure)) => {
            write_error(err, err_cap, &failure.message);
            0
        }
        Err(_) => {
            write_error(err, err_cap, "provider session creation panicked");
            0
        }
    }
}

/// # Safety
///
/// `request` must point to `request_len` readable bytes; `out` must point to
/// `out_cap` writable bytes; `out_len` may be null.
#[no_mangle]
pub unsafe extern "C" fn cpp_invoke(
    handle: Handle,
    operation: i32,
    request: *const u8,
    request_len: i32,
    out: *mut u8,
    out_cap: i32,
    out_len: *mut i32,
) -> i32 {
    if !out_len.is_null() {
        *out_len = 0;
    }
    if handle == 0 {
        return INVOKE_FAILED;
    }
    let input: &[u8] = if request.is_null() || request_len <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(request, request_len as usize)
    };
    let response = match catch_unwind(AssertUnwindSafe(|| {
        session::invoke(handle, operation, input)
    })) {
        Ok(response) => response,
        Err(_) => return INVOKE_FAILED,
    };
    let length = response.len() as i32;
    if !out_len.is_null() {
        *out_len = length;
    }
    if length > 0 && (out.is_null() || out_cap < length) {
        return INVOKE_TOO_SMALL;
    }
    if length > 0 {
        ptr::copy_nonoverlapping(response.as_ptr(), out, response.len());
    }
    INVOKE_OK
}

/// # Safety
///
/// `handle` must be a live handle returned by `cpp_create`; the handle stays
/// valid until `cpp_close`.
#[no_mangle]
pub unsafe extern "C" fn cpp_cancel(handle: Handle) {
    let _ = catch_unwind(AssertUnwindSafe(|| session::cancel(handle)));
}

/// # Safety
///
/// `handle` must be a live handle returned by `cpp_create` and must not be
/// closed twice.
#[no_mangle]
pub unsafe extern "C" fn cpp_close(handle: Handle) {
    let _ = catch_unwind(AssertUnwindSafe(|| session::close(handle)));
}
