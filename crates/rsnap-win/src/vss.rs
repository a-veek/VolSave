//! Volume Shadow Copy Service: consistent point-in-time views of live volumes.
//!
//! `IVssBackupComponents` is not part of the Windows metadata that
//! windows-rs is generated from, so its vtable is declared here in the order
//! of `vsbackup.h`. Only the methods rsnap calls have real signatures; the
//! others are placeholders that keep the slots in place.

use std::ffi::c_void;
use std::ptr::null_mut;

use windows::Win32::Storage::Vss::{
    IVssAsync, IVssEnumObject, VSS_BT_COPY, VSS_CTX_BACKUP, VSS_E_BAD_STATE,
    VSS_E_FLUSH_WRITES_TIMEOUT, VSS_E_HOLD_WRITES_TIMEOUT, VSS_E_INSUFFICIENT_STORAGE,
    VSS_E_MAXIMUM_NUMBER_OF_SNAPSHOTS_REACHED, VSS_E_MAXIMUM_NUMBER_OF_VOLUMES_REACHED,
    VSS_E_OBJECT_NOT_FOUND, VSS_E_PROVIDER_VETO, VSS_E_SNAPSHOT_SET_IN_PROGRESS,
    VSS_E_UNEXPECTED_PROVIDER_ERROR, VSS_E_VOLUME_NOT_SUPPORTED,
    VSS_E_VOLUME_NOT_SUPPORTED_BY_PROVIDER, VSS_OBJECT_NONE, VSS_OBJECT_PROP, VSS_OBJECT_SNAPSHOT,
    VSS_S_ASYNC_FINISHED, VSS_SNAPSHOT_PROP, VSS_WS_FAILED_AT_IDENTIFY,
};
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoInitializeSecurity, CoTaskMemFree,
    EOAC_DYNAMIC_CLOAKING, RPC_C_AUTHN_LEVEL_PKT_PRIVACY, RPC_C_IMP_LEVEL_IDENTIFY,
};
use windows::core::{BSTR, GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface};

use crate::util::{format_guid, from_wide, wide};

windows_link::link!("vssapi.dll" "system" fn CreateVssBackupComponentsInternal(components: *mut *mut c_void) -> HRESULT);

const E_ACCESSDENIED: HRESULT = HRESULT(0x8007_0005_u32 as i32);

type This = *mut c_void;
type AsyncCall = unsafe extern "system" fn(This, *mut *mut c_void) -> HRESULT;
type Unused = usize;

#[repr(C)]
#[allow(dead_code)]
struct BackupComponentsVtbl {
    base: IUnknown_Vtbl,
    get_writer_components_count: Unused,
    get_writer_components: Unused,
    initialize_for_backup: unsafe extern "system" fn(This, *mut u16) -> HRESULT,
    set_backup_state: unsafe extern "system" fn(This, bool, bool, i32, bool) -> HRESULT,
    initialize_for_restore: Unused,
    set_restore_state: Unused,
    gather_writer_metadata: AsyncCall,
    get_writer_metadata_count: Unused,
    get_writer_metadata: Unused,
    free_writer_metadata: unsafe extern "system" fn(This) -> HRESULT,
    add_component: Unused,
    prepare_for_backup: AsyncCall,
    abort_backup: unsafe extern "system" fn(This) -> HRESULT,
    gather_writer_status: AsyncCall,
    get_writer_status_count: unsafe extern "system" fn(This, *mut u32) -> HRESULT,
    free_writer_status: unsafe extern "system" fn(This) -> HRESULT,
    get_writer_status: unsafe extern "system" fn(
        This,
        u32,
        *mut GUID,
        *mut GUID,
        *mut *mut u16,
        *mut i32,
        *mut HRESULT,
    ) -> HRESULT,
    set_backup_succeeded: Unused,
    set_backup_options: Unused,
    set_selected_for_restore: Unused,
    set_restore_options: Unused,
    set_additional_restores: Unused,
    set_previous_backup_stamp: Unused,
    save_as_xml: Unused,
    backup_complete: AsyncCall,
    add_alternative_location_mapping: Unused,
    add_restore_subcomponent: Unused,
    set_file_restore_status: Unused,
    add_new_target: Unused,
    set_ranges_file_path: Unused,
    pre_restore: Unused,
    post_restore: Unused,
    set_context: unsafe extern "system" fn(This, i32) -> HRESULT,
    start_snapshot_set: unsafe extern "system" fn(This, *mut GUID) -> HRESULT,
    add_to_snapshot_set: unsafe extern "system" fn(This, *const u16, GUID, *mut GUID) -> HRESULT,
    do_snapshot_set: AsyncCall,
    delete_snapshots: Unused,
    import_snapshots: Unused,
    break_snapshot_set: Unused,
    get_snapshot_properties:
        unsafe extern "system" fn(This, GUID, *mut VSS_SNAPSHOT_PROP) -> HRESULT,
    query: unsafe extern "system" fn(This, GUID, i32, i32, *mut *mut c_void) -> HRESULT,
    is_volume_supported: unsafe extern "system" fn(This, GUID, *const u16, *mut i32) -> HRESULT,
}

