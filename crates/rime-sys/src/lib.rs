//! Minimal hand-written FFI bindings for the librime C API.
//!
//! Struct layouts mirror `rime_api.h` (v1.17) exactly; only the functions
//! used by rime-daemon are typed precisely, the remaining `RimeApi` fields
//! are declared as generic function pointers (all fn pointers share one size,
//! so struct layout is unaffected).
//!
//! # Safety
//!
//! librime's C API is not thread-safe: all calls must be serialized.
//! rime-daemon serializes them with a single global mutex.

#![allow(non_camel_case_types)]
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};

/// `Bool` is `#define Bool int` in rime_api.h.
pub type Bool = c_int;
/// `RimeSessionId` is `typedef uintptr_t RimeSessionId;`.
pub type RimeSessionId = usize;

/// `typedef void (*RimeNotificationHandler)(void*, RimeSessionId, const char*, const char*);`
pub type RimeNotificationHandler =
    Option<unsafe extern "C" fn(*mut c_void, RimeSessionId, *const c_char, *const c_char)>;

/// `rime_traits_t` — field order per rime_api.h.
#[repr(C)]
pub struct RimeTraits {
    pub data_size: c_int,
    pub shared_data_dir: *const c_char,
    pub user_data_dir: *const c_char,
    pub distribution_name: *const c_char,
    pub distribution_code_name: *const c_char,
    pub distribution_version: *const c_char,
    pub app_name: *const c_char,
    pub modules: *const *const c_char,
    pub min_log_level: c_int,
    pub log_dir: *const c_char,
    pub prebuilt_data_dir: *const c_char,
    pub staging_dir: *const c_char,
}

/// `rime_commit_t`.
#[repr(C)]
pub struct RimeCommit {
    pub data_size: c_int,
    pub text: *mut c_char,
}

/// `RimeComposition`.
#[repr(C)]
pub struct RimeComposition {
    pub length: c_int,
    pub cursor_pos: c_int,
    pub sel_start: c_int,
    pub sel_end: c_int,
    pub preedit: *mut c_char,
}

/// `rime_candidate_t`.
#[repr(C)]
pub struct RimeCandidate {
    pub text: *mut c_char,
    pub comment: *mut c_char,
    pub reserved: *mut c_void,
}

/// `RimeMenu`.
#[repr(C)]
pub struct RimeMenu {
    pub page_size: c_int,
    pub page_no: c_int,
    pub is_last_page: Bool,
    pub highlighted_candidate_index: c_int,
    pub num_candidates: c_int,
    pub candidates: *mut RimeCandidate,
    pub select_keys: *mut c_char,
}

/// `rime_context_t`.
#[repr(C)]
pub struct RimeContext {
    pub data_size: c_int,
    pub composition: RimeComposition,
    pub menu: RimeMenu,
    pub commit_text_preview: *mut c_char,
    pub select_labels: *mut *mut c_char,
}

/// `rime_schema_list_item_t`.
#[repr(C)]
pub struct RimeSchemaListItem {
    pub schema_id: *mut c_char,
    pub name: *mut c_char,
    pub reserved: *mut c_void,
}

/// `rime_schema_list_t`.
#[repr(C)]
pub struct RimeSchemaList {
    pub size: usize,
    pub list: *mut RimeSchemaListItem,
}

/// Generic function pointer for RimeApi fields we do not call.
type FnPtr = Option<unsafe extern "C" fn()>;

