// SPDX-License-Identifier: MIT
//! GSS-Negotiate (SPNEGO) authentication for HTTP (RFC 4559).
//!
//! A [`Negotiator`] produces the token for `Authorization: Negotiate
//! <base64>`. The system implementation loads the GSS-API library at run
//! time (`libgssapi_krb5.so.2` from MIT Kerberos, or Heimdal's
//! `libgssapi.so.3`), so rgit builds and runs without Kerberos: without the
//! library, or without a ticket, every attempt fails and authentication
//! falls back to the credential helpers, as with libcurl.
//!
//! The token is created for the service `HTTP@<host>`
//! (`GSS_C_NT_HOSTBASED_SERVICE`) with the SPNEGO mechanism
//! (1.3.6.1.5.5.2), requesting mutual authentication and the delegation of
//! `http.delegation`.

use crate::options::Delegation;

/// Produces Negotiate tokens.
pub trait Negotiator: Send {
    /// The initial token for the service `HTTP@<host>`, or a message why
    /// none could be made (shown in the curl trace).
    fn token(&mut self, host: &str, delegation: Delegation) -> Result<Vec<u8>, String>;
}

/// The GSS-API library of the system.
pub fn system() -> Box<dyn Negotiator> {
    Box::new(SystemGss)
}

struct SystemGss;

impl Negotiator for SystemGss {
    fn token(&mut self, host: &str, delegation: Delegation) -> Result<Vec<u8>, String> {
        imp::token(host, delegation)
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use std::ffi::{CStr, c_int, c_void};
    use std::sync::OnceLock;

    use crate::options::Delegation;

    #[repr(C)]
    struct OidDesc {
        length: u32,
        elements: *const c_void,
    }

    #[repr(C)]
    struct BufferDesc {
        length: usize,
        value: *mut c_void,
    }

    type ImportName =
        unsafe extern "C" fn(*mut u32, *mut BufferDesc, *const OidDesc, *mut *mut c_void) -> u32;
    type InitSecContext = unsafe extern "C" fn(
        *mut u32,          // minor status
        *mut c_void,       // claimant credential (GSS_C_NO_CREDENTIAL)
        *mut *mut c_void,  // context handle
        *mut c_void,       // target name
        *const OidDesc,    // mechanism
        u32,               // request flags
        u32,               // time request
        *mut c_void,       // channel bindings
        *mut BufferDesc,   // input token
        *mut *mut OidDesc, // actual mechanism
        *mut BufferDesc,   // output token
        *mut u32,          // returned flags
        *mut u32,          // time received
    ) -> u32;
    type ReleaseBuffer = unsafe extern "C" fn(*mut u32, *mut BufferDesc) -> u32;
    type ReleaseName = unsafe extern "C" fn(*mut u32, *mut *mut c_void) -> u32;
    type DeleteSecContext =
        unsafe extern "C" fn(*mut u32, *mut *mut c_void, *mut BufferDesc) -> u32;
    type DisplayStatus = unsafe extern "C" fn(
        *mut u32,
        u32,
        c_int,
        *const OidDesc,
        *mut u32,
        *mut BufferDesc,
    ) -> u32;

    struct Lib {
        import_name: ImportName,
        init_sec_context: InitSecContext,
        release_buffer: ReleaseBuffer,
        release_name: ReleaseName,
        delete_sec_context: DeleteSecContext,
        display_status: DisplayStatus,
    }

    // SAFETY: the table holds plain function pointers of a library that
    // stays loaded for the life of the process.
    unsafe impl Send for Lib {}
    unsafe impl Sync for Lib {}

    const LIBRARIES: [&CStr; 3] = [
        c"libgssapi_krb5.so.2",
        c"libgssapi_krb5.so",
        c"libgssapi.so.3",
    ];

    /// 1.2.840.113554.1.2.1.4 (`GSS_C_NT_HOSTBASED_SERVICE`).
    static HOSTBASED: [u8; 10] = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x12, 0x01, 0x02, 0x01, 0x04];
    /// 1.3.6.1.5.5.2 (SPNEGO).
    static SPNEGO: [u8; 6] = [0x2b, 0x06, 0x01, 0x05, 0x05, 0x02];

    const GSS_C_DELEG_FLAG: u32 = 1;
    const GSS_C_MUTUAL_FLAG: u32 = 2;
    const GSS_C_DELEG_POLICY_FLAG: u32 = 0x8000;
    const GSS_C_GSS_CODE: c_int = 1;
    const GSS_C_MECH_CODE: c_int = 2;

