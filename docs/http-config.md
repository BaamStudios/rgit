# HTTP configuration: what rgit supports

rgit's smart HTTP client (`crates/rgit-http`) is its own HTTP/1.1 stack on
`std::net` with TLS from rustls; it does not use libcurl or OpenSSL (decision
on issue #162). This page lists every variable of git's
`Documentation/config/http.adoc` and says whether rgit implements it, accepts
it without an effect, or rejects it, and why. `http.<url>.<key>` works for
every key below with git's URL matching rules (issue #170).

Status: **yes** = implemented as documented; **checked** = the value is
parsed and checked like git (a bad boolean or number is the same fatal
error) but has no effect in rgit; **no** = not supported, with the
behaviour described.

| Variable | Status | Notes |
|---|---|---|
| `http.proxy` | yes | `http://`, `socks4://`, `socks4a://`, `socks5://`, `socks5h://` proxies, `user[:password]@` (a missing password is asked for through the credential helpers), `remote.<name>.proxy`, `http_proxy`, `https_proxy`, `all_proxy`, `no_proxy`. Invalid proxy URLs fail with git's messages (`Invalid proxy URL '...': unsupported proxy scheme '...'`). **no**: `https://` proxies (TLS to the proxy itself) and SOCKS proxies on Unix domain sockets (`socks5://localhost/path`) fail with an error naming the limitation. |
| `http.proxyAuthMethod` | partly | Values are checked like git (`unsupported proxy authentication method ...: using anyauth`), `remote.<name>.proxyAuthMethod` and `GIT_HTTP_PROXY_AUTHMETHOD` are read. Proxy authentication is always Basic: Digest, Negotiate and NTLM are not implemented. |
| `http.proxySSLCert`, `http.proxySSLKey`, `http.proxySSLCertPasswordProtected`, `http.proxySSLCAInfo` | no | Only apply to HTTPS proxies, which rgit does not support (see `http.proxy`). |
| `http.emptyAuth` | checked | rgit implements no GSS-Negotiate (Kerberos), the only scheme that needs empty credentials, so every value behaves like `false`. |
| `http.proactiveAuth` | yes | `basic`, `auto`, `none`; the helper's `authtype`/`credential` are used when it announces `capability[]=authtype`. |
| `http.delegation` | checked | Only meaningful for GSS-API, which rgit does not implement. Unknown values warn like git. |
| `http.extraHeader` | yes | Multi-valued; an empty value resets the list. |
| `http.cookieFile`, `http.saveCookies` | yes | Netscape/Mozilla format and plain `Set-Cookie:` header lines; an empty `http.cookieFile` keeps cookies for the session only. Cookie values are redacted in `GIT_TRACE_CURL` unless `GIT_TRACE_REDACT=0`. |
| `http.version` | partly | `HTTP/1.1` only. `HTTP/2` is accepted and served over HTTP/1.1 (rgit has no HTTP/2 framing); other values warn like git. |
| `http.curloptResolve` | yes | `[+]HOST:PORT:ADDRESS[,ADDRESS]`, `-HOST:PORT`, an empty value resets the list; malformed entries fail with libcurl's message. |
| `http.sslVersion` | partly | `tlsv1.3` restricts to TLS 1.3; `tlsv1`, `tlsv1.0`, `tlsv1.1`, `tlsv1.2` mean "TLS 1.2 or later" (rustls implements nothing older than TLS 1.2); `sslv2`/`sslv3` fail. `GIT_SSL_VERSION` (empty = default) is read. |
| `http.sslCipherList` | partly | OpenSSL or IANA names of the suites rustls implements (ECDHE with AES-GCM or ChaCha20-Poly1305, and the TLS 1.3 suites); a list without TLS 1.3 names leaves TLS 1.3 alone, as OpenSSL does; a list that selects nothing fails with `failed setting cipher list: ...`. `GIT_SSL_CIPHER_LIST` is read. |
| `http.sslVerify` | yes | `GIT_SSL_NO_VERIFY`. |
| `http.sslCert`, `http.sslKey` | yes | PEM certificate chain and PEM private key (PKCS#8, PKCS#1 or SEC1; the key may be in the certificate file). `GIT_SSL_CERT`, `GIT_SSL_KEY`. |
| `http.sslCertPasswordProtected` | no | Encrypted private keys cannot be loaded (rustls has no PEM decryption); loading one fails with `unable to set private key file`. |
| `http.sslCAInfo`, `http.sslCAPath` | yes | PEM files; setting either replaces the system trust store, like libcurl. `GIT_SSL_CAINFO`, `GIT_SSL_CAPATH`. |
| `http.sslBackend` | partly | rgit has one backend, `rustls`; `openssl` is accepted as a synonym so existing configurations keep working. Any other value fails with git's message (`Unsupported SSL backend '...'. Supported SSL backends:`). |
| `http.sslCertType`, `http.sslKeyType` | partly | `PEM` only; `DER`, `P12` and `ENG` fail with `not supported file type`. `GIT_SSL_CERT_TYPE`, `GIT_SSL_KEY_TYPE`. |
| `http.schannelCheckRevoke`, `http.schannelUseSSLCAInfo` | checked | Windows Schannel options; no Schannel backend. |
| `http.pinnedPubkey` | yes | `sha256//<base64>` (several separated by `;`) or a PEM/DER public key file; a mismatch fails with `SSL: public key does not match pinned public key`. |
| `http.sslTry` | checked | FTP only; rgit has no FTP transport. |
| `http.maxRequests`, `http.minSessions` | checked | rgit's smart HTTP client sends one request at a time on one reused connection; dumb HTTP (the parallel object fetcher these tune) is not implemented. |
| `http.postBuffer` | yes | Requests that would not leave room for one more maximum-size packet in the buffer are sent with `Transfer-Encoding: chunked` after a `0000` probe, as git does. |
| `http.lowSpeedLimit`, `http.lowSpeedTime` | yes | `GIT_HTTP_LOW_SPEED_LIMIT`, `GIT_HTTP_LOW_SPEED_TIME`. |
| `http.keepAliveIdle`, `http.keepAliveInterval`, `http.keepAliveCount` | checked | TCP keepalive tuning is not set; connections are reused while the server keeps them open. |
| `http.retryAfter`, `http.maxRetries`, `http.maxRetryTime` | yes | `429` responses are retried with the `Retry-After` delay (seconds or an HTTP date) or `http.retryAfter`; delays above `http.maxRetryTime` fail at once. `GIT_HTTP_RETRY_AFTER`, `GIT_HTTP_MAX_RETRIES`, `GIT_HTTP_MAX_RETRY_TIME`. |
| `http.noEPSV` | checked | FTP only. |
| `http.userAgent` | yes | `GIT_HTTP_USER_AGENT`. |
| `http.followRedirects` | yes | `true`, `false`, `initial`. |

Related settings outside `http.*`:

| Variable | Status | Notes |
|---|---|---|
| `remote.<name>.proxy`, `remote.<name>.proxyAuthMethod` | yes | Applied to the remote whose `url` or `pushurl` is the URL being accessed. |
| `transfer.credentialsInUrl` | yes | `allow`, `warn`, `die` for `user:password@` in any URL used by clone, fetch, push and ls-remote. |
| `credential.*`, `capability[]=authtype`/`state`, `wwwauth[]`, multistage (`continue=1`) | yes | See `gitcredentials(7)`; the HTTP client passes `WWW-Authenticate` challenges to the helpers and uses `authtype`/`credential` answers (Bearer tokens and other schemes). |
| `GIT_TRACE_CURL`, `GIT_CURL_VERBOSE`, `GIT_TRACE_CURL_NO_DATA`, `GIT_TRACE_REDACT` | yes | libcurl's line format. |
| Dumb HTTP (`GIT_SMART_HTTP=0`, servers without smart HTTP) | no | Fails with `... is served by a dumb HTTP server, which rgit does not support`. |
