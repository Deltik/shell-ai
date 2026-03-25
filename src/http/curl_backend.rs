use serde_json::Value;
use std::ffi::{c_char, c_long, c_void, CString};
use std::io::{BufReader, Read};
use std::sync::mpsc;

use super::curl_ffi::{self, CurlLibrary};
use super::{HttpError, SseStream, STREAM_TIMEOUT_SECS};

/// Header info extracted from the HTTP response.
struct HeaderInfo {
    status: u16,
    retry_after_secs: Option<u64>,
}

/// State passed to the header callback via userdata pointer.
struct HeaderCallbackState {
    status_code: u16,
    retry_after: Option<u64>,
    header_tx: Option<mpsc::SyncSender<HeaderInfo>>,
}

impl Drop for HeaderCallbackState {
    fn drop(&mut self) {
        // Send header info for responses where the header callback saw a status line
        // but the blank-line signal was missed (e.g., zero-body error responses).
        // If status_code is still 0, the transfer failed before any HTTP response
        // was received — don't send fake header info; let the channel close so the
        // caller gets a proper network error.
        if self.status_code > 0 {
            if let Some(tx) = self.header_tx.take() {
                let _ = tx.send(HeaderInfo {
                    status: self.status_code,
                    retry_after_secs: self.retry_after,
                });
            }
        }
    }
}

/// Bridges curl's push-based write callback to a pull-based Read interface.
struct CurlStreamReader {
    receiver: mpsc::Receiver<Vec<u8>>,
    buffer: Vec<u8>,
    offset: usize,
}

impl Read for CurlStreamReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // Drain remaining buffer first
        if self.offset < self.buffer.len() {
            let n = std::cmp::min(buf.len(), self.buffer.len() - self.offset);
            buf[..n].copy_from_slice(&self.buffer[self.offset..self.offset + n]);
            self.offset += n;
            return Ok(n);
        }

        // Wait for next chunk from curl
        match self.receiver.recv() {
            Ok(chunk) => {
                let n = std::cmp::min(buf.len(), chunk.len());
                buf[..n].copy_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    self.buffer = chunk;
                    self.offset = n;
                } else {
                    self.buffer.clear();
                    self.offset = 0;
                }
                Ok(n)
            }
            Err(_) => Ok(0), // Channel closed = EOF
        }
    }
}

/// curl write callback: receives body data chunks and sends them to the channel.
///
/// Signature: size_t write_callback(char *ptr, size_t size, size_t nmemb, void *userdata)
unsafe extern "C" fn write_callback(
    ptr: *mut c_char,
    size: usize,
    nmemb: usize,
    userdata: *mut c_void,
) -> usize {
    let total = size * nmemb;
    if total == 0 {
        return 0;
    }
    let data = std::slice::from_raw_parts(ptr as *const u8, total);
    let sender = &*(userdata as *const mpsc::SyncSender<Vec<u8>>);
    match sender.send(data.to_vec()) {
        Ok(()) => total,
        Err(_) => 0, // Reader dropped, abort transfer
    }
}

/// curl header callback: receives header lines and extracts status + Retry-After.
///
/// Signature: size_t header_callback(char *buffer, size_t size, size_t nitems, void *userdata)
unsafe extern "C" fn header_callback(
    buffer: *mut c_char,
    size: usize,
    nitems: usize,
    userdata: *mut c_void,
) -> usize {
    let total = size * nitems;
    let data = std::slice::from_raw_parts(buffer as *const u8, total);
    let line = String::from_utf8_lossy(data);
    let trimmed = line.trim();

    let state = &mut *(userdata as *mut HeaderCallbackState);

    if trimmed.starts_with("HTTP/") {
        // Parse status line: "HTTP/2 200" or "HTTP/1.1 200 OK"
        if let Some(code_str) = trimmed.split_whitespace().nth(1) {
            if let Ok(code) = code_str.parse::<u16>() {
                state.status_code = code;
            }
        }
    } else if trimmed.is_empty() {
        // Blank line = end of headers. Send status info to main thread.
        if let Some(tx) = state.header_tx.take() {
            let _ = tx.send(HeaderInfo {
                status: state.status_code,
                retry_after_secs: state.retry_after,
            });
        }
    } else {
        // Parse Retry-After header (case-insensitive)
        let lower = trimmed.to_lowercase();
        if let Some(value) = lower.strip_prefix("retry-after:") {
            if let Ok(secs) = value.trim().parse::<u64>() {
                state.retry_after = Some(secs);
            }
        }
    }

    total
}

/// RAII guard that cleans up a curl easy handle and slist on drop.
struct CurlHandle<'a> {
    curl: &'a CurlLibrary,
    handle: *mut c_void,
    headers: *mut c_void,
}

impl<'a> Drop for CurlHandle<'a> {
    fn drop(&mut self) {
        if !self.headers.is_null() {
            unsafe { (self.curl.slist_free_all)(self.headers) };
        }
        unsafe { (self.curl.easy_cleanup)(self.handle) };
    }
}

