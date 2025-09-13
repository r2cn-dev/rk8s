#[macro_use]
extern crate log;

pub mod overlayfs;
pub mod passthrough;
mod server;
mod util;

// Test utilities (only compiled during tests)
#[cfg(test)]
pub mod test_utils {
    /// Macro: unwrap result or skip test when encountering EPERM (Permission denied).
    ///
    /// Behavior:
    /// - On Ok(v): returns v
    /// - On Err(e) where e -> io::Error has raw_os_error()==EPERM (or PermissionDenied):
    ///     * If env RUN_PRIVILEGED_TESTS=1 -> panic (treat as hard failure)
    ///     * Else: print a line indicating skip and `return` from test (so test counted as ignored when used with #[ignore]).
    /// - On Err(e) other than EPERM -> panic with diagnostic.
    ///
    /// Usage examples:
    /// let handle = unwrap_or_skip_eperm!(some_async_call.await, "mount session");
    #[macro_export]
    macro_rules! unwrap_or_skip_eperm {
        ($expr:expr, $ctx:expr) => {{
            match $expr {
                Ok(v) => v,
                Err(e) => {
                    let ioerr: std::io::Error = e.into();
                    let is_eperm = ioerr.raw_os_error() == Some(libc::EPERM)
                        || ioerr.kind() == std::io::ErrorKind::PermissionDenied;
                    if is_eperm {
                        if std::env::var("RUN_PRIVILEGED_TESTS").ok().as_deref() == Some("1") {
                            panic!(
                                "{} failed with EPERM while RUN_PRIVILEGED_TESTS=1: {:?}",
                                $ctx, ioerr
                            );
                        } else {
                            eprintln!("skip (EPERM) {}: {:?}", $ctx, ioerr);
                            return;
                        }
                    }
                    panic!("{} unexpected error: {:?}", $ctx, ioerr);
                }
            }
        }};
    }
}
