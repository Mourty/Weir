//! The window's display going away, as when someone logs out of an X11
//! desktop with the mixer open, or its window being destroyed by another
//! program, ends the window: it is not a bug in Weir.

use std::io::Write;

/// Close the window quietly, with no crash report, when a panic says its
/// display or its window is gone. Call after
/// [`weir_protocol::crash::install`], so this hook runs first.
///
/// winit 0.30 unwraps X calls it expects never to fail, such as finding
/// the window's position, which eframe asks for every frame. When the X
/// server goes away, whichever notices first ends the window: Xlib prints
/// that the connection broke and exits with 1, or winit panics. When
/// another program destroys the window (`xdotool windowclose`), winit
/// panics on the next frame. Each panic left a crash report, which
/// greeted the user at their next login (Weir issue #47), or even, within
/// half a minute of starting, the offer of safe mode.
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
        if let Some(gone) = gone(file, message) {
            // Not eprintln!, which panics when stderr cannot be written (a
            // terminal closed at logout), and a panic in a panic hook
            // aborts the program.
            let _ = writeln!(std::io::stderr(), "{}", gone.says());
            std::process::exit(gone.exit_code());
        }
        hook(info);
    }));
}

/// What went away from under the window.
#[derive(Debug, PartialEq, Eq)]
enum Gone {
    /// The connection to the X server: the desktop ended or the server
    /// crashed.
    Display,
    /// The window itself, destroyed by another program.
    Window,
}

impl Gone {
    fn says(&self) -> &'static str {
        match self {
            Gone::Display => "Weir's window lost its connection to the display, so it closes.",
            Gone::Window => "Weir's window was closed by another program.",
        }
    }

    /// 1 for a lost display, as Xlib exits when it notices first; 0 for a
    /// window closed from outside, as if closed the usual way.
    fn exit_code(&self) -> i32 {
        match self {
            Gone::Display => 1,
            Gone::Window => 0,
        }
    }
}

/// Whether a panic at `file` saying `message` is winit's X11 code finding
/// the display or the window gone. winit names a broken connection
/// `Connection(..)`; a window that no longer exists gives the X server's
/// BadWindow (`error_kind: Window`) or, asked for its size, BadDrawable
/// (`error_kind: Drawable`), whichever call comes first. Other X errors
/// are still reported.
fn gone(file: &str, message: &str) -> Option<Gone> {
    if !(file.contains("winit") && file.contains("/x11/")) {
        return None;
    }
    if message.contains("Connection(") {
        Some(Gone::Display)
    } else if message.contains("error_kind: Window,") || message.contains("error_kind: Drawable,") {
        Some(Gone::Window)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const X11: &str = "/github/home/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/winit-0.30.13/src/platform_impl/linux/x11/window.rs";

    #[test]
    fn a_broken_x_connection_in_winit_is_the_display_going_away() {
        // As in issue #47.
        let message = "called `Result::unwrap()` on an `Err` value: Connection(IoError(Custom { kind: Other, error: UnknownError }))";
        assert_eq!(gone(X11, message), Some(Gone::Display));
        assert_eq!(
            gone(
                "/builddir/build/BUILD/weir/vendor/winit/src/platform_impl/linux/x11/window.rs",
                "Failed to call XUnmapWindow: Connection(IoError(Kind(BrokenPipe)))"
            ),
            Some(Gone::Display),
            "a vendored winit, and an expect"
        );
    }

    #[test]
    fn a_window_destroyed_from_outside_is_closed() {
        // As `xdotool windowclose` makes it.
        let message = "called `Result::unwrap()` on an `Err` value: X11(X11Error { error_kind: Window, error_code: 3, sequence: 2731, bad_value: 2097155, minor_opcode: 0, major_opcode: 40, extension_name: None, request_name: Some(\"TranslateCoordinates\") })";
        assert_eq!(gone(X11, message), Some(Gone::Window));
        let size = "called `Result::unwrap()` on an `Err` value: X11(X11Error { error_kind: Drawable, error_code: 9, sequence: 5810, bad_value: 2097155, minor_opcode: 0, major_opcode: 14, extension_name: None, request_name: Some(\"GetGeometry\") })";
        assert_eq!(gone(X11, size), Some(Gone::Window), "asked for its size");
    }

    #[test]
    fn other_panics_are_still_crashes() {
        assert_eq!(
            gone(
                X11,
                "called `Result::unwrap()` on an `Err` value: X11(X11Error { error_kind: Value, error_code: 2 })"
            ),
            None,
            "another X error"
        );
        assert_eq!(
            gone("crates/gui/src/app/bus.rs", "Connection(IoError(..))"),
            None,
            "Weir's own code"
        );
        assert_eq!(
            gone(
                "/x/winit-0.30.13/src/platform_impl/linux/wayland/window/mod.rs",
                "Connection(..)"
            ),
            None,
            "only X11"
        );
    }
}
