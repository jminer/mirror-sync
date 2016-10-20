
use std::fs::File;
use std::io;
use std::mem;
use std::path::Path;
use std::ptr;
use std::time::{Duration, SystemTime};
use std::os::windows::io::AsRawHandle;

use kernel32::*;
use winapi::*;

fn duration_to_intervals(duration: Duration) -> u64 {
    duration.as_secs() * 10_000_000 + (duration.subsec_nanos() / 100) as u64
}

/// Sets the last modified time of the specified file.
pub fn set_modified<P: AsRef<Path>>(file: P, time: SystemTime) -> Result<(), io::Error> {
    let file = File::open(file)?;
    unsafe {
        let mut system_time_now: SYSTEMTIME = mem::uninitialized();
        GetSystemTime(&mut system_time_now as *mut SYSTEMTIME);
        let mut file_time_now: FILETIME = mem::uninitialized();
        if SystemTimeToFileTime(&system_time_now as *const SYSTEMTIME,
                                &mut file_time_now as *mut FILETIME) == 0 {
            return Err(io::Error::last_os_error());
        }

        let file_time_count = file_time_now.dwLowDateTime as u64 |
                              ((file_time_now.dwHighDateTime as u64) << 32);
        let file_time_count = match time.elapsed() {
            Ok(dur) => file_time_count - duration_to_intervals(dur),
            Err(err) => file_time_count + duration_to_intervals(err.duration()),
        };
        let file_time = FILETIME {
            dwLowDateTime: file_time_count as u32,
            dwHighDateTime: (file_time_count >> 32) as u32,
        };

        if SetFileTime(file.as_raw_handle(), ptr::null(), &file_time as *const FILETIME, ptr::null()) == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