/// `rime_api_t` — every field must stay in the exact order of rime_api.h.
#[repr(C)]
pub struct RimeApi {
    pub data_size: c_int,
    pub setup: Option<unsafe extern "C" fn(*mut RimeTraits)>,
    pub set_notification_handler:
        Option<unsafe extern "C" fn(RimeNotificationHandler, *mut c_void)>,
    pub initialize: Option<unsafe extern "C" fn(*mut RimeTraits)>,
    pub finalize: Option<unsafe extern "C" fn()>,
    pub start_maintenance: Option<unsafe extern "C" fn(Bool) -> Bool>,
    pub is_maintenance_mode: Option<unsafe extern "C" fn() -> Bool>,
    pub join_maintenance_thread: Option<unsafe extern "C" fn()>,
    pub deployer_initialize: FnPtr,
    pub prebuild: FnPtr,
    pub deploy: FnPtr,
    pub deploy_schema: FnPtr,
    pub deploy_config_file: FnPtr,
    pub sync_user_data: FnPtr,
    pub create_session: Option<unsafe extern "C" fn() -> RimeSessionId>,
    pub find_session: FnPtr,
    pub destroy_session: Option<unsafe extern "C" fn(RimeSessionId) -> Bool>,
    pub cleanup_stale_sessions: FnPtr,
    pub cleanup_all_sessions: FnPtr,
    pub process_key: Option<unsafe extern "C" fn(RimeSessionId, c_int, c_int) -> Bool>,
    pub commit_composition: Option<unsafe extern "C" fn(RimeSessionId) -> Bool>,
    pub clear_composition: Option<unsafe extern "C" fn(RimeSessionId)>,
    pub get_commit: Option<unsafe extern "C" fn(RimeSessionId, *mut RimeCommit) -> Bool>,
    pub free_commit: Option<unsafe extern "C" fn(*mut RimeCommit) -> Bool>,
    pub get_context: Option<unsafe extern "C" fn(RimeSessionId, *mut RimeContext) -> Bool>,
    pub free_context: Option<unsafe extern "C" fn(*mut RimeContext) -> Bool>,
    pub get_status: FnPtr,
    pub free_status: FnPtr,
    pub set_option: FnPtr,
    pub get_option: FnPtr,
    pub set_property: FnPtr,
    pub get_property: FnPtr,
    pub get_schema_list: Option<unsafe extern "C" fn(*mut RimeSchemaList) -> Bool>,
    pub free_schema_list: Option<unsafe extern "C" fn(*mut RimeSchemaList)>,
    pub get_current_schema: Option<unsafe extern "C" fn(RimeSessionId, *mut c_char, usize) -> Bool>,
    pub select_schema: Option<unsafe extern "C" fn(RimeSessionId, *const c_char) -> Bool>,
    pub schema_open: FnPtr,
    pub config_open: FnPtr,
    pub config_close: FnPtr,
    pub config_get_bool: FnPtr,
    pub config_get_int: FnPtr,
    pub config_get_double: FnPtr,
    pub config_get_string: FnPtr,
    pub config_get_cstring: FnPtr,
    pub config_update_signature: FnPtr,
    pub config_begin_map: FnPtr,
    pub config_next: FnPtr,
    pub config_end: FnPtr,
    pub simulate_key_sequence: FnPtr,
    pub register_module: FnPtr,
    pub find_module: FnPtr,
    pub run_task: FnPtr,
    pub get_shared_data_dir: FnPtr,
    pub get_user_data_dir: FnPtr,
    pub get_sync_dir: FnPtr,
    pub get_user_id: FnPtr,
    pub get_user_data_sync_dir: FnPtr,
    pub config_init: FnPtr,
    pub config_load_string: FnPtr,
    pub config_set_bool: FnPtr,
    pub config_set_int: FnPtr,
    pub config_set_double: FnPtr,
    pub config_set_string: FnPtr,
    pub config_get_item: FnPtr,
    pub config_set_item: FnPtr,
    pub config_clear: FnPtr,
    pub config_create_list: FnPtr,
    pub config_create_map: FnPtr,
    pub config_list_size: FnPtr,
    pub config_begin_list: FnPtr,
    pub get_input: FnPtr,
    pub get_caret_pos: FnPtr,
    pub select_candidate: FnPtr,
    pub get_version: FnPtr,
    pub set_caret_pos: FnPtr,
    pub select_candidate_on_current_page: FnPtr,
    pub candidate_list_begin: FnPtr,
    pub candidate_list_next: FnPtr,
    pub candidate_list_end: FnPtr,
    pub user_config_open: FnPtr,
    pub candidate_list_from_index: FnPtr,
    pub get_prebuilt_data_dir: FnPtr,
    pub get_staging_dir: FnPtr,
    pub commit_proto: FnPtr,
    pub context_proto: FnPtr,
    pub status_proto: FnPtr,
    pub get_state_label: FnPtr,
    pub delete_candidate: FnPtr,
    pub delete_candidate_on_current_page: FnPtr,
    pub get_state_label_abbreviated: FnPtr,
    pub set_input: FnPtr,
    pub get_shared_data_dir_s: FnPtr,
    pub get_user_data_dir_s: FnPtr,
    pub get_prebuilt_data_dir_s: FnPtr,
    pub get_staging_dir_s: FnPtr,
    pub get_sync_dir_s: FnPtr,
    pub highlight_candidate: FnPtr,
    pub highlight_candidate_on_current_page: FnPtr,
    pub change_page: FnPtr,
}

