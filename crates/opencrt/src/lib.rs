//! OpenCRT: the terminal application (`gui_main`) and the parts the
//! command-line tools (`ocrt`) share with it.

mod agent;
mod app;
mod cli;
mod cast;
mod codec;
mod config;
mod crt;
mod filebrowser;
mod forward;
mod gitsync;
#[cfg(fuzzing)]
pub mod fuzz;
mod ime;
mod input;
mod instance;
mod kermit;
mod load;
mod local;
mod log;
#[cfg(test)]
mod memfs;
mod print;
mod proxy;
mod python;
mod rdp;
mod vnc;
mod render;
mod rhaiscript;
mod script;
mod serial;
mod sessions;
mod sftp;
mod ssh;
mod telnet;
mod terminal;
mod tftp;
mod update;
mod xymodem;
mod zmodem;

slint::include_modules!();

/// The command-line tools, `ocrt`; returns the exit code.
pub fn cli_main() -> i32 {
    cli::main()
}

/// Some Wayland desktops apply HiDPI through XSettings and leave the
/// compositor's wl_output scale at 1, so winit reports 100% and Slint draws
/// every logical pixel unscaled. SLINT_SCALE_FACTOR is Slint's override; the
/// X11 backend reads Xft.dpi itself and must not be overridden.
#[cfg(all(unix, not(target_os = "macos")))]
fn apply_desktop_scale() {
    if std::env::var_os("SLINT_SCALE_FACTOR").is_some() {
        return;
    }
    let wayland = ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"]
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|value| !value.is_empty()));
    if !wayland {
        return;
    }
    let Some(scale) = config::desktop_scale().filter(|s| *s > 1.0) else {
        return;
    };
    // SAFETY: gui_main calls this before gitsync or the UI spawn any threads.
    unsafe { std::env::set_var("SLINT_SCALE_FACTOR", scale.to_string()) };
}

/// Fcitx 4 has no native Wayland text-input frontend. On a Wayland session
/// where it is the active input method, use X11/XWayland so its XIM server
/// receives the terminal's IME events. Fcitx 5 keeps the native Wayland path.
fn forced_x11_backend() -> bool {
    #[cfg(target_os = "linux")]
    return fcitx4_needs_x11();
    #[cfg(not(target_os = "linux"))]
    false
}

#[cfg(target_os = "linux")]
fn fcitx4_needs_x11() -> bool {
    let wayland = ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"]
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|value| !value.is_empty()));
    let x11 = std::env::var("DISPLAY").is_ok_and(|value| !value.is_empty());
    let xmodifiers = std::env::var("XMODIFIERS").unwrap_or_default();
    let fcitx5 = process_named(b"fcitx5");
    let fcitx4 = process_named(b"fcitx") || command_in_path("fcitx-remote");
    prefer_x11_for_fcitx4(wayland, x11, &xmodifiers, fcitx4, fcitx5)
}

#[cfg(target_os = "linux")]
fn prefer_x11_for_fcitx4(wayland: bool, x11: bool, xmodifiers: &str, fcitx4: bool, fcitx5: bool) -> bool {
    wayland && x11 && fcitx4 && !fcitx5 && xmodifiers.contains("@im=fcitx")
}

#[cfg(target_os = "linux")]
fn process_named(prefix: &[u8]) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else { return false };
    entries.filter_map(Result::ok).any(|entry| {
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else { return false };
        let Some(program) = cmdline.split(|byte| *byte == 0).next() else { return false };
        let name = program.rsplit(|byte| *byte == b'/').next().unwrap_or(program);
        name.starts_with(prefix)
    })
}

#[cfg(target_os = "linux")]
fn command_in_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

#[cfg(target_os = "linux")]
fn x11_event_loop_builder() -> slint::winit_030::EventLoopBuilder {
    use slint::winit_030::winit::event_loop::EventLoop;
    use slint::winit_030::winit::platform::x11::EventLoopBuilderExtX11;
    let mut builder = EventLoop::with_user_event();
    builder.with_x11();
    builder
}

/// The GUI application: `opencrt [options]` (see `app::parse_args`).
pub fn gui_main() -> anyhow::Result<()> {
    let _force_x11 = forced_x11_backend();
    // The X11 backend reads Xft.dpi itself; Slint's Wayland scale override
    // must not be applied when we force X11/XWayland.
    #[cfg(all(unix, not(target_os = "macos")))]
    if !_force_x11 {
        apply_desktop_scale();
    }
    // A settings repository is brought up to date before anything reads it.
    let args: Vec<String> = std::env::args().skip(1).collect();
    gitsync::start_up(&args);
    // `/F folder` picks the configuration folder; everything below uses it.
    let args = config::use_dir_from_args(args);
    if args.iter().any(|a| a.eq_ignore_ascii_case("/T")) && instance::hand_over(&args) {
        return Ok(());
    }
    // Release builds have no console: keep a record of panics and fatal errors.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        config::log_crash(&format!("panic: {info}\n{}", std::backtrace::Backtrace::force_capture()));
        default_hook(info);
    }));

    // CPU-rendered like a classic desktop app: no GPU swapchain.
    let selector = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .with_winit_window_attributes_hook(desktop_id);
    #[cfg(target_os = "linux")]
    let selector = if _force_x11 {
        selector.with_winit_event_loop_builder(x11_event_loop_builder())
    } else {
        selector
    };
    let result = selector
        .select()
        .map_err(anyhow::Error::from)
        .and_then(|()| app::run(&args));
    if let Err(e) = &result {
        config::log_crash(&format!("error: {e:#}"));
    }
    gitsync::finish();
    result
}

/// Name the window "opencrt" (Wayland app ID, X11 class) so the desktop finds
/// opencrt.desktop and shows its icon: Wayland compositors ignore window icons.
fn desktop_id(attributes: slint::winit_030::winit::window::WindowAttributes) -> slint::winit_030::winit::window::WindowAttributes {
    #[cfg(all(unix, not(target_os = "macos")))]
    let attributes = {
        use slint::winit_030::winit::platform::{wayland::WindowAttributesExtWayland, x11::WindowAttributesExtX11};
        let attributes = WindowAttributesExtWayland::with_name(attributes, "opencrt", "opencrt");
        WindowAttributesExtX11::with_name(attributes, "opencrt", "opencrt")
    };
    attributes
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::prefer_x11_for_fcitx4;

    #[test]
    fn fcitx4_uses_xwayland_only_when_needed() {
        assert!(prefer_x11_for_fcitx4(true, true, "@im=fcitx", true, false));
        assert!(!prefer_x11_for_fcitx4(false, true, "@im=fcitx", true, false), "already on X11");
        assert!(!prefer_x11_for_fcitx4(true, false, "@im=fcitx", true, false), "no XWayland");
        assert!(!prefer_x11_for_fcitx4(true, true, "@im=ibus", true, false), "not Fcitx");
        assert!(!prefer_x11_for_fcitx4(true, true, "@im=fcitx", false, false), "Fcitx 4 absent");
        assert!(!prefer_x11_for_fcitx4(true, true, "@im=fcitx", true, true), "Fcitx 5 keeps Wayland");
    }
}
