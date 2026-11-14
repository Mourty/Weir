//! The window's display going away, as when someone logs out of an X11
//! desktop with the mixer open, ends the window: it is not a bug in Weir.

/// Close the window quietly, with no crash report, when a panic says it
/// lost its connection to the display. Call after
/// [`weir_protocol::crash::install`], so this hook runs first.
///
/// When the X server goes away, whichever notices first ends the window:
/// Xlib prints that the connection broke and exits with 1, or winit 0.30
/// panics in a call it expects never to fail (finding the window's
/// position, which eframe asks for every frame). The panic left a crash
/// report that greeted the user at their next login (Weir issue #47).
/// This makes both endings the same.
pub fn end_quietly_when_lost() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let file = info.location().map(|l| l.file()).unwrap_or_default();
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or_default();
        if lost(file, message) {
            eprintln!("Weir's window lost its connection to the display, so it closes.");
            std::process::exit(1);
        }
        hook(info);
    }));
}

/// Whether a panic at `file` saying `message` is winit's X11 code finding
/// the connection to the X server broken. winit names that error
/// `Connection(..)`; a bad window or other X error has another name and
/// is still reported.
fn lost(file: &str, message: &str) -> bool {
    file.contains("winit") && file.contains("/x11/") && message.contains("Connection(")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_broken_x_connection_in_winit_is_the_display_going_away() {
        // As in issue #47.
        let file = "/github/home/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/winit-0.30.13/src/platform_impl/linux/x11/window.rs";
        let message = "called `Result::unwrap()` on an `Err` value: Connection(IoError(Custom { kind: Other, error: UnknownError }))";
        assert!(lost(file, message));
        assert!(
            lost(
                "/builddir/build/BUILD/weir/vendor/winit/src/platform_impl/linux/x11/window.rs",
                "Failed to call XUnmapWindow: Connection(IoError(Kind(BrokenPipe)))"
            ),
            "a vendored winit, and an expect"
        );
    }

    #[test]
    fn other_panics_are_still_crashes() {
        let x11 = "/x/winit-0.30.13/src/platform_impl/linux/x11/window.rs";
        assert!(
            !lost(
                x11,
                "called `Result::unwrap()` on an `Err` value: X11(X11Error { error_kind: Window })"
            ),
            "a bad window is not a lost display"
        );
        assert!(
            !lost("crates/gui/src/app/bus.rs", "Connection(IoError(..))"),
            "Weir's own code"
        );
        assert!(
            !lost(
                "/x/winit-0.30.13/src/platform_impl/linux/wayland/window/mod.rs",
                "Connection(..)"
            ),
            "only X11"
        );
    }
}
