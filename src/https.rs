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
    let (status, bytes) = request(&url, Request::Download, max_bytes, cancelled)?;
    ensure!(status == 200, "Download returned HTTP {status}");
    Ok(bytes)
}

pub(crate) enum Request<'a> {
    Download,
    Json {
        headers: &'a [u16],
        body: Option<&'a [u8]>,
    },
}

pub(crate) fn request(
    url: &Url,
    request: Request<'_>,
    max_bytes: usize,
    cancelled: impl Fn() -> bool,
) -> Result<(u32, Vec<u8>)> {
    let timeout = Duration::from_secs(if matches!(request, Request::Download) {
        45
    } else {
        60
    });
    let mut body = Vec::new();
    let (status, _) = request_into(url, request, max_bytes, timeout, cancelled, |bytes| {
        body.extend_from_slice(bytes);
        Ok(())
    })?;
    Ok((status, body))
}

pub(crate) fn download_into(
    url: &Url,
    max_bytes: usize,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
    consume: impl FnMut(&[u8]) -> Result<()>,
) -> Result<usize> {
    let (status, received) = request_into(
        url,
        Request::Download,
        max_bytes,
        timeout,
        cancelled,
        consume,
    )?;
    ensure!(status == 200, "Download returned HTTP {status}");
    Ok(received)
}

fn request_into(
    url: &Url,
    request: Request<'_>,
    max_bytes: usize,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
    mut consume: impl FnMut(&[u8]) -> Result<()>,
) -> Result<(u32, usize)> {
    let download = matches!(request, Request::Download);
    let deadline = Instant::now() + timeout;
    let check = || {
        ensure!(!cancelled(), "HTTPS request cancelled");
        ensure!(Instant::now() < deadline, "HTTPS request timed out");
        Ok::<_, anyhow::Error>(())
    };
    check()?;
    let proxy = if url.scheme() == "https" {
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY
    } else {
        WINHTTP_ACCESS_TYPE_NO_PROXY
    };
    let session = Handle::new(unsafe {
        WinHttpOpen(w!("w11term"), proxy, PCWSTR::null(), PCWSTR::null(), 0)
    })
    .context("Opening the Windows HTTPS session")?;
    // Allow CDN redirects and retain normal HTTPS certificate validation.
    session.option(
        WINHTTP_OPTION_REDIRECT_POLICY,
        if download {
            WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP
        } else {
            WINHTTP_OPTION_REDIRECT_POLICY_NEVER
        },
    )?;
    session.option(WINHTTP_OPTION_REJECT_USERPWD_IN_URL, 1)?;
    unsafe {
        WinHttpSetTimeouts(
            session.as_ptr(),
            8_000,
            8_000,
            8_000,
            if download { 10_000 } else { 30_000 },
        )
    }?;
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
    let (headers, body) = match request {
        Request::Download => (None, None),
        Request::Json { headers, body } => (Some(headers), body),
    };
    let method = if body.is_some() {
        w!("POST")
    } else {
        w!("GET")
    };
    let request = Handle::new(unsafe {
        WinHttpOpenRequest(
            server.as_ptr(),
            method,
            &object,
            PCWSTR::null(),
            PCWSTR::null(),
            null(),
            if url.scheme() == "https" {
                WINHTTP_FLAG_SECURE
            } else {
                WINHTTP_OPEN_REQUEST_FLAGS(0)
            },
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
        let size = body.map_or(Ok(0), |body| u32::try_from(body.len()))?;
        WinHttpSendRequest(
            request.as_ptr(),
            headers,
            body.map(|body| body.as_ptr().cast()),
            size,
            size,
            0,
        )?;
        check()?;
        WinHttpReceiveResponse(request.as_ptr(), null_mut())?;
        check()?;
        WinHttpQueryHeaders(
            request.as_ptr(),
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut status_size,
            &mut index,
        )?;
    }
    if status != 200 {
        // Error bodies may echo user data or credentials. Callers classify status codes.
        return Ok((status, 0));
    }

    let mut received = 0;
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
            return Ok((status, received));
        }
        let length = length as usize;
        ensure!(
            length <= chunk.len() && length <= max_bytes.saturating_sub(received),
            "HTTPS response exceeds the download byte limit"
        );
        consume(&chunk[..length])?;
        received += length;
    }
}
