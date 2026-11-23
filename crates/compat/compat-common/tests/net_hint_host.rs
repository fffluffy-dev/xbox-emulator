use compat_common::gdk::net::{self, NlHint};
use compat_common::os_host;

#[test]
fn title_connectivity_hint_follows_the_setting_without_the_host_export() {
    std::env::set_var("TOMBOX_CONNECTIVITY", "local");
    let mut h = NlHint { level: 9, cost: 9, approaching: 1, over: 1, roaming: 1 };
    assert_eq!(unsafe { net::get_network_connectivity_hint(&mut h) }, 0);
    assert_eq!(h.level, 2);
    assert_ne!(h.cost, 0);
    assert_eq!(h.level, net::connectivity_level());
    assert_eq!(unsafe { net::get_network_connectivity_hint(core::ptr::null_mut()) }, 87);
    std::env::remove_var("TOMBOX_CONNECTIVITY");
}

#[test]
fn iphlpapi_connectivity_hint_is_hooked_in_title_imports() {
    let h = os_host::hooks();
    let hook = h.iter().find(|h| h.name == "GetNetworkConnectivityHint").expect("hook");
    assert!(os_host::patch::dll_matches("IPHLPAPI.DLL", hook.dlls));
    assert_eq!(hook.hook, net::get_network_connectivity_hint as *const () as usize);
}
