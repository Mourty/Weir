//! Running from an AppImage: one file holding Weir's programs, which an
//! AppImage's own starter mounts and runs. Nothing is installed, so the
//! things an installed Weir finds in fixed places (its login service, its
//! menu entry, the way to update it) have to name the AppImage file
//! instead, and the service and the window both need to know it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The AppImage this program runs from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppImage {
    /// The AppImage file itself, such as `~/Applications/Weir-1.5.0-x86_64.AppImage`.
    pub file: PathBuf,
    /// Where its contents are mounted while it runs.
    pub dir: PathBuf,
}

/// The AppImage this program runs from, if it does.
///
/// The AppImage's starter says so in `APPIMAGE` and `APPDIR`, but every
/// program started from it inherits them, including a terminal opened from
/// some other AppImage. So they count only when this program's own file is
/// inside `APPDIR`.
pub fn appimage() -> Option<AppImage> {
    let exe = crate::own_exe().ok()?;
    appimage_from(
        std::env::var_os("APPIMAGE"),
        std::env::var_os("APPDIR"),
        &exe,
    )
}

fn appimage_from(file: Option<OsString>, dir: Option<OsString>, exe: &Path) -> Option<AppImage> {
    let file = PathBuf::from(file.filter(|f| !f.is_empty())?);
    let dir = PathBuf::from(dir.filter(|d| !d.is_empty())?);
    (file.is_absolute() && dir.is_absolute() && exe.starts_with(&dir))
        .then_some(AppImage { file, dir })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn only_a_program_inside_the_mount_runs_from_the_appimage() {
        let exe = Path::new("/tmp/.mount_WeirAb12/usr/bin/weir-daemon");
        assert_eq!(
            appimage_from(
                os("/home/me/Apps/Weir-x86_64.AppImage"),
                os("/tmp/.mount_WeirAb12"),
                exe
            ),
            Some(AppImage {
                file: "/home/me/Apps/Weir-x86_64.AppImage".into(),
                dir: "/tmp/.mount_WeirAb12".into(),
            })
        );
        // Started from a terminal that another AppImage opened.
        assert_eq!(
            appimage_from(
                os("/home/me/Apps/Editor.AppImage"),
                os("/tmp/.mount_EditXy"),
                Path::new("/usr/bin/weir-daemon")
            ),
            None
        );
        assert_eq!(appimage_from(None, os("/tmp/.mount_WeirAb12"), exe), None);
        assert_eq!(
            appimage_from(os("Weir.AppImage"), os("/tmp/.mount_WeirAb12"), exe),
            None,
            "a relative path is no use once the folder changes"
        );
    }
}