#[derive(Debug, thiserror::Error)]
pub enum VssError {
    #[error("VSS {step} failed: {message} (0x{code:08X})")]
    Call {
        step: &'static str,
        code: u32,
        message: &'static str,
    },
    #[error("volume {0} cannot be snapshotted by VSS (only NTFS and ReFS are supported)")]
    NotSupported(String),
    #[error(
        "VSS created snapshot {snapshot_id} of {volume} but reported no device for it (state {state}); run with RSNAP_DEBUG=1 for details"
    )]
    NoDevice {
        snapshot_id: String,
        volume: String,
        state: i32,
    },
}

/// One volume of a snapshot set.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// The live volume, `\\?\Volume{GUID}\`.
    pub original_volume: String,
    /// Device to read the frozen view from,
    /// `\\?\GLOBALROOT\Device\HarddiskVolumeShadowCopyN`.
    pub device_object: String,
}

impl Snapshot {
    /// Root directory inside the snapshot, for opening files.
    pub fn root(&self) -> String {
        format!("{}\\", self.device_object)
    }
}

/// Snapshots of one or more volumes taken at the same instant. They exist
/// until the set is dropped.
pub struct SnapshotSet {
    components: IUnknown,
    snapshots: Vec<Snapshot>,
    warnings: Vec<String>,
    completed: bool,
}

impl SnapshotSet {
    /// Creates a snapshot of `volumes` (each `\\?\Volume{GUID}\` or `C:\`).
    /// VSS writers (databases, Hyper-V, …) are asked to flush their data first.
    pub fn create(volumes: &[&str]) -> Result<Self, VssError> {
        init_com();
        let mut raw = null_mut();
        // SAFETY: `raw` receives an owned interface pointer.
        check("CreateVssBackupComponents", unsafe {
            CreateVssBackupComponentsInternal(&mut raw)
        })?;
        // SAFETY: the pointer was just returned with one reference we now own.
        let components = unsafe { IUnknown::from_raw(raw) };
        let mut set = Self {
            components,
            snapshots: Vec::new(),
            warnings: Vec::new(),
            completed: false,
        };
        set.take_snapshots(volumes)?;
        Ok(set)
    }

    pub fn snapshots(&self) -> &[Snapshot] {
        &self.snapshots
    }

    /// The snapshot of `volume` (`\\?\Volume{GUID}\`), if it is in the set.
    pub fn snapshot_of(&self, volume: &str) -> Option<&Snapshot> {
        self.snapshots
            .iter()
            .find(|snapshot| snapshot.original_volume.eq_ignore_ascii_case(volume))
    }

    /// Writers that reported problems while the snapshot was taken.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Tells the writers the backup succeeded, then releases the snapshots.
    pub fn complete(mut self) -> Result<(), VssError> {
        self.completed = true;
        let vtbl = self.vtbl();
        self.wait("BackupComplete", vtbl.backup_complete)
    }

