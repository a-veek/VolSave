use windows::Win32::Foundation::{CloseHandle, ERROR_NOT_ALL_ASSIGNED, GetLastError, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_BACKUP_NAME,
    SE_PRIVILEGE_ENABLED, SE_RESTORE_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::PCWSTR;

/// Enables the backup and restore privileges administrators hold but have
/// switched off. With them, files and folders whose permissions exclude
/// administrators (such as `System Volume Information`) can be read.
///
/// Returns `false` if they could not be enabled, e.g. without elevation.
pub fn enable_backup_privileges() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: `token` receives a handle that is closed below.
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    }
    .is_err()
    {
        return false;
    }
    let enabled = [SE_BACKUP_NAME, SE_RESTORE_NAME]
        .into_iter()
        .all(|name| enable(token, name));
    // SAFETY: closing the handle opened above.
    let _ = unsafe { CloseHandle(token) };
    enabled
}

fn enable(token: HANDLE, name: PCWSTR) -> bool {
    let mut luid = LUID::default();
    // SAFETY: `name` is a static string; `luid` is a local.
    if unsafe { LookupPrivilegeValueW(PCWSTR::null(), name, &mut luid) }.is_err() {
        return false;
    }
    let privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    // SAFETY: `privileges` outlives the call. The call succeeds even when the
    // privilege is missing, so the last error tells whether it was assigned.
    unsafe {
        AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None).is_ok()
            && GetLastError() != ERROR_NOT_ALL_ASSIGNED
    }
}