/// Send a POST request for SSE streaming using libcurl-impersonate.
pub fn post_json_streaming(
    curl: &'static CurlLibrary,
    url: &str,
    bearer_token: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: &Value,
) -> Result<SseStream, HttpError> {
    let body_bytes = serde_json::to_vec(body)
        .map_err(|e| HttpError::Network(format!("Failed to serialize request body: {}", e)))?;
    let url_c = CString::new(url)
        .map_err(|_| HttpError::Config("URL contains null byte".into()))?;

    // Create channels
    let (body_tx, body_rx) = mpsc::sync_channel::<Vec<u8>>(64);
    let (header_tx, header_rx) = mpsc::sync_channel::<HeaderInfo>(1);

    // Build header list
    let mut header_strings: Vec<CString> = Vec::new();
    header_strings.push(CString::new("Content-Type: application/json").unwrap());
    header_strings.push(CString::new("Accept: text/event-stream").unwrap());

    if let Some(token) = bearer_token {
        header_strings.push(
            CString::new(format!("Authorization: Bearer {}", token))
                .map_err(|_| HttpError::Config("Bearer token contains null byte".into()))?,
        );
    }

    for (k, v) in extra_headers {
        header_strings.push(
            CString::new(format!("{}: {}", k, v))
                .map_err(|_| HttpError::Config("Header contains null byte".into()))?,
        );
    }

    // Everything is prepared. Spawn the transfer on a background thread.
    // The thread owns the curl handle and runs perform() synchronously.
    std::thread::spawn(move || {
        let handle = unsafe { (curl.easy_init)() };
        if handle.is_null() {
            log::error!("curl_easy_init returned NULL");
            return;
        }

        let mut curl_handle = CurlHandle {
            curl,
            handle,
            headers: std::ptr::null_mut(),
        };

        unsafe {
            // URL
            (curl.easy_setopt_ptr)(handle, curl_ffi::CURLOPT_URL, url_c.as_ptr() as *const c_void);

            // POST
            (curl.easy_setopt_long)(handle, curl_ffi::CURLOPT_POST, 1);

            // POST body (curl copies the data with CURLOPT_POSTFIELDS when POSTFIELDSIZE is set)
            (curl.easy_setopt_ptr)(
                handle,
                curl_ffi::CURLOPT_POSTFIELDS,
                body_bytes.as_ptr() as *const c_void,
            );
            (curl.easy_setopt_long)(
                handle,
                curl_ffi::CURLOPT_POSTFIELDSIZE_LARGE,
                body_bytes.len() as c_long,
            );

            // Headers
            let mut slist: *mut c_void = std::ptr::null_mut();
            for h in &header_strings {
                slist = (curl.slist_append)(slist, h.as_ptr());
            }
            (curl.easy_setopt_ptr)(handle, curl_ffi::CURLOPT_HTTPHEADER, slist);
            curl_handle.headers = slist;

            // Write callback + userdata
            (curl.easy_setopt_ptr)(
                handle,
                curl_ffi::CURLOPT_WRITEFUNCTION,
                write_callback as *const c_void,
            );
            (curl.easy_setopt_ptr)(
                handle,
                curl_ffi::CURLOPT_WRITEDATA,
                &body_tx as *const mpsc::SyncSender<Vec<u8>> as *const c_void,
            );

            // Header callback + userdata
            let mut header_state = HeaderCallbackState {
                status_code: 0,
                retry_after: None,
                header_tx: Some(header_tx),
            };
            (curl.easy_setopt_ptr)(
                handle,
                curl_ffi::CURLOPT_HEADERFUNCTION,
                header_callback as *const c_void,
            );
            (curl.easy_setopt_ptr)(
                handle,
                curl_ffi::CURLOPT_HEADERDATA,
                &mut header_state as *mut HeaderCallbackState as *const c_void,
            );

            // Timeout
            (curl.easy_setopt_long)(
                handle,
                curl_ffi::CURLOPT_TIMEOUT,
                STREAM_TIMEOUT_SECS as c_long,
            );

            // Recommended for multi-threaded programs
            (curl.easy_setopt_long)(handle, curl_ffi::CURLOPT_NOSIGNAL, 1);

            // Perform the transfer (blocks until complete)
            let ret = (curl.easy_perform)(handle);
            if ret != curl_ffi::CURLE_OK {
                log::debug!("curl_easy_perform returned error code {}", ret);
            }

            // header_state is dropped here, which sends header info if not already sent.
            // body_tx is dropped here, closing the channel → reader gets EOF.
            // curl_handle is dropped here, cleaning up the handle and slist.
            drop(header_state);
            drop(body_tx);
            drop(curl_handle);
        }
    });

    // Wait for headers to arrive
    let header_info = header_rx
        .recv()
        .map_err(|_| HttpError::Network("curl transfer failed before headers were received".into()))?;

    let reader: Box<dyn Read + Send> = Box::new(CurlStreamReader {
        receiver: body_rx,
        buffer: Vec::new(),
        offset: 0,
    });

    Ok(SseStream {
        reader: BufReader::new(reader),
        status: header_info.status,
        retry_after_secs: header_info.retry_after_secs,
    })
}