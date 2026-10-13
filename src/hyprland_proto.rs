//! Generated bindings for `hyprland-toplevel-mapping-v1`, which maps a foreign toplevel to the
//! Hyprland window address that `hyprctl` reports, so a picked window can be captured by handle.

#![allow(dead_code, non_upper_case_globals, non_camel_case_types, clippy::all)]

pub mod mapping {
    use wayland_client;
    use wayland_protocols::ext::foreign_toplevel_list::v1::client::*;
    use wayland_protocols_wlr::foreign_toplevel::v1::client::*;

    pub mod __interfaces {
        use wayland_protocols::ext::foreign_toplevel_list::v1::client::__interfaces::*;
        use wayland_protocols_wlr::foreign_toplevel::v1::client::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/hyprland-toplevel-mapping-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/hyprland-toplevel-mapping-v1.xml");
}
