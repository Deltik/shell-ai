use std::ffi::{c_char, c_int, c_long, c_uint, c_void};
use std::sync::Once;

use anyhow::{anyhow, Result};
use libloading::Library;

// CURLOPT constants (from curl.h)
// CURLOPTTYPE_LONG = 0, CURLOPTTYPE_OBJECTPOINT = 10000, CURLOPTTYPE_FUNCTIONPOINT = 20000
pub const CURLOPT_WRITEDATA: c_uint = 10001;
pub const CURLOPT_URL: c_uint = 10002;
pub const CURLOPT_POST: c_uint = 47;
pub const CURLOPT_POSTFIELDS: c_uint = 10015;
pub const CURLOPT_POSTFIELDSIZE_LARGE: c_uint = 30120;
pub const CURLOPT_HTTPHEADER: c_uint = 10023;
pub const CURLOPT_HEADERDATA: c_uint = 10029;
pub const CURLOPT_WRITEFUNCTION: c_uint = 20011;
pub const CURLOPT_HEADERFUNCTION: c_uint = 20079;
pub const CURLOPT_NOSIGNAL: c_uint = 99;
pub const CURLOPT_TIMEOUT: c_uint = 13;

// curl_global_init flags
pub const CURL_GLOBAL_DEFAULT: c_long = 3;

// CURLcode
pub const CURLE_OK: c_int = 0;

/// Holds dynamically loaded libcurl-impersonate function pointers.
///
/// The `_lib` field keeps the loaded library alive. When loaded from the global
/// symbol namespace (LD_PRELOAD), this is `None` since the library is already
/// in-process and doesn't need to be kept alive by us.
pub struct CurlLibrary {
    _lib: Option<Library>,
    pub global_init: unsafe extern "C" fn(c_long) -> c_int,
    pub easy_init: unsafe extern "C" fn() -> *mut c_void,
    pub easy_cleanup: unsafe extern "C" fn(*mut c_void),
    // curl_easy_setopt is variadic in C. We use typed non-variadic function pointers
    // which is safe on x86_64 and aarch64-linux where integer/pointer variadic args
    // use the same registers as named args.
    pub easy_setopt_long: unsafe extern "C" fn(*mut c_void, c_uint, c_long) -> c_int,
    pub easy_setopt_ptr: unsafe extern "C" fn(*mut c_void, c_uint, *const c_void) -> c_int,
    pub easy_perform: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub slist_append: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    pub slist_free_all: unsafe extern "C" fn(*mut c_void),
}

// Safety: CurlLibrary only holds function pointers (which are Send+Sync) and an
// optional Library handle. The Library itself is Send+Sync. The function pointers
// are safe to call from any thread as long as each CURL easy handle is used from
// only one thread at a time (which our curl_backend ensures).
unsafe impl Send for CurlLibrary {}
unsafe impl Sync for CurlLibrary {}

static CURL_GLOBAL_INIT: Once = Once::new();

/// Try to load libcurl-impersonate at runtime.
///
/// Detection strategy:
/// 1. Check the global symbol namespace (covers LD_PRELOAD)
/// 2. Try dlopen("libcurl-impersonate.so") (lexiforest/curl-impersonate unified library)
///
/// The distinguishing marker is the `curl_easy_impersonate` symbol, which regular
/// libcurl does not have.
pub fn try_load() -> Result<CurlLibrary> {
    // Strategy 1: Check global namespace (LD_PRELOAD or already linked)
    if let Ok(lib) = try_load_from_global() {
        return Ok(lib);
    }

    // Strategy 2: Try loading the library by name
    let lib_names = [
        "libcurl-impersonate.so",
    ];

    let mut last_err = anyhow!("No library names to try");
    for name in &lib_names {
        match try_load_library(name) {
            Ok(lib) => return Ok(lib),
            Err(e) => {
                log::debug!("Failed to load {}: {}", name, e);
                last_err = e;
            }
        }
    }

    Err(last_err)
}

