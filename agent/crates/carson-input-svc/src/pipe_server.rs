//! The named pipe itself. Security is two layers: the pipe's DACL only admits authenticated
//! (non-anonymous) users, and every connection is then checked against the machine's active
//! console session — so even another logged-on user's session, or a network caller, cannot
//! inject input, only whoever is sitting at this computer right now.
#![allow(unsafe_code)] // Win32 FFI only; justified per call below.

use std::os::windows::io::FromRawHandle;
use std::sync::mpsc::Receiver;

use carson_input::pipe::{BackendCall, PIPE_PATH, read_call};
use carson_input::{Backend, direct_backend};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, HLOCAL, LocalFree,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAGS_AND_ATTRIBUTES, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientSessionId,
    PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows::core::PCWSTR;

use crate::log;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Runs until `stop` fires, serving one client at a time — only the active session's own
/// agent is ever expected to connect.
pub fn run(stop: &Receiver<()>) {
    while stop.try_recv().is_err() {
        if let Err(e) = accept_one() {
            log::line(&format!("pipe session ended: {e:#}"));
        }
    }
}

fn accept_one() -> anyhow::Result<()> {
    let sddl = wide("D:(A;;GA;;;AU)"); // authenticated users only; the session check below is the real gate
    let mut psd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `sddl` is a valid, nul-terminated wide string; `psd` is an out-param this
    // function fills in, freed with LocalFree before returning in every path.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION,
            &mut psd,
            None,
        )?;
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: psd.0,
        bInheritHandle: false.into(),
    };
    let name = wide(PIPE_PATH);
    // SAFETY: `name` and `sa` are valid for the call's duration; the returned handle is
    // owned by this function and is closed on every return path below.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(PIPE_ACCESS_DUPLEX.0 | FILE_FLAG_FIRST_PIPE_INSTANCE.0),
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            4096,
            4096,
            0,
            Some(&sa),
        )
    };
    // SAFETY: `psd` was allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW,
    // which documents LocalFree as the matching release.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(psd.0)));
    }

    if handle.is_invalid() {
        anyhow::bail!("CreateNamedPipeW failed");
    }

    // SAFETY: `handle` is the pipe instance just created above; ConnectNamedPipe blocks
    // until a client connects.
    let connect_result = unsafe { ConnectNamedPipe(handle, None) };
    if connect_result.is_err() {
        // SAFETY: immediately after a failed call, reading the thread's last error is valid.
        let err = unsafe { GetLastError() };
        if err != ERROR_PIPE_CONNECTED {
            // SAFETY: `handle` is a valid, still-owned handle; closing it here is the only
            // release on this path.
            unsafe {
                let _ = CloseHandle(handle);
            }
            anyhow::bail!("ConnectNamedPipe failed: {err:?}");
        }
    }

    let mut client_session = 0u32;
    // SAFETY: `handle` is connected; `client_session` is a valid, uninitialized-but-sized
    // out-pointer the call fully writes before returning Ok.
    let session_ok = unsafe { GetNamedPipeClientSessionId(handle, &mut client_session) };
    // SAFETY: no preconditions.
    let active_session = unsafe { WTSGetActiveConsoleSessionId() };
    if session_ok.is_err() || client_session != active_session {
        log::line(&format!(
            "rejected a pipe client in session {client_session} (active console session is {active_session})"
        ));
        // SAFETY: `handle` is a valid, connected, still-owned handle; both calls release it,
        // and nothing below touches it again.
        unsafe {
            let _ = DisconnectNamedPipe(handle);
            let _ = CloseHandle(handle);
        }
        return Ok(());
    }

    // SAFETY: `handle` is a valid, connected, duplex pipe instance that nothing else holds a
    // reference to; `File` takes exclusive ownership and closes it when dropped.
    let mut pipe = unsafe { std::fs::File::from_raw_handle(handle.0) };

    let mut backend = direct_backend()?;
    loop {
        let call = match read_call(&mut pipe) {
            Ok(c) => c,
            Err(_) => return Ok(()), // disconnected, or sent garbage: wait for the next client
        };
        if let Err(e) = apply(&mut *backend, call) {
            log::line(&format!("injection failed: {e:#}"));
        }
    }
}

fn apply(backend: &mut dyn Backend, call: BackendCall) -> anyhow::Result<()> {
    match call {
        BackendCall::MoveTo { x, y, held } => backend.move_to(x, y, held),
        BackendCall::Button {
            button,
            down,
            x,
            y,
            clicks,
        } => backend.button(button, down, x, y, clicks),
        BackendCall::Scroll { dx, dy } => backend.scroll(dx, dy),
        BackendCall::Key { code, down } => backend.key(&code, down).map(|_| ()),
    }
}
