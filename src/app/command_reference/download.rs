use super::catalog::MAX_ARCHIVE;
use anyhow::{Context, Result, ensure};
use std::{
    ffi::c_void,
    ptr::{NonNull, null, null_mut},
    time::{Duration, Instant},
};
use windows::Win32::Networking::WinHttp::*;
use windows::core::{Error, PCWSTR, w};

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
}

impl Drop for Handle {
    fn drop(&mut self) {
        // Each successful WinHTTP open transfers one handle to this owner.
        let _ = unsafe { WinHttpCloseHandle(self.as_ptr()) };
    }
}

pub(super) fn latest(cancelled: impl Fn() -> bool) -> Result<Vec<u8>> {
    let deadline = Instant::now() + Duration::from_secs(45);
    ensure!(!cancelled(), "Command reference update cancelled");
    // The URL is fixed. WinHTTP validates HTTPS certificates and defaults to
    // refusing HTTPS-to-HTTP redirects when following GitHub's CDN redirect.
    let session = Handle::new(unsafe {
        WinHttpOpen(
            w!("w11term Command reference"),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    })
    .context("Opening the Windows HTTPS session")?;
    unsafe { WinHttpSetTimeouts(session.as_ptr(), 8_000, 8_000, 8_000, 10_000) }?;
    let server = Handle::new(unsafe { WinHttpConnect(session.as_ptr(), w!("github.com"), 443, 0) })
        .context("Connecting to GitHub")?;
    let request = Handle::new(unsafe {
        WinHttpOpenRequest(
            server.as_ptr(),
            w!("GET"),
            w!("/tldr-pages/tldr/releases/latest/download/tldr-pages.en.zip"),
            PCWSTR::null(),
            PCWSTR::null(),
            null(),
            WINHTTP_FLAG_SECURE,
        )
    })
    .context("Opening the TLDR archive request")?;
    let mut status = 0u32;
    let mut status_size = size_of::<u32>() as u32;
    let mut index = 0;
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
    ensure!(
        status == 200,
        "TLDR archive download returned HTTP {status}"
    );

    let mut archive = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        ensure!(!cancelled(), "Command reference update cancelled");
        ensure!(
            Instant::now() < deadline,
            "Command reference download timed out"
        );
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
            return Ok(archive);
        }
        let length = length as usize;
        ensure!(
            length <= chunk.len() && archive.len() + length <= MAX_ARCHIVE,
            "Command reference archive is too large"
        );
        archive.extend_from_slice(&chunk[..length]);
    }
}
