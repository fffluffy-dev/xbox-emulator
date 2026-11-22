use core::ffi::c_void;

pub const S_OK: i32 = 0;
pub const E_INVALIDARG: i32 = 0x8007_0057u32 as i32;
pub const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Guid {
    pub d1: u32,
    pub d2: u16,
    pub d3: u16,
    pub d4: [u8; 8],
}

#[no_mangle]
pub extern "system" fn InitializeApiImpl(_gdk_version: u64, _game_os_version: u64) -> i32 {
    S_OK
}

#[no_mangle]
pub extern "system" fn InitializeApiImplEx(_gdk_version: u64, _game_os_version: u64, _options: usize) -> i32 {
    S_OK
}

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "system" fn QueryApiImpl(provider: *const Guid, iid: *const Guid, out: *mut *mut c_void) -> i32 {
    if provider.is_null() || iid.is_null() || out.is_null() {
        return E_INVALIDARG;
    }
    unsafe { *out = core::ptr::null_mut() };
    E_NOINTERFACE
}

#[no_mangle]
pub extern "system" fn UninitializeApiImpl() {}

#[no_mangle]
pub extern "system" fn XErrorReport(_hr: i32, _message: *const u8) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_answers_no_interface_and_clears_the_output() {
        let (c, i) = (Guid { d1: 1, ..Default::default() }, Guid { d1: 2, ..Default::default() });
        let mut out = 0x1234usize as *mut c_void;
        assert_eq!(QueryApiImpl(&c, &i, &mut out), E_NOINTERFACE);
        assert!(out.is_null());
        assert_eq!(QueryApiImpl(core::ptr::null(), &i, &mut out), E_INVALIDARG);
        assert_eq!(QueryApiImpl(&c, &i, core::ptr::null_mut()), E_INVALIDARG);
    }

    #[test]
    fn initialize_and_teardown_succeed() {
        assert_eq!(InitializeApiImpl(0x2306_0000_0000_0000, 0), S_OK);
        assert_eq!(InitializeApiImplEx(0, 0, 0), S_OK);
        UninitializeApiImpl();
        XErrorReport(E_NOINTERFACE, c"x".as_ptr().cast());
    }

    #[test]
    fn exports_txt_names_every_entry_point_once() {
        let names: Vec<String> = xone_exports_names();
        assert_eq!(names, ["InitializeApiImpl", "InitializeApiImplEx", "QueryApiImpl", "UninitializeApiImpl", "XErrorReport"]);
    }

    fn xone_exports_names() -> Vec<String> {
        include_str!("../exports.txt").lines().filter_map(|l| l.split_whitespace().nth(1).map(str::to_string)).collect()
    }
}
