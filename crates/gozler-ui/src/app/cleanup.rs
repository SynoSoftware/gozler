//! Setup's preference reset runs as the desktop user, including when another
//! administrator approved elevation; saved file operations are left intact.

use crate::core::startup::{Cleanup, USER_CLEANUP_ARGUMENT};
use crate::win;
use std::ffi::OsStr;
use std::io;

pub(super) fn run(cleanup: Cleanup) -> ! {
    let result = win::work::spawn(move |worker| {
        let _shell = win::shell::Session::new(worker);
        let result = match cleanup {
            Cleanup::Desktop => std::env::current_exe().and_then(|program| {
                win::shell::run_as_user(&program, OsStr::new(USER_CLEANUP_ARGUMENT), worker)
            }),
            Cleanup::User => remove(worker).map(|()| 0),
        };
        match result {
            Ok(code) => code,
            Err(error) => {
                if let Ok(folder) = win::shell::local_app_data() {
                    super::write_log(
                        &folder.join("Gozler").join("ui-error.log"),
                        &format!("{cleanup:?} settings cleanup failed: {error}"),
                    );
                }
                error.raw_os_error().unwrap_or(1) as u32
            }
        }
    })
    .join();
    std::process::exit(result.unwrap_or(1) as i32);
}

fn remove(worker: &win::work::OffThread) -> io::Result<()> {
    if win::instance::is_elevated() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Cleanup requires the desktop user",
        ));
    }
    let data_dir = win::shell::local_app_data()
        .map_err(io::Error::other)?
        .join("Gozler");
    win::shell::remove_settings(worker)?;
    for name in ["ui.json", "ui.json.tmp"] {
        match std::fs::remove_file(data_dir.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