    fn vtbl(&self) -> &'static BackupComponentsVtbl {
        // SAFETY: the object implements IVssBackupComponents, whose vtable
        // starts with the layout above and lives as long as the module.
        unsafe { &**(self.components.as_raw() as *const *const BackupComponentsVtbl) }
    }

    fn this(&self) -> This {
        self.components.as_raw()
    }

    fn take_snapshots(&mut self, volumes: &[&str]) -> Result<(), VssError> {
        let vtbl = self.vtbl();
        let this = self.this();
        // SAFETY (all calls below): `this` is a live IVssBackupComponents and
        // every pointer argument references a local that outlives the call.
        unsafe {
            check(
                "InitializeForBackup",
                (vtbl.initialize_for_backup)(this, null_mut()),
            )?;
            check("SetContext", (vtbl.set_context)(this, VSS_CTX_BACKUP.0))?;
            // Copy backup: does not truncate application logs (e.g. Exchange, SQL).
            check(
                "SetBackupState",
                (vtbl.set_backup_state)(this, false, false, VSS_BT_COPY.0, false),
            )?;
        }
        self.wait("GatherWriterMetadata", vtbl.gather_writer_metadata)?;
        // SAFETY: as above.
        unsafe { check("FreeWriterMetadata", (vtbl.free_writer_metadata)(this))? };

        let names: Vec<Vec<u16>> = volumes.iter().map(|volume| wide(volume)).collect();
        for (volume, name) in volumes.iter().zip(&names) {
            let mut supported = 0;
            // SAFETY: as above.
            let hr = unsafe {
                (vtbl.is_volume_supported)(this, GUID::zeroed(), name.as_ptr(), &mut supported)
            };
            if hr.is_err() || supported == 0 {
                return Err(VssError::NotSupported(volume.to_string()));
            }
        }

        let mut set_id = GUID::zeroed();
        let mut ids = Vec::new();
        // SAFETY: as above.
        unsafe {
            check(
                "StartSnapshotSet",
                (vtbl.start_snapshot_set)(this, &mut set_id),
            )?;
            for name in &names {
                let mut id = GUID::zeroed();
                check(
                    "AddToSnapshotSet",
                    (vtbl.add_to_snapshot_set)(this, name.as_ptr(), GUID::zeroed(), &mut id),
                )?;
                ids.push(id);
            }
        }
        self.wait("PrepareForBackup", vtbl.prepare_for_backup)?;
        self.wait("DoSnapshotSet", vtbl.do_snapshot_set)?;
        self.warnings = self.writer_problems();

        debug(&format!("snapshot set {}", format_guid(&set_id)));

        for (id, volume) in ids.into_iter().zip(volumes) {
            let mut props = VSS_SNAPSHOT_PROP::default();
            // SAFETY: as above; the strings in `props` are freed right after copying.
            let mut snapshot = unsafe {
                check(
                    "GetSnapshotProperties",
                    (vtbl.get_snapshot_properties)(this, id, &mut props),
                )?;
                let snapshot = snapshot_from(&props);
                free_snapshot_properties(&props);
                snapshot
            };
            debug(&format!(
                "snapshot {}: device {:?}, original volume {:?}, state {}",
                format_guid(&id),
                snapshot.device_object,
                snapshot.original_volume,
                props.m_eStatus.0
            ));
            if snapshot.device_object.is_empty() {
                // Second route to the same information.
                match self.query_snapshot(id) {
                    Some(found) => snapshot = found,
                    None => {
                        return Err(VssError::NoDevice {
                            snapshot_id: format_guid(&id),
                            volume: volume.to_string(),
                            state: props.m_eStatus.0,
                        });
                    }
                }
            }
            if snapshot.original_volume.is_empty() {
                snapshot.original_volume = volume.to_string();
            }
            self.snapshots.push(snapshot);
        }
        Ok(())
    }

    /// Looks a snapshot up through `IVssBackupComponents::Query`.
    fn query_snapshot(&self, id: GUID) -> Option<Snapshot> {
        let mut raw = null_mut();
        // SAFETY: `raw` receives an owned IVssEnumObject pointer.
        let hr = unsafe {
            (self.vtbl().query)(
                self.this(),
                GUID::zeroed(),
                VSS_OBJECT_NONE.0,
                VSS_OBJECT_SNAPSHOT.0,
                &mut raw,
            )
        };
        debug(&format!("Query: 0x{:08X}", hr.0 as u32));
        if hr.is_err() || raw.is_null() {
            return None;
        }
        // SAFETY: the pointer was just returned with one reference we now own.
        let objects = unsafe { IVssEnumObject::from_raw(raw) };
        loop {
            let mut item = [VSS_OBJECT_PROP::default()];
            let mut fetched = 0u32;
            // SAFETY: `item` and `fetched` are locals.
            let next = unsafe { objects.Next(&mut item, &mut fetched) };
            if next.is_err() || fetched == 0 {
                return None;
            }
            if item[0].Type != VSS_OBJECT_SNAPSHOT {
                continue;
            }
            // SAFETY: the type says the union holds a snapshot; its strings
            // are freed right after copying them.
            let props = unsafe { item[0].Obj.Snap };
            let snapshot = unsafe { snapshot_from(&props) };
            unsafe { free_snapshot_properties(&props) };
            debug(&format!(
                "Query found {}: device {:?}",
                format_guid(&props.m_SnapshotId),
                snapshot.device_object
            ));
            if props.m_SnapshotId == id && !snapshot.device_object.is_empty() {
                return Some(snapshot);
            }
        }
    }

    /// Runs an asynchronous VSS call and waits for it to finish.
    fn wait(&self, step: &'static str, call: AsyncCall) -> Result<(), VssError> {
        let mut raw = null_mut();
        // SAFETY: `raw` receives an owned IVssAsync pointer.
        check(step, unsafe { call(self.this(), &mut raw) })?;
        // SAFETY: the pointer was just returned with one reference we now own.
        let pending = unsafe { IVssAsync::from_raw(raw) };
        let mut status = HRESULT(0);
        // SAFETY: `pending` is live; `status` is a local.
        unsafe {
            pending
                .Wait(u32::MAX)
                .map_err(|error| call_error(step, error.code()))?;
            pending
                .QueryStatus(&mut status, null_mut())
                .map_err(|error| call_error(step, error.code()))?;
        }
        if status == VSS_S_ASYNC_FINISHED {
            Ok(())
        } else {
            Err(call_error(step, status))
        }
    }

    /// Names and states of writers that failed. Errors here only cost the
    /// warnings, never the backup.
    fn writer_problems(&self) -> Vec<String> {
        let vtbl = self.vtbl();
        let this = self.this();
        if self
            .wait("GatherWriterStatus", vtbl.gather_writer_status)
            .is_err()
        {
            return Vec::new();
        }
        let mut problems = Vec::new();
        let mut count = 0u32;
        // SAFETY: as in `take_snapshots`; `name` is freed by `BSTR`'s drop.
        unsafe {
            if (vtbl.get_writer_status_count)(this, &mut count).is_ok() {
                for i in 0..count {
                    let (mut instance, mut writer) = (GUID::zeroed(), GUID::zeroed());
                    let mut name = null_mut();
                    let (mut state, mut failure) = (0i32, HRESULT(0));
                    let hr = (vtbl.get_writer_status)(
                        this,
                        i,
                        &mut instance,
                        &mut writer,
                        &mut name,
                        &mut state,
                        &mut failure,
                    );
                    let name = BSTR::from_raw(name);
                    if hr.is_ok() && state >= VSS_WS_FAILED_AT_IDENTIFY.0 {
                        problems.push(format!(
                            "VSS writer \"{name}\" failed (state {state}, error 0x{:08X})",
                            failure.0 as u32
                        ));
                    }
                }
            }
            let _ = (vtbl.free_writer_status)(this);
        }
        problems
    }
}