    fn lib() -> Option<&'static Lib> {
        static LIB: OnceLock<Option<Lib>> = OnceLock::new();
        LIB.get_or_init(load).as_ref()
    }

    fn load() -> Option<Lib> {
        for name in LIBRARIES {
            // SAFETY: dlopen with a valid C string; the handle is never
            // closed, so the symbols stay valid.
            let h = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if h.is_null() {
                continue;
            }
            let sym = |n: &CStr| {
                // SAFETY: dlsym on a live handle with a valid C string.
                let p = unsafe { libc::dlsym(h, n.as_ptr()) };
                (!p.is_null()).then_some(p)
            };
            let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) = (
                sym(c"gss_import_name"),
                sym(c"gss_init_sec_context"),
                sym(c"gss_release_buffer"),
                sym(c"gss_release_name"),
                sym(c"gss_delete_sec_context"),
                sym(c"gss_display_status"),
            ) else {
                continue;
            };
            // SAFETY: the symbols have the GSS-API C signatures (RFC 2744).
            unsafe {
                return Some(Lib {
                    import_name: std::mem::transmute::<*mut c_void, ImportName>(a),
                    init_sec_context: std::mem::transmute::<*mut c_void, InitSecContext>(b),
                    release_buffer: std::mem::transmute::<*mut c_void, ReleaseBuffer>(c),
                    release_name: std::mem::transmute::<*mut c_void, ReleaseName>(d),
                    delete_sec_context: std::mem::transmute::<*mut c_void, DeleteSecContext>(e),
                    display_status: std::mem::transmute::<*mut c_void, DisplayStatus>(f),
                });
            }
        }
        None
    }

    /// The messages of a status code (`gss_display_status` loop).
    fn status_text(l: &Lib, code: u32, kind: c_int) -> String {
        let mut out = Vec::new();
        let mut ctx = 0u32;
        loop {
            let mut minor = 0u32;
            let mut buf = BufferDesc {
                length: 0,
                value: std::ptr::null_mut(),
            };
            // SAFETY: valid out-pointers; the buffer is released below.
            let major = unsafe {
                (l.display_status)(&mut minor, code, kind, std::ptr::null(), &mut ctx, &mut buf)
            };
            if major != 0 {
                break;
            }
            if !buf.value.is_null() {
                // SAFETY: the library returned `length` bytes at `value`.
                let s = unsafe { std::slice::from_raw_parts(buf.value as *const u8, buf.length) };
                out.push(
                    String::from_utf8_lossy(s)
                        .trim_end_matches('\0')
                        .to_string(),
                );
                // SAFETY: releasing a buffer the library allocated.
                unsafe { (l.release_buffer)(&mut minor, &mut buf) };
            }
            if ctx == 0 {
                break;
            }
        }
        out.join(". ")
    }

    pub(super) fn token(host: &str, delegation: Delegation) -> Result<Vec<u8>, String> {
        let l = lib().ok_or_else(|| "GSS-API library not available".to_string())?;
        let service = format!("HTTP@{host}");
        let mut name_buf = BufferDesc {
            length: service.len(),
            value: service.as_ptr() as *mut c_void,
        };
        let hostbased = OidDesc {
            length: HOSTBASED.len() as u32,
            elements: HOSTBASED.as_ptr() as *const c_void,
        };
        let spnego = OidDesc {
            length: SPNEGO.len() as u32,
            elements: SPNEGO.as_ptr() as *const c_void,
        };
        let mut minor = 0u32;
        let mut name: *mut c_void = std::ptr::null_mut();
        // SAFETY: the buffer and OID point to live data for the call.
        let major = unsafe { (l.import_name)(&mut minor, &mut name_buf, &hostbased, &mut name) };
        if major >> 16 != 0 {
            return Err(format!(
                "gss_import_name() failed: {}",
                status_text(l, major, GSS_C_GSS_CODE)
            ));
        }
        let flags = GSS_C_MUTUAL_FLAG
            | match delegation {
                Delegation::None => 0,
                Delegation::Policy => GSS_C_DELEG_POLICY_FLAG,
                Delegation::Always => GSS_C_DELEG_FLAG,
            };
        let mut ctx: *mut c_void = std::ptr::null_mut();
        let mut out = BufferDesc {
            length: 0,
            value: std::ptr::null_mut(),
        };
        let mut ret_flags = 0u32;
        let mut time_rec = 0u32;
        // SAFETY: all pointers are valid for the call; no input token, no
        // channel bindings, default credentials.
        let major = unsafe {
            (l.init_sec_context)(
                &mut minor,
                std::ptr::null_mut(),
                &mut ctx,
                name,
                &spnego,
                flags,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut out,
                &mut ret_flags,
                &mut time_rec,
            )
        };
        let token = if out.value.is_null() {
            Vec::new()
        } else {
            // SAFETY: the library returned `length` bytes at `value`.
            unsafe { std::slice::from_raw_parts(out.value as *const u8, out.length) }.to_vec()
        };
        let mut m2 = 0u32;
        // SAFETY: releasing what the library allocated; null handles are
        // accepted.
        unsafe {
            if !out.value.is_null() {
                (l.release_buffer)(&mut m2, &mut out);
            }
            if !ctx.is_null() {
                (l.delete_sec_context)(&mut m2, &mut ctx, std::ptr::null_mut());
            }
            (l.release_name)(&mut m2, &mut name);
        }
        if major >> 16 != 0 || token.is_empty() {
            let mut msg = status_text(l, major, GSS_C_GSS_CODE);
            let mech = status_text(l, minor, GSS_C_MECH_CODE);
            if !mech.is_empty() {
                msg = format!("{msg}. {mech}");
            }
            return Err(format!("gss_init_sec_context() failed: {msg}"));
        }
        Ok(token)
    }
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
mod imp {
    use crate::options::Delegation;

    pub(super) fn token(_host: &str, _delegation: Delegation) -> Result<Vec<u8>, String> {
        Err("GSS-Negotiate is not supported on this platform".to_string())
    }
}
