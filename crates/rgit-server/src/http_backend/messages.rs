// SPDX-License-Identifier: MIT
//! User-visible messages of `http-backend`, word for word as git 2.56
//! prints them (observed from the binary). They go to standard error,
//! which the web server writes to its error log; only the 415 text is
//! part of the response.

/// `REQUEST_METHOD` is missing.
pub const NO_REQUEST_METHOD: &str = "No REQUEST_METHOD from server";

/// Neither `GIT_PROJECT_ROOT` nor `PATH_TRANSLATED` is set.
pub const NO_PROJECT_ROOT: &str = "No GIT_PROJECT_ROOT or PATH_TRANSLATED from server";

/// `GIT_PROJECT_ROOT` without `PATH_INFO`.
pub const NO_PATH_INFO: &str = "GIT_PROJECT_ROOT is set but PATH_INFO is not";

/// `PATH_INFO` that would leave the project root or is not canonical.
pub fn aliased(path: &str) -> String {
    format!("'{path}': aliased")
}

/// A path that names none of the files or services served.
pub fn request_not_supported(path: &str) -> String {
    format!("Request not supported: '{path}'")
}

/// No repository at the requested place.
pub fn not_a_repository(dir: &str) -> String {
    format!("Not a git repository: '{dir}'")
}

/// Neither `GIT_HTTP_EXPORT_ALL` nor `git-daemon-export-ok`.
pub fn not_exported(dir: &str) -> String {
    format!("Repository not exported: '{dir}'")
}

/// `http.getanyfile` is off.
pub const GETANYFILE_DISABLED: &str = "Unsupported service: getanyfile";

/// `info/refs?service=` names no service.
pub fn unsupported_service(name: &str) -> String {
    format!("Unsupported service: '{name}'")
}

/// `http.uploadpack` / `http.receivepack` is off.
pub fn service_not_enabled(name: &str) -> String {
    format!("Service not enabled: '{name}'")
}

/// A static file that cannot be read.
pub fn cannot_open(path: &str, err: &str) -> String {
    format!("Cannot open '{path}': {err}")
}

/// The body of the 415 response.
pub fn wrong_content_type(expected: &str, got: &str) -> String {
    format!("Expected POST with Content-Type '{expected}', but received '{got}' instead.\n")
}

/// `CONTENT_LENGTH` is not a number.
pub fn bad_content_length(value: &str) -> String {
    format!("failed to parse CONTENT_LENGTH: {value}")
}

/// `GIT_HTTP_MAX_REQUEST_BUFFER` is not a size.
pub const BAD_MAX_REQUEST_BUFFER: &str = "failed to parse GIT_HTTP_MAX_REQUEST_BUFFER";

/// An `upload-pack` request announced as larger than the buffer limit.
pub fn request_too_large(max: u64, len: i64) -> String {
    format!(
        "request was larger than our maximum size ({max}): {len}; try setting GIT_HTTP_MAX_REQUEST_BUFFER"
    )
}

/// A gzip body that stops early.
pub const GZIP_TRUNCATED: &str = "request ended in the middle of the gzip stream";

/// zlib's error, as `error: inflate: ...`.
pub fn inflate_error(reason: &str) -> String {
    format!("inflate: {reason}")
}

/// The fatal message after [`inflate_error`].
pub fn inflate_failed(code: i32) -> String {
    format!("zlib error inflating request, result {code}")
}

/// A request body that could not be read (rgit wording; git reports the
/// system error the same way).
pub fn read_failed(err: &str) -> String {
    format!("failed to read request body: {err}")
}

/// The service could not be started (rgit wording).
pub fn cannot_run(service: &str, err: &str) -> String {
    format!("cannot run {service}: {err}")
}

/// `Cache-Control` of a response that must not be cached.
pub const CACHE_CONTROL_NO_CACHE: &str = "no-cache, max-age=0, must-revalidate";

/// `Content-Type` of the reference advertisement of `service`
/// (gitprotocol-http).
pub fn advertisement_content_type(service: &str) -> String {
    format!("application/x-git-{service}-advertisement")
}