/// Try loading curl symbols from the global namespace (for LD_PRELOAD).
fn try_load_from_global() -> Result<CurlLibrary> {
    // dlopen(NULL) returns a handle to the current process
    let lib = unsafe { Library::new("") }
        .map_err(|e| anyhow!("Failed to open global namespace: {}", e))?;

    // Check for the impersonation marker symbol
    if unsafe { lib.get::<*const c_void>(b"curl_easy_impersonate\0") }.is_err() {
        return Err(anyhow!("curl_easy_impersonate not found in global namespace"));
    }

    let curl_lib = load_symbols_from(&lib)?;

    // Don't keep the global handle — it doesn't need to be held open
    std::mem::forget(lib);

    init_global(&curl_lib);

    Ok(CurlLibrary {
        _lib: None,
        ..curl_lib
    })
}

/// Try loading a specific library file and verify it has impersonation support.
fn try_load_library(name: &str) -> Result<CurlLibrary> {
    let lib = unsafe { Library::new(name) }
        .map_err(|e| anyhow!("Failed to load {}: {}", name, e))?;

    // Check for the impersonation marker symbol
    if unsafe { lib.get::<*const c_void>(b"curl_easy_impersonate\0") }.is_err() {
        return Err(anyhow!("{} does not have curl_easy_impersonate", name));
    }

    let mut curl_lib = load_symbols_from(&lib)?;

    init_global(&curl_lib);

    curl_lib._lib = Some(lib);
    Ok(curl_lib)
}

/// Load all required curl function symbols from a library handle.
fn load_symbols_from(lib: &Library) -> Result<CurlLibrary> {
    unsafe {
        let global_init = *lib.get::<unsafe extern "C" fn(c_long) -> c_int>(b"curl_global_init\0")
            .map_err(|e| anyhow!("curl_global_init: {}", e))?;
        let easy_init = *lib.get::<unsafe extern "C" fn() -> *mut c_void>(b"curl_easy_init\0")
            .map_err(|e| anyhow!("curl_easy_init: {}", e))?;
        let easy_cleanup = *lib.get::<unsafe extern "C" fn(*mut c_void)>(b"curl_easy_cleanup\0")
            .map_err(|e| anyhow!("curl_easy_cleanup: {}", e))?;

        // Load curl_easy_setopt once, cast to typed variants
        let easy_setopt_raw = *lib.get::<unsafe extern "C" fn()>(b"curl_easy_setopt\0")
            .map_err(|e| anyhow!("curl_easy_setopt: {}", e))?;
        let easy_setopt_long: unsafe extern "C" fn(*mut c_void, c_uint, c_long) -> c_int =
            std::mem::transmute(easy_setopt_raw);
        let easy_setopt_ptr: unsafe extern "C" fn(*mut c_void, c_uint, *const c_void) -> c_int =
            std::mem::transmute(easy_setopt_raw);

        let easy_perform = *lib.get::<unsafe extern "C" fn(*mut c_void) -> c_int>(b"curl_easy_perform\0")
            .map_err(|e| anyhow!("curl_easy_perform: {}", e))?;

        let slist_append = *lib.get::<unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void>(b"curl_slist_append\0")
            .map_err(|e| anyhow!("curl_slist_append: {}", e))?;
        let slist_free_all = *lib.get::<unsafe extern "C" fn(*mut c_void)>(b"curl_slist_free_all\0")
            .map_err(|e| anyhow!("curl_slist_free_all: {}", e))?;

        Ok(CurlLibrary {
            _lib: None,
            global_init,
            easy_init,
            easy_cleanup,
            easy_setopt_long,
            easy_setopt_ptr,
            easy_perform,
            slist_append,
            slist_free_all,
        })
    }
}

/// Call curl_global_init exactly once.
fn init_global(curl: &CurlLibrary) {
    CURL_GLOBAL_INIT.call_once(|| {
        let ret = unsafe { (curl.global_init)(CURL_GLOBAL_DEFAULT) };
        if ret != CURLE_OK {
            log::warn!("curl_global_init failed with code {}", ret);
        }
    });
}