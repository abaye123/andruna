//! `andruna-sysd --host <package>`: the **placeholder** host process.
//!
//! The real host is `andruna-app.exe` (ART, the framework, the app), which
//! is not built yet. Until it is, sysd starts its own executable in this
//! mode so that the launcher, the pipe protocol and process supervision can
//! run end to end on real processes. The placeholder runs no app code: it
//! logs that it is a placeholder and shows a window that says so, and it
//! exits when the user closes that window. It is removed when andruna-app
//! exists.

use crate::l10n::{self, Lang};
use crate::{Layout, info, sys, warn};

pub fn run(layout: &Layout, package: &str, activity: Option<&str>) -> i32 {
    let _ = crate::log::init(&layout.logs_dir(), "host");
    warn!(
        "placeholder_host",
        package = package,
        activity = activity.unwrap_or("-"),
        detail = "andruna-app does not exist yet; this process runs no app code"
    );
    let lang = Lang::current();
    let title = l10n::text(lang, "host_title", &[("package", package)]);
    let text = l10n::text(lang, "host_text", &[("package", package)]);
    if let Err(e) = sys::show_host_window(&title, &text, lang.is_rtl()) {
        crate::error!("host_window_failed", package = package, error = e);
        return 1;
    }
    sys::run_message_loop();
    info!("placeholder_host_closed", package = package);
    0
}