unsafe extern "C" {
    /// `RIME_API RimeApi* rime_get_api(void);`
    fn rime_get_api() -> *const RimeApi;
}

impl RimeApi {
    /// Acquire the process-wide `RimeApi`. Must be called after linking librime;
    /// the returned reference lives for the whole process lifetime.
    ///
    /// # Safety
    ///
    /// Safe to call once librime is loaded; the daemon calls this exactly once
    /// before spawning connection threads.
    pub unsafe fn get() -> &'static RimeApi {
        &*rime_get_api()
    }
}

/// Owned C strings used to build a `RimeTraits`; keep alive for the daemon's
/// lifetime (traits pointers are consumed by RimeInitialize immediately, but
/// librime may retain some of them).
pub struct TraitsOwned {
    pub shared_data_dir: CString,
    pub user_data_dir: CString,
    pub log_dir: CString,
    pub distribution_name: CString,
    pub distribution_code_name: CString,
    pub distribution_version: CString,
    pub app_name: CString,
}

/// Build a `RimeTraits` from owned strings. `min_log_level` matches the old
/// rimeshim default (FATAL = 3) unless overridden.
pub fn build_traits(t: &TraitsOwned, min_log_level: i32) -> RimeTraits {
    RimeTraits {
        data_size: (std::mem::size_of::<RimeTraits>() - std::mem::size_of::<c_int>()) as c_int,
        shared_data_dir: t.shared_data_dir.as_ptr(),
        user_data_dir: t.user_data_dir.as_ptr(),
        distribution_name: t.distribution_name.as_ptr(),
        distribution_code_name: t.distribution_code_name.as_ptr(),
        distribution_version: t.distribution_version.as_ptr(),
        app_name: t.app_name.as_ptr(),
        modules: std::ptr::null(),
        min_log_level,
        log_dir: t.log_dir.as_ptr(),
        prebuilt_data_dir: std::ptr::null(),
        staging_dir: std::ptr::null(),
    }
}

/// Initialize a C struct like the `RIME_STRUCT(Type, var)` macro.
///
/// # Safety
///
/// `ptr` must point to a zeroed-able struct whose first member is `data_size`.
pub unsafe fn rime_struct_init<T>(ptr: *mut T) {
    let data_size = (std::mem::size_of::<T>() - std::mem::size_of::<c_int>()) as c_int;
    std::ptr::write_bytes(ptr as *mut u8, 0, std::mem::size_of::<T>());
    std::ptr::write(ptr as *mut c_int, data_size);
}

/// Convert a borrowed C string to a Rust string (lossy), treating NULL as "".
///
/// # Safety
///
/// `ptr` must be NULL or a valid NUL-terminated string.
pub unsafe fn cstr_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    CStr::from_ptr(ptr).to_string_lossy().into_owned()
}
