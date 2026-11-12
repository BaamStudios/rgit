// SPDX-License-Identifier: MIT
//! User-visible warning texts of the trace subsystem, kept in one place
//! (see CLEANROOM.md: short functional strings reproduced for
//! compatibility, as observed from git 2.56.0).

use std::io;

/// Warning for a value that is neither on/off, an fd nor an absolute path.
pub fn unknown_value(key: &str, value: &str) -> String {
    format!(
        "warning: unknown trace value for '{key}': {value}\n         \
         If you want to trace into a file, then please set {key}\n         \
         to an absolute pathname (starting with /)\n"
    )
}

/// Warning when the trace file cannot be opened.
pub fn cannot_open(path: &str, err: &io::Error) -> String {
    format!(
        "warning: could not open '{path}' for tracing: {}\n",
        strerror(err)
    )
}

/// Warning when writing a trace line failed; the key is disabled afterwards.
pub fn cannot_write(key: &str, err: &io::Error) -> String {
    format!(
        "warning: unable to write trace for {key}: {}\n",
        strerror(err)
    )
}

/// The C library style error text: `io::Error`'s display without the
/// trailing ` (os error N)`.
pub fn strerror(err: &io::Error) -> String {
    let s = err.to_string();
    match s.rfind(" (os error ") {
        Some(i) if s.ends_with(')') => s[..i].to_string(),
        _ => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strerror_strips_code() {
        let e = io::Error::from_raw_os_error(2);
        assert_eq!(strerror(&e), "No such file or directory");
        let e = io::Error::other("custom");
        assert_eq!(strerror(&e), "custom");
    }

    #[test]
    fn unknown_value_text() {
        assert_eq!(
            unknown_value("GIT_TRACE", "yes"),
            "warning: unknown trace value for 'GIT_TRACE': yes\n\
             \x20        If you want to trace into a file, then please set GIT_TRACE\n\
             \x20        to an absolute pathname (starting with /)\n"
        );
    }
}
