use std::ffi::c_char;
#[cfg(not(no_swift_bridge))]
use std::path::{Path, PathBuf};
#[cfg(not(no_swift_bridge))]
use std::sync::OnceLock;

use libloading::Library;

#[repr(i32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexStoreStatus {
    Ok = 0,
    OpenFailed = 1,
    InvalidHandle = 2,
    ExtractFailed = 3,
}

impl TryFrom<i32> for IndexStoreStatus {
    type Error = i32;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Ok),
            1 => Ok(Self::OpenFailed),
            2 => Ok(Self::InvalidHandle),
            3 => Ok(Self::ExtractFailed),
            other => Err(other),
        }
    }
}

type IndexStoreOpenFn = unsafe extern "C" fn(*const c_char, *mut i32) -> *mut std::ffi::c_void;
type IndexStoreCloseFn = unsafe extern "C" fn(*mut std::ffi::c_void);
type IndexStoreExtractFn =
    unsafe extern "C" fn(*mut std::ffi::c_void, *const c_char, *mut u32, *mut i32) -> *const u8;
type IndexStoreWarmupFn = unsafe extern "C" fn(*mut std::ffi::c_void);
type SwiftSyntaxExtractFn =
    unsafe extern "C" fn(*const c_char, usize, *const c_char) -> *const c_char;
type FreeStringFn = unsafe extern "C" fn(*mut c_char);
type FreeBufferFn = unsafe extern "C" fn(*mut u8);

pub struct SwiftBridge {
    _lib: Library,
    pub indexstore_open: IndexStoreOpenFn,
    pub indexstore_close: IndexStoreCloseFn,
    pub indexstore_extract: IndexStoreExtractFn,
    pub indexstore_warmup: Option<IndexStoreWarmupFn>,
    pub swiftsyntax_extract: SwiftSyntaxExtractFn,
    pub free_string: FreeStringFn,
    pub free_buffer: FreeBufferFn,
}

#[cfg(not(no_swift_bridge))]
static BRIDGE: OnceLock<Option<SwiftBridge>> = OnceLock::new();
#[cfg(not(no_swift_bridge))]
const BRIDGE_DYLIB_NAME: &str = "libGraphaSwiftBridge.dylib";
#[cfg(not(no_swift_bridge))]
const RUNTIME_BRIDGE_PATH_ENV: &str = "GRAPHA_SWIFT_BRIDGE_PATH";

impl SwiftBridge {
    #[cfg(not(no_swift_bridge))]
    fn load() -> Option<Self> {
        let lib_path = Self::find_dylib()?;
        let lib = unsafe { Library::new(&lib_path) }.ok()?;

        unsafe {
            let indexstore_open = *lib
                .get::<IndexStoreOpenFn>(b"grapha_indexstore_open")
                .ok()?;
            let indexstore_close = *lib
                .get::<IndexStoreCloseFn>(b"grapha_indexstore_close")
                .ok()?;
            let indexstore_extract = *lib
                .get::<IndexStoreExtractFn>(b"grapha_indexstore_extract")
                .ok()?;
            let indexstore_warmup = lib
                .get::<IndexStoreWarmupFn>(b"grapha_indexstore_warmup")
                .ok()
                .map(|s| *s);
            let swiftsyntax_extract = *lib
                .get::<SwiftSyntaxExtractFn>(b"grapha_swiftsyntax_extract")
                .ok()?;
            let free_string = *lib.get::<FreeStringFn>(b"grapha_free_string").ok()?;
            let free_buffer = *lib.get::<FreeBufferFn>(b"grapha_free_buffer").ok()?;

            Some(SwiftBridge {
                _lib: lib,
                indexstore_open,
                indexstore_close,
                indexstore_extract,
                indexstore_warmup,
                swiftsyntax_extract,
                free_string,
                free_buffer,
            })
        }
    }

    #[cfg(not(no_swift_bridge))]
    fn find_dylib() -> Option<PathBuf> {
        let runtime_override = std::env::var_os(RUNTIME_BRIDGE_PATH_ENV).map(PathBuf::from);
        let current_exe = std::env::current_exe().ok();
        let build_dir = option_env!("SWIFT_BRIDGE_PATH").map(PathBuf::from);

        resolve_existing_dylib(
            runtime_override.as_deref(),
            current_exe.as_deref(),
            build_dir.as_deref(),
        )
    }
}

#[cfg(not(no_swift_bridge))]
fn resolve_existing_dylib(
    runtime_override: Option<&Path>,
    current_exe: Option<&Path>,
    build_dir: Option<&Path>,
) -> Option<PathBuf> {
    dylib_candidates(runtime_override, current_exe, build_dir)
        .into_iter()
        .find(|path| path.exists())
}

#[cfg(not(no_swift_bridge))]
fn dylib_candidates(
    runtime_override: Option<&Path>,
    current_exe: Option<&Path>,
    build_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    let mut push_unique = |path: PathBuf| {
        if !candidates.iter().any(|existing| existing == &path) {
            candidates.push(path);
        }
    };

    if let Some(path) = runtime_override {
        push_unique(normalize_dylib_candidate(path));
    }

    if let Some(executable_path) = current_exe
        && let Some(executable_dir) = executable_path.parent()
    {
        push_unique(executable_dir.join(BRIDGE_DYLIB_NAME));
        push_unique(executable_dir.join("lib").join(BRIDGE_DYLIB_NAME));
    }

    if let Some(path) = build_dir {
        push_unique(normalize_dylib_candidate(path));
    }

    candidates
}

#[cfg(not(no_swift_bridge))]
fn normalize_dylib_candidate(path: &Path) -> PathBuf {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == BRIDGE_DYLIB_NAME)
    {
        path.to_path_buf()
    } else {
        path.join(BRIDGE_DYLIB_NAME)
    }
}

#[cfg(not(no_swift_bridge))]
pub fn bridge() -> Option<&'static SwiftBridge> {
    BRIDGE.get_or_init(SwiftBridge::load).as_ref()
}

#[cfg(no_swift_bridge)]
pub fn bridge() -> Option<&'static SwiftBridge> {
    None
}

#[cfg(test)]
mod tests;
