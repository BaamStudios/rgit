// SPDX-License-Identifier: MIT
//! Smart HTTP transport client for rgit.
//!
//! A pure Rust stack (decision on issue #162): a small blocking HTTP/1.1
//! client on `std::net` with TLS from rustls (ring provider) and trust
//! roots from the operating system (`rustls-native-certs`). No libcurl.
//!
//! - [`SmartHttp`]: discovery (`GET info/refs?service=...`) and stateless
//!   POSTs to `git-upload-pack` / `git-receive-pack`, with `Git-Protocol`,
//!   gzip request bodies, chunked transfer above `http.postBuffer`,
//!   redirects (`http.followRedirects`) and authentication through a
//!   [`CredentialProvider`].
//! - [`HttpOptions`]: `http.*` and `http.<url>.*` configuration and the
//!   environment overrides (`GIT_SSL_NO_VERIFY`, proxies, ...).
//! - `GIT_TRACE_CURL` / `GIT_CURL_VERBOSE` lines in libcurl's format, with
//!   `GIT_TRACE_REDACT` and `GIT_TRACE_CURL_NO_DATA`.
//!
//! Behaviour follows `Documentation/gitprotocol-http.adoc`,
//! `gitprotocol-v2.adoc` ("HTTP Transport"), `config/http.adoc` and
//! `git.adoc`, and observations of the git 2.56.0 binary against
//! `git http-backend`. Dumb HTTP is not supported (issue #186): a dumb
//! server gives [`Error::DumbNotSupported`].

pub mod auth;
mod client;
mod error;
pub mod messages;
pub mod options;
mod smart;
mod tls;
mod trace;
mod transport;
pub mod url;

pub use auth::{Credential, CredentialProvider};
pub use error::{Error, Result, Transport};
pub use options::{FollowRedirects, HttpOptions, HttpVersion, Proxy};
pub use smart::{Advertisement, RpcBody, RpcResponse, Service, SmartHttp};
pub use transport::{HttpTransport, transport_for};
pub use url::{Scheme, Url};
