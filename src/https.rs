//! Bounded public HTTPS downloads using the Windows TLS and proxy settings.
use anyhow::{Context, Result, ensure};
use std::{
    ffi::c_void,
    ptr::{NonNull, null, null_mut},
    time::{Duration, Instant},
};
use url::Url;
use windows::Win32::Networking::WinHttp::*;
use windows::core::{Error, HSTRING, PCWSTR, w};

struct Handle(NonNull<c_void>);

impl Handle {
    fn new(handle: *mut c_void) -> windows::core::Result<Self> {
        NonNull::new(handle)
            .map(Self)
            .ok_or_else(Error::from_thread)
    }

    fn as_ptr(&self) -> *mut c_void {
        self.0.as_ptr()
    }

    fn option(&self, option: u32, value: u32) -> windows::core::Result<()> {
        // The numeric setting is read during this call.
        unsafe { WinHttpSetOption(Some(self.as_ptr()), option, Some(&value.to_ne_bytes())) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // Each successful WinHTTP open transfers one handle to this owner.
        let _ = unsafe { WinHttpCloseHandle(self.as_ptr()) };
    }
}

pub(crate) fn parse(address: &str) -> Result<Url> {
    ensure!(
        address.len() <= 2048 && !address.chars().any(char::is_control),
        "Invalid HTTPS download URL"
    );
    let url = Url::parse(address).context("Invalid HTTPS download URL")?;
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.port_or_known_default().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "Use an HTTPS download URL without credentials or a fragment"
    );
    Ok(url)
}

/// Read a public HTTPS response under a byte budget and a caller cancellation check.
pub fn get(address: &str, max_bytes: usize, cancelled: impl Fn() -> bool) -> Result<Vec<u8>> {
    let url = parse(address)?;
    let deadline = Instant::now() + Duration::from_secs(45);
    let check = || {
        ensure!(!cancelled(), "HTTPS download cancelled");
        ensure!(Instant::now() < deadline, "HTTPS download timed out");
        Ok::<_, anyhow::Error>(())
    };
    check()?;
    let session = Handle::new(unsafe {
        WinHttpOpen(
            w!("w11term"),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    })
    .context("Opening the Windows HTTPS session")?;
    // Allow CDN redirects and retain normal HTTPS certificate validation.
    session.option(
        WINHTTP_OPTION_REDIRECT_POLICY,
        WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP,
    )?;
    session.option(WINHTTP_OPTION_REJECT_USERPWD_IN_URL, 1)?;
    unsafe { WinHttpSetTimeouts(session.as_ptr(), 8_000, 8_000, 8_000, 10_000) }?;
    let host = HSTRING::from(url.host_str().expect("validated host"));
    let port = url.port_or_known_default().expect("validated HTTPS port");
    let server = Handle::new(unsafe { WinHttpConnect(session.as_ptr(), &host, port, 0) })
        .context("Connecting to the download host")?;
    let object = if let Some(query) = url.query() {
        format!("{}?{query}", url.path())
    } else {
        url.path().to_owned()
    };
    let object = HSTRING::from(object);
    let request = Handle::new(unsafe {
        WinHttpOpenRequest(
            server.as_ptr(),
            w!("GET"),
            &object,
            PCWSTR::null(),
            PCWSTR::null(),
            null(),
            WINHTTP_FLAG_SECURE,
        )
    })
    .context("Opening the HTTPS download request")?;
    request.option(
        WINHTTP_OPTION_DISABLE_FEATURE,
        WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_AUTHENTICATION,
    )?;
    let mut status = 0u32;
    let mut status_size = size_of::<u32>() as u32;
    let mut index = 0;
    check()?;
    // All buffer sizes match the pointed-to variables. WinHTTP uses the live
    // request handle synchronously and does not retain those pointers.
    unsafe {
        WinHttpSendRequest(request.as_ptr(), None, None, 0, 0, 0)?;
        WinHttpReceiveResponse(request.as_ptr(), null_mut())?;
        WinHttpQueryHeaders(
            request.as_ptr(),
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut status_size,
            &mut index,
        )?;
    }
    ensure!(status == 200, "Download returned HTTP {status}");

    let mut archive = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        check()?;
        let mut length = 0;
        // The fixed-size destination remains valid for the synchronous read.
        unsafe {
            WinHttpReadData(
                request.as_ptr(),
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut length,
            )?;
        }
        if length == 0 {
            check()?;
            return Ok(archive);
        }
        let length = length as usize;
        ensure!(
            length <= chunk.len() && length <= max_bytes.saturating_sub(archive.len()),
            "HTTPS response exceeds the download byte limit"
        );
        archive.extend_from_slice(&chunk[..length]);
    }
}
