//! WinHTTP transport, Windows only: no bundled TLS stack, and it picks up the
//! machine's proxy configuration for free.

use super::Endpoint;
use std::ffi::c_void;
use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

/// Closes a WinHTTP handle when it goes out of scope.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// UTF-16 without a NUL, for the APIs that take an explicit length.
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().collect()
}

/// UTF-16 with a NUL, for the APIs that do not.
fn wide_z(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_os_error(what: &str) -> String {
    format!("{what} failed: {}", std::io::Error::last_os_error())
}

pub fn post(
    endpoint: &Endpoint,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<(u16, String), String> {
    let agent = wide_z(concat!("freespeak/", env!("CARGO_PKG_VERSION")));
    let host = wide_z(&endpoint.host);
    let verb = wide_z("POST");
    let object = wide_z(&endpoint.object);

    let mut joined = String::new();
    for (name, value) in headers {
        joined.push_str(name);
        joined.push_str(": ");
        joined.push_str(value);
        joined.push_str("\r\n");
    }
    let header_block = wide(&joined);

    let flags = if endpoint.secure {
        WINHTTP_FLAG_SECURE
    } else {
        Default::default()
    };

    unsafe {
        let session = WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return Err(last_os_error("WinHttpOpen"));
        }
        let session = Handle(session);
        // resolve, connect, send, receive - so a dead network cannot hang a paste.
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 30_000, 90_000);

        let connect = WinHttpConnect(session.0, PCWSTR(host.as_ptr()), endpoint.port, 0);
        if connect.is_null() {
            return Err(last_os_error("WinHttpConnect"));
        }
        let connect = Handle(connect);

        let request = WinHttpOpenRequest(
            connect.0,
            PCWSTR(verb.as_ptr()),
            PCWSTR(object.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            flags,
        );
        if request.is_null() {
            return Err(last_os_error("WinHttpOpenRequest"));
        }
        let request = Handle(request);

        WinHttpSendRequest(
            request.0,
            Some(&header_block),
            Some(body.as_ptr() as *const c_void),
            body.len() as u32,
            body.len() as u32,
            0,
        )
        .map_err(|e| format!("could not send the request: {e}"))?;

        WinHttpReceiveResponse(request.0, std::ptr::null_mut())
            .map_err(|e| format!("no response from {}: {e}", endpoint.host))?;

        let mut status: u32 = 0;
        let mut status_length = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut status_length,
            std::ptr::null_mut(),
        )
        .map_err(|e| format!("could not read the HTTP status: {e}"))?;

        let mut raw = Vec::new();
        loop {
            let mut available: u32 = 0;
            WinHttpQueryDataAvailable(request.0, &mut available)
                .map_err(|e| format!("error while reading the response: {e}"))?;
            if available == 0 {
                break;
            }
            let mut chunk = vec![0u8; available as usize];
            let mut read: u32 = 0;
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr() as *mut c_void,
                available,
                &mut read,
            )
            .map_err(|e| format!("error while reading the response: {e}"))?;
            if read == 0 {
                break;
            }
            chunk.truncate(read as usize);
            raw.extend_from_slice(&chunk);
        }

        Ok((status as u16, String::from_utf8_lossy(&raw).into_owned()))
    }
}
