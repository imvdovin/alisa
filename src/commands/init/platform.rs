use std::{
    io,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::fd::AsRawFd;

#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, RawHandle};

pub(crate) fn wait_for_stdin(timeout: Duration) -> io::Result<bool> {
    wait_for_stdin_impl(timeout)
}

#[cfg(unix)]
fn wait_for_stdin_impl(timeout: Duration) -> io::Result<bool> {
    use libc::{POLLHUP, POLLIN, poll, pollfd};

    let fd = io::stdin().as_raw_fd();
    let mut descriptor = pollfd {
        fd,
        events: POLLIN | POLLHUP,
        revents: 0,
    };
    let deadline = Instant::now() + timeout;

    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(false);
        }

        let remaining = deadline - now;
        let timeout_ms = clamp_duration_to_millis_i32(remaining);
        descriptor.revents = 0;

        let result = unsafe { poll(&mut descriptor, 1, timeout_ms) };
        if result < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if result == 0 {
            continue;
        }

        if (descriptor.revents & (POLLIN | POLLHUP)) == 0 {
            continue;
        }

        if stdin_has_buffered_data(fd)? || (descriptor.revents & POLLHUP) != 0 {
            return Ok(true);
        }
    }
}

#[cfg(unix)]
fn stdin_has_buffered_data(fd: std::os::fd::RawFd) -> io::Result<bool> {
    let mut available: libc::c_int = 0;
    let rc = unsafe { libc::ioctl(fd, libc::FIONREAD, &mut available) };
    if rc == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(available > 0)
}

#[cfg(unix)]
fn clamp_duration_to_millis_i32(timeout: Duration) -> i32 {
    use std::cmp::min;

    let ms = timeout.as_millis();
    min(ms, i32::MAX as u128) as i32
}

#[cfg(windows)]
fn wait_for_stdin_impl(timeout: Duration) -> io::Result<bool> {
    use windows_sys::Win32::System::Threading::{
        WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT, WaitForSingleObject,
    };

    const ACTIVITY_BACKOFF: Duration = Duration::from_millis(10);

    let handle = io::stdin().as_raw_handle();
    if handle.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "stdin handle is unavailable",
        ));
    }

    let mut deadline = Instant::now() + timeout;

    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(false);
        }

        let remaining = deadline - now;
        let timeout_ms = clamp_duration_to_millis_u32(remaining);
        let status = unsafe { WaitForSingleObject(handle as isize, timeout_ms) };

        match status {
            WAIT_OBJECT_0 => match classify_stdin_ready_state(handle)? {
                StdinReadyState::DataAvailable | StdinReadyState::Disconnected => return Ok(true),
                StdinReadyState::Activity => {
                    deadline = Instant::now() + timeout;
                    std::thread::sleep(ACTIVITY_BACKOFF);
                }
            },
            WAIT_TIMEOUT => continue,
            WAIT_FAILED => return Err(io::Error::last_os_error()),
            _ => return Err(io::Error::last_os_error()),
        }
    }
}

#[cfg(windows)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum StdinReadyState {
    DataAvailable,
    Activity,
    Disconnected,
}

#[cfg(windows)]
fn classify_stdin_ready_state(handle: RawHandle) -> io::Result<StdinReadyState> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_TYPE_CHAR, FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType,
    };

    let file_type = unsafe { GetFileType(handle as isize) };
    match file_type {
        FILE_TYPE_CHAR => classify_console_ready_state(handle),
        FILE_TYPE_PIPE | FILE_TYPE_DISK => classify_pipe_like_ready_state(handle),
        _ => classify_pipe_like_ready_state(handle),
    }
}

#[cfg(windows)]
fn classify_pipe_like_ready_state(handle: RawHandle) -> io::Result<StdinReadyState> {
    use windows_sys::Win32::{Foundation::ERROR_BROKEN_PIPE, System::Pipes::PeekNamedPipe};

    let mut bytes_available: u32 = 0;
    let ok = unsafe {
        PeekNamedPipe(
            handle as isize,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut bytes_available,
            std::ptr::null_mut(),
        )
    };

    if ok == 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
            return Ok(StdinReadyState::Disconnected);
        }
        return Err(err);
    }

    if bytes_available > 0 {
        Ok(StdinReadyState::DataAvailable)
    } else {
        Ok(StdinReadyState::Activity)
    }
}

#[cfg(windows)]
fn classify_console_ready_state(handle: RawHandle) -> io::Result<StdinReadyState> {
    use std::{mem::MaybeUninit, slice};
    use windows_sys::Win32::{
        System::Console::{
            GetNumberOfConsoleInputEvents, INPUT_RECORD, KEY_EVENT, PeekConsoleInputW,
        },
        UI::Input::KeyboardAndMouse::VK_RETURN,
    };

    let mut events_available: u32 = 0;
    if unsafe { GetNumberOfConsoleInputEvents(handle as isize, &mut events_available) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if events_available == 0 {
        return Ok(StdinReadyState::Activity);
    }

    const BUFFER_SIZE: usize = 32;
    let mut records = MaybeUninit::<[INPUT_RECORD; BUFFER_SIZE]>::uninit();
    let mut events_read: u32 = 0;
    let ok = unsafe {
        PeekConsoleInputW(
            handle as isize,
            records.as_mut_ptr() as *mut _,
            BUFFER_SIZE as u32,
            &mut events_read,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut saw_key_event = false;
    let record_slice: &[INPUT_RECORD] = unsafe {
        slice::from_raw_parts(records.as_ptr() as *const INPUT_RECORD, BUFFER_SIZE)
            .get(..events_read as usize)
            .expect("events_read must not exceed buffer size")
    };

    for record in record_slice {
        if record.EventType != KEY_EVENT {
            continue;
        }

        let key_event = unsafe { record.Event.KeyEvent };
        if key_event.bKeyDown == 0 {
            continue;
        }
        saw_key_event = true;

        let unicode = unsafe { key_event.uChar.UnicodeChar };
        if unicode == b'\r' as u16 || unicode == b'\n' as u16 {
            return Ok(StdinReadyState::DataAvailable);
        }
        if key_event.wVirtualKeyCode == VK_RETURN as u16 {
            return Ok(StdinReadyState::DataAvailable);
        }
    }

    if saw_key_event {
        Ok(StdinReadyState::Activity)
    } else {
        Ok(StdinReadyState::Activity)
    }
}

#[cfg(windows)]
fn clamp_duration_to_millis_u32(timeout: Duration) -> u32 {
    use std::cmp::min;

    min(timeout.as_millis(), u32::MAX as u128) as u32
}

#[cfg(not(any(unix, windows)))]
fn wait_for_stdin_impl(_timeout: Duration) -> io::Result<bool> {
    Ok(true)
}