impl Drop for SnapshotSet {
    fn drop(&mut self) {
        if !self.completed {
            // SAFETY: `this` is live; aborting tells writers to resume normally.
            let _ = unsafe { (self.vtbl().abort_backup)(self.this()) };
        }
        // Releasing the last reference deletes the non-persistent snapshots.
    }
}

/// COM must be initialised once per thread; security once per process.
fn init_com() {
    // SAFETY: plain initialisation calls; "already initialised" results are fine.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = CoInitializeSecurity(
            None,
            -1,
            None,
            None,
            RPC_C_AUTHN_LEVEL_PKT_PRIVACY,
            RPC_C_IMP_LEVEL_IDENTIFY,
            None,
            EOAC_DYNAMIC_CLOAKING,
            None,
        );
    }
}

/// Prints VSS details when the environment variable `RSNAP_DEBUG` is set.
fn debug(message: &str) {
    if std::env::var_os("RSNAP_DEBUG").is_some() {
        eprintln!("[vss] {message}");
    }
}

/// # Safety
/// `props` must have been filled by VSS.
unsafe fn snapshot_from(props: &VSS_SNAPSHOT_PROP) -> Snapshot {
    // SAFETY: VSS fills these with null or NUL-terminated strings.
    unsafe {
        Snapshot {
            original_volume: wide_ptr_to_string(props.m_pwszOriginalVolumeName),
            device_object: wide_ptr_to_string(props.m_pwszSnapshotDeviceObject),
        }
    }
}

fn check(step: &'static str, hr: HRESULT) -> Result<(), VssError> {
    debug(&format!("{step}: 0x{:08X}", hr.0 as u32));
    if hr.is_ok() {
        Ok(())
    } else {
        Err(call_error(step, hr))
    }
}

fn call_error(step: &'static str, hr: HRESULT) -> VssError {
    VssError::Call {
        step,
        code: hr.0 as u32,
        message: describe(hr),
    }
}

fn describe(hr: HRESULT) -> &'static str {
    match hr {
        E_ACCESSDENIED => "access denied, run as administrator",
        VSS_E_BAD_STATE => "VSS is in an unexpected state",
        VSS_E_INSUFFICIENT_STORAGE => "not enough free space for the shadow copy",
        VSS_E_FLUSH_WRITES_TIMEOUT | VSS_E_HOLD_WRITES_TIMEOUT => {
            "the system was too busy to freeze writes"
        }
        VSS_E_MAXIMUM_NUMBER_OF_SNAPSHOTS_REACHED => "too many shadow copies exist",
        VSS_E_MAXIMUM_NUMBER_OF_VOLUMES_REACHED => "too many volumes in one snapshot",
        VSS_E_OBJECT_NOT_FOUND => "volume not found",
        VSS_E_PROVIDER_VETO => "the shadow copy provider refused",
        VSS_E_SNAPSHOT_SET_IN_PROGRESS => "another shadow copy is being created",
        VSS_E_UNEXPECTED_PROVIDER_ERROR => "the shadow copy provider failed",
        VSS_E_VOLUME_NOT_SUPPORTED | VSS_E_VOLUME_NOT_SUPPORTED_BY_PROVIDER => {
            "volume not supported"
        }
        _ => "unexpected error",
    }
}

/// # Safety
/// `text` must be null or a NUL-terminated UTF-16 string.
unsafe fn wide_ptr_to_string(text: *const u16) -> String {
    if text.is_null() {
        return String::new();
    }
    let mut len = 0;
    // SAFETY: the caller guarantees NUL termination.
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` characters before the NUL are readable.
    from_wide(unsafe { std::slice::from_raw_parts(text, len) })
}

/// What `VssFreeSnapshotProperties` does: free the strings VSS allocated.
///
/// # Safety
/// `props` must have been filled by `GetSnapshotProperties`.
unsafe fn free_snapshot_properties(props: &VSS_SNAPSHOT_PROP) {
    for text in [
        props.m_pwszSnapshotDeviceObject,
        props.m_pwszOriginalVolumeName,
        props.m_pwszOriginatingMachine,
        props.m_pwszServiceMachine,
        props.m_pwszExposedName,
        props.m_pwszExposedPath,
    ] {
        if !text.is_null() {
            // SAFETY: VSS allocated these strings with CoTaskMemAlloc.
            unsafe { CoTaskMemFree(Some(text as *const c_void)) };
        }
    }
}
