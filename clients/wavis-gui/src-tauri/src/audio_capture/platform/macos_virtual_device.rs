//! macOS virtual audio device discovery for the audio-share isolation fallback.
//!
//! This module only handles task 4 of the isolation spec: enumerate CoreAudio
//! devices, identify supported loopback devices by name, and provide the
//! CoreAudio helpers needed by the follow-up routing tasks.

use super::macos_routing::{
    plan_stale_multi_output_cleanup, select_virtual_device_candidate, StaleCleanupDeviceInfo,
    VirtualDeviceCandidate,
};
use core_foundation::{
    array::CFArray,
    base::{CFType, TCFType},
    boolean::CFBoolean,
    dictionary::CFDictionary,
    string::{CFString, CFStringRef},
};
use std::{
    ffi::c_void,
    mem, ptr,
    time::{SystemTime, UNIX_EPOCH},
};

macro_rules! share_audio_debug {
    ($($arg:tt)*) => {
        if crate::debug_env::debug_share_audio_enabled() {
            crate::debug_eprintln!($($arg)*);
        }
    };
}

macro_rules! share_audio_info {
    ($($arg:tt)*) => {
        if crate::debug_env::debug_share_audio_enabled() {
            log::info!($($arg)*);
        }
    };
}

pub(super) type AudioObjectID = u32;

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct VirtualDeviceInfo {
    pub(super) device_id: AudioObjectID,
    pub(super) uid: String,
    pub(super) name: String,
}

#[repr(C)]
struct AudioObjectPropertyAddress {
    m_selector: u32,
    m_scope: u32,
    m_element: u32,
}

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyDataSize(
        in_object_id: AudioObjectID,
        in_address: *const AudioObjectPropertyAddress,
        in_qualifier_data_size: u32,
        in_qualifier_data: *const c_void,
        out_data_size: *mut u32,
    ) -> i32;

    fn AudioObjectGetPropertyData(
        in_object_id: AudioObjectID,
        in_address: *const AudioObjectPropertyAddress,
        in_qualifier_data_size: u32,
        in_qualifier_data: *const c_void,
        io_data_size: *mut u32,
        out_data: *mut c_void,
    ) -> i32;

    fn AudioObjectSetPropertyData(
        in_object_id: AudioObjectID,
        in_address: *const AudioObjectPropertyAddress,
        in_qualifier_data_size: u32,
        in_qualifier_data: *const c_void,
        in_data_size: u32,
        in_data: *const c_void,
    ) -> i32;

    fn AudioHardwareCreateAggregateDevice(
        in_description: *const c_void,
        out_device_id: *mut AudioObjectID,
    ) -> i32;

    fn AudioHardwareDestroyAggregateDevice(in_device_id: AudioObjectID) -> i32;
}

const WAVIS_AUDIO_BRIDGE_NAME: &str = "Wavis Audio Bridge";
const WAVIS_AUDIO_BRIDGE_UID_PREFIX: &str = "com.wavis.audio-bridge-";
const K_AUDIO_AGGREGATE_DEVICE_NAME_KEY: &str = "name";
const K_AUDIO_AGGREGATE_DEVICE_UID_KEY: &str = "uid";
const K_AUDIO_AGGREGATE_DEVICE_SUB_DEVICE_LIST_KEY: &str = "subdevices";
const K_AUDIO_AGGREGATE_DEVICE_MAIN_SUB_DEVICE_KEY: &str = "master";
const K_AUDIO_AGGREGATE_DEVICE_IS_STACKED_KEY: &str = "stacked";
const K_AUDIO_SUB_DEVICE_UID_KEY: &str = "uid";
const K_AUDIO_OBJECT_SYSTEM_OBJECT: AudioObjectID = 1;
const K_AUDIO_HARDWARE_NO_ERROR: i32 = 0;
const K_AUDIO_HARDWARE_PROPERTY_DEVICES: u32 = 0x64657623;
const K_AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE: u32 = 0x644f_7574;
const K_AUDIO_DEVICE_PROPERTY_DEVICE_UID: u32 = 0x7569_6420;
const K_AUDIO_OBJECT_PROPERTY_NAME: u32 = 0x6c6e_616d;
const K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL: u32 = 0x676c_6f62;
const K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN: u32 = 0;
// kAudioAggregateDevicePropertyFullSubDeviceList = 'gSub'
// Returns a CFArrayRef containing CFString UIDs of all sub-devices.
const K_AUDIO_AGGREGATE_DEVICE_PROPERTY_FULL_SUB_DEVICE_LIST: u32 = 0x6753_7562;

fn property_address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        m_selector: selector,
        m_scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
        m_element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    }
}

fn enumerate_audio_devices() -> Result<Vec<AudioObjectID>, String> {
    let address = property_address(K_AUDIO_HARDWARE_PROPERTY_DEVICES);
    let mut data_size = 0u32;
    let size_status = unsafe {
        AudioObjectGetPropertyDataSize(
            K_AUDIO_OBJECT_SYSTEM_OBJECT,
            &address,
            0,
            ptr::null(),
            &mut data_size,
        )
    };
    if size_status != K_AUDIO_HARDWARE_NO_ERROR {
        return Err(format!(
            "AudioObjectGetPropertyDataSize(devices) failed: OSStatus {size_status}"
        ));
    }

    if data_size == 0 {
        return Ok(Vec::new());
    }

    let object_size = mem::size_of::<AudioObjectID>() as u32;
    if data_size % object_size != 0 {
        return Err(format!(
            "CoreAudio device list size {data_size} is not aligned to AudioObjectID"
        ));
    }

    let mut devices = vec![0u32; (data_size / object_size) as usize];
    let read_status = unsafe {
        AudioObjectGetPropertyData(
            K_AUDIO_OBJECT_SYSTEM_OBJECT,
            &address,
            0,
            ptr::null(),
            &mut data_size,
            devices.as_mut_ptr() as *mut c_void,
        )
    };
    if read_status != K_AUDIO_HARDWARE_NO_ERROR {
        return Err(format!(
            "AudioObjectGetPropertyData(devices) failed: OSStatus {read_status}"
        ));
    }

    Ok(devices)
}

fn get_string_property(object_id: AudioObjectID, selector: u32) -> Result<Option<String>, String> {
    let address = property_address(selector);
    let mut value: CFStringRef = ptr::null();
    let mut data_size = mem::size_of::<CFStringRef>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object_id,
            &address,
            0,
            ptr::null(),
            &mut data_size,
            &mut value as *mut _ as *mut c_void,
        )
    };
    if status != K_AUDIO_HARDWARE_NO_ERROR {
        return Err(format!(
            "AudioObjectGetPropertyData(selector=0x{selector:08X}) failed for device {object_id}: \
             OSStatus {status}"
        ));
    }

    if value.is_null() {
        return Ok(None);
    }

    let value = unsafe { CFString::wrap_under_get_rule(value) }.to_string();
    Ok(Some(value))
}

fn get_audio_object_id_property(
    object_id: AudioObjectID,
    selector: u32,
) -> Result<AudioObjectID, String> {
    let address = property_address(selector);
    let mut value = 0u32;
    let mut data_size = mem::size_of::<AudioObjectID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object_id,
            &address,
            0,
            ptr::null(),
            &mut data_size,
            &mut value as *mut _ as *mut c_void,
        )
    };
    if status != K_AUDIO_HARDWARE_NO_ERROR {
        return Err(format!(
            "AudioObjectGetPropertyData(selector=0x{selector:08X}) failed for object {object_id}: \
             OSStatus {status}"
        ));
    }
    if data_size != mem::size_of::<AudioObjectID>() as u32 {
        return Err(format!(
            "AudioObjectGetPropertyData(selector=0x{selector:08X}) returned unexpected size \
             {data_size} for object {object_id}"
        ));
    }
    if value == 0 {
        return Err(format!(
            "AudioObjectGetPropertyData(selector=0x{selector:08X}) returned null object id for \
             object {object_id}"
        ));
    }

    Ok(value)
}

fn set_audio_object_id_property(
    object_id: AudioObjectID,
    selector: u32,
    value: AudioObjectID,
) -> Result<(), String> {
    let address = property_address(selector);
    let status = unsafe {
        AudioObjectSetPropertyData(
            object_id,
            &address,
            0,
            ptr::null(),
            mem::size_of::<AudioObjectID>() as u32,
            &value as *const _ as *const c_void,
        )
    };
    if status != K_AUDIO_HARDWARE_NO_ERROR {
        return Err(format!(
            "AudioObjectSetPropertyData(selector=0x{selector:08X}) failed for object {object_id}: \
             OSStatus {status}"
        ));
    }

    Ok(())
}

fn get_default_output_device_id() -> Result<AudioObjectID, String> {
    get_audio_object_id_property(
        K_AUDIO_OBJECT_SYSTEM_OBJECT,
        K_AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE,
    )
}

/// Returns the CoreAudio device IDs of all sub-devices of an aggregate device.
/// Returns an empty vec if the device is not an aggregate or the query fails.
///
/// `kAudioAggregateDevicePropertyFullSubDeviceList` returns a CFArrayRef of
/// CFString UIDs — NOT an array of AudioObjectIDs. We read the CFArrayRef,
/// extract each UID string, then resolve UIDs to device IDs by scanning all
/// devices.
fn get_aggregate_sub_device_ids(device_id: AudioObjectID) -> Vec<AudioObjectID> {
    use core_foundation::array::CFArrayRef;

    let address = property_address(K_AUDIO_AGGREGATE_DEVICE_PROPERTY_FULL_SUB_DEVICE_LIST);
    let mut array_ref: CFArrayRef = ptr::null();
    let mut data_size = mem::size_of::<CFArrayRef>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            &address,
            0,
            ptr::null(),
            &mut data_size,
            &mut array_ref as *mut _ as *mut c_void,
        )
    };
    if status != K_AUDIO_HARDWARE_NO_ERROR || array_ref.is_null() {
        return Vec::new();
    }

    // SAFETY: CoreAudio gives us a +1 retained CFArray; we own it and must release.
    let uid_array = unsafe { CFArray::<CFString>::wrap_under_create_rule(array_ref) };

    // Collect the UID strings from the array.
    let uids: Vec<String> = (0..uid_array.len())
        .filter_map(|i| {
            let item = uid_array.get(i)?;
            // Each element is a CFStringRef.
            let cf_str =
                unsafe { CFString::wrap_under_get_rule(item.as_CFTypeRef() as CFStringRef) };
            Some(cf_str.to_string())
        })
        .collect();

    if uids.is_empty() {
        return Vec::new();
    }

    // Resolve UIDs → device IDs by scanning all CoreAudio devices.
    let all_devices = match enumerate_audio_devices() {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let mut result = Vec::with_capacity(uids.len());
    for uid in &uids {
        for &dev_id in &all_devices {
            if let Ok(Some(dev_uid)) = get_device_uid(dev_id) {
                if &dev_uid == uid {
                    result.push(dev_id);
                    break;
                }
            }
        }
    }
    result
}

/// If `device_id` is a stacked aggregate that already contains `loopback_uid`
/// as a direct sub-device, returns the UID of the first non-loopback
/// sub-device (i.e., the hardware speaker).  Returns `None` if the device is
/// not an aggregate, does not contain `loopback_uid`, or no hardware sub-device
/// can be identified.
pub(super) fn find_hardware_speaker_uid_in_aggregate(
    device_id: AudioObjectID,
    loopback_uid: &str,
) -> Option<String> {
    let sub_ids = get_aggregate_sub_device_ids(device_id);
    if sub_ids.is_empty() {
        return None;
    }

    let mut has_loopback = false;
    let mut hardware_uid: Option<String> = None;

    for sub_id in sub_ids {
        let uid = match get_device_uid(sub_id).ok().flatten() {
            Some(u) => u,
            None => continue,
        };
        let name = get_device_name(sub_id).ok().flatten().unwrap_or_default();

        if uid == loopback_uid {
            has_loopback = true;
        } else if !is_wavis_audio_bridge_name(&name)
            && !uid.starts_with(WAVIS_AUDIO_BRIDGE_UID_PREFIX)
            && super::macos_routing::virtual_device_rank(&name).is_none()
            && hardware_uid.is_none()
        {
            hardware_uid = Some(uid);
        }
    }

    if has_loopback {
        hardware_uid
    } else {
        None
    }
}
fn get_device_name(object_id: AudioObjectID) -> Result<Option<String>, String> {
    get_string_property(object_id, K_AUDIO_OBJECT_PROPERTY_NAME)
}

fn get_device_uid(object_id: AudioObjectID) -> Result<Option<String>, String> {
    get_string_property(object_id, K_AUDIO_DEVICE_PROPERTY_DEVICE_UID)
}

/// Look up the human-readable name for a device by its CoreAudio UID.
/// Returns `None` if no device with that UID is found or the name is unavailable.
pub(super) fn get_device_name_for_uid(uid: &str) -> Option<String> {
    let ids = enumerate_audio_devices().ok()?;
    for id in ids {
        if let Ok(Some(dev_uid)) = get_device_uid(id) {
            if dev_uid == uid {
                return get_device_name(id).ok().flatten();
            }
        }
    }
    None
}
fn is_wavis_audio_bridge_name(name: &str) -> bool {
    name == WAVIS_AUDIO_BRIDGE_NAME
}

fn build_bridge_uid(timestamp_millis: u128) -> String {
    format!("{WAVIS_AUDIO_BRIDGE_UID_PREFIX}{timestamp_millis}")
}

fn bridge_uid_timestamp_millis() -> Result<u128, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|err| format!("system clock is before UNIX_EPOCH: {err}"))
}

fn build_subdevice_entry(uid: &str) -> CFDictionary<CFString, CFType> {
    CFDictionary::from_CFType_pairs(&[(
        CFString::new(K_AUDIO_SUB_DEVICE_UID_KEY),
        CFString::new(uid).into_CFType(),
    )])
}

fn build_aggregate_device_description(
    real_output_uid: &str,
    virtual_device_uid: &str,
    bridge_uid: &str,
) -> CFDictionary<CFString, CFType> {
    let subdevices = [
        build_subdevice_entry(real_output_uid),
        build_subdevice_entry(virtual_device_uid),
    ];
    let subdevice_array: CFArray<CFDictionary<CFString, CFType>> =
        CFArray::from_CFTypes(&subdevices);

    CFDictionary::from_CFType_pairs(&[
        (
            CFString::new(K_AUDIO_AGGREGATE_DEVICE_NAME_KEY),
            CFString::new(WAVIS_AUDIO_BRIDGE_NAME).into_CFType(),
        ),
        (
            CFString::new(K_AUDIO_AGGREGATE_DEVICE_UID_KEY),
            CFString::new(bridge_uid).into_CFType(),
        ),
        (
            CFString::new(K_AUDIO_AGGREGATE_DEVICE_SUB_DEVICE_LIST_KEY),
            subdevice_array.into_CFType(),
        ),
        (
            CFString::new(K_AUDIO_AGGREGATE_DEVICE_MAIN_SUB_DEVICE_KEY),
            CFString::new(real_output_uid).into_CFType(),
        ),
        (
            // Must be kCFBooleanTrue (not CFNumber(1)) — the HAL checks the
            // CFTypeID of this value and will silently ignore non-Boolean values,
            // leaving the aggregate as a standard (non-multi-output) device.
            CFString::new(K_AUDIO_AGGREGATE_DEVICE_IS_STACKED_KEY),
            CFBoolean::true_value().into_CFType(),
        ),
    ])
}

#[allow(dead_code)]
pub(super) fn create_multi_output_device(
    real_output_uid: &str,
    virtual_device_uid: &str,
) -> Result<AudioObjectID, String> {
    let bridge_uid = build_bridge_uid(bridge_uid_timestamp_millis()?);
    let description =
        build_aggregate_device_description(real_output_uid, virtual_device_uid, &bridge_uid);
    let mut aggregate_device_id = 0u32;
    let status = unsafe {
        AudioHardwareCreateAggregateDevice(
            description.as_CFTypeRef() as *const c_void,
            &mut aggregate_device_id,
        )
    };
    if status != K_AUDIO_HARDWARE_NO_ERROR {
        return Err(format!(
            "AudioHardwareCreateAggregateDevice failed for '{}' ({bridge_uid}): OSStatus {status}",
            WAVIS_AUDIO_BRIDGE_NAME
        ));
    }
    if aggregate_device_id == 0 {
        return Err("AudioHardwareCreateAggregateDevice returned device id 0".to_string());
    }

    log::info!(
        "[audio_capture] virtual-device: created stacked aggregate {} '{}' uid='{}' \
         real='{}' loopback='{}'",
        aggregate_device_id,
        WAVIS_AUDIO_BRIDGE_NAME,
        bridge_uid,
        real_output_uid,
        virtual_device_uid
    );

    Ok(aggregate_device_id)
}

#[allow(dead_code)]
pub(super) fn swap_system_default_output(
    aggregate_device_id: AudioObjectID,
) -> Result<AudioObjectID, String> {
    let original_device_id = get_default_output_device_id()?;
    set_audio_object_id_property(
        K_AUDIO_OBJECT_SYSTEM_OBJECT,
        K_AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE,
        aggregate_device_id,
    )?;

    share_audio_debug!(
        "wavis: audio_capture: virtual_device: swapped default output {} -> {}",
        original_device_id,
        aggregate_device_id
    );
    share_audio_info!(
        "[audio_capture] virtual-device: swapped system default output {} -> {}",
        original_device_id,
        aggregate_device_id
    );

    Ok(original_device_id)
}

#[allow(dead_code)]
pub(super) fn restore_system_default_output(
    original_device_id: AudioObjectID,
) -> Result<(), String> {
    set_audio_object_id_property(
        K_AUDIO_OBJECT_SYSTEM_OBJECT,
        K_AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE,
        original_device_id,
    )?;

    share_audio_debug!(
        "wavis: audio_capture: virtual_device: restored default output to {}",
        original_device_id
    );
    share_audio_info!(
        "[audio_capture] virtual-device: restored system default output to {}",
        original_device_id
    );

    Ok(())
}

#[allow(dead_code)]
pub(super) fn destroy_multi_output_device(
    aggregate_device_id: AudioObjectID,
) -> Result<(), String> {
    let status = unsafe { AudioHardwareDestroyAggregateDevice(aggregate_device_id) };
    if status != K_AUDIO_HARDWARE_NO_ERROR {
        return Err(format!(
            "AudioHardwareDestroyAggregateDevice failed for device {aggregate_device_id}: \
             OSStatus {status}"
        ));
    }

    share_audio_debug!(
        "wavis: audio_capture: virtual_device: destroyed aggregate device {}",
        aggregate_device_id
    );
    share_audio_info!(
        "[audio_capture] virtual-device: destroyed multi-output device {}",
        aggregate_device_id
    );

    Ok(())
}

#[allow(dead_code)]
pub(super) fn get_real_output_device_uid() -> Result<String, String> {
    let default_output_device_id = get_default_output_device_id()?;
    let uid = get_device_uid(default_output_device_id)?.ok_or_else(|| {
        format!(
            "default output device {} did not expose a CoreAudio UID",
            default_output_device_id
        )
    })?;
    let name = get_device_name(default_output_device_id)
        .ok()
        .flatten()
        .unwrap_or_default();

    log::info!(
        "[audio_capture] virtual-device: current default output device={} name='{}' uid='{}'",
        default_output_device_id,
        name,
        uid
    );

    // Defensive guard: if the current default output is itself a Wavis Audio
    // Bridge, stale cleanup did not fully restore the real speakers. Creating
    // another bridge on top of this one would produce a dead routing chain.
    if uid.starts_with(WAVIS_AUDIO_BRIDGE_UID_PREFIX) || is_wavis_audio_bridge_name(&name) {
        return Err(format!(
            "default output device {} ('{}') is a stale Wavis Audio Bridge — \
             stale cleanup did not restore real speakers; uid='{}'",
            default_output_device_id, name, uid
        ));
    }

    Ok(uid)
}

#[allow(dead_code)]
pub(super) fn cleanup_stale_multi_output_devices() -> Result<(), String> {
    let devices = enumerate_audio_devices()?;
    let default_output_device_id = get_default_output_device_id()?;
    let cleanup_devices = devices
        .iter()
        .map(|&device_id| {
            Ok(StaleCleanupDeviceInfo {
                device_id,
                name: get_device_name(device_id)?,
                uid: get_device_uid(device_id)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    // Log all devices so we can diagnose why stale cleanup sometimes misses bridges.
    log::info!(
        "[audio_capture] virtual-device: cleanup scan: {} devices, default_output={}",
        cleanup_devices.len(),
        default_output_device_id
    );
    for d in &cleanup_devices {
        if d.name
            .as_deref()
            .map(is_wavis_audio_bridge_name)
            .unwrap_or(false)
            || d.uid
                .as_deref()
                .map(|u| u.starts_with(WAVIS_AUDIO_BRIDGE_UID_PREFIX))
                .unwrap_or(false)
        {
            log::info!(
                "[audio_capture] virtual-device: cleanup found bridge device={} name={:?} uid={:?}",
                d.device_id,
                d.name,
                d.uid
            );
        }
    }

    let Some(plan) = plan_stale_multi_output_cleanup(&cleanup_devices, default_output_device_id)?
    else {
        log::info!(
            "[audio_capture] virtual-device: cleanup: no stale bridges found, \
             default_output={default_output_device_id} is clean"
        );
        return Ok(());
    };

    log::info!(
        "[audio_capture] virtual-device: cleanup: found {} stale bridge(s): {:?}; \
         replacement_default={:?}",
        plan.stale_bridge_ids.len(),
        plan.stale_bridge_ids,
        plan.replacement_default_output
    );

    if let Some(replacement) = plan.replacement_default_output {
        if replacement != default_output_device_id {
            set_audio_object_id_property(
                K_AUDIO_OBJECT_SYSTEM_OBJECT,
                K_AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE,
                replacement,
            )?;
            log::info!(
                "[audio_capture] virtual-device: cleanup: reset default output \
                 {} -> {replacement}",
                default_output_device_id
            );
        }
    }

    let mut destroy_errors = Vec::new();
    for bridge_id in plan.stale_bridge_ids {
        log::info!("[audio_capture] virtual-device: cleanup: destroying bridge {bridge_id}");
        if let Err(err) = destroy_multi_output_device(bridge_id) {
            log::warn!(
                "[audio_capture] virtual-device: cleanup: destroy bridge {bridge_id} failed: {err}"
            );
            destroy_errors.push(err);
        }
    }

    if destroy_errors.is_empty() {
        log::info!("[audio_capture] virtual-device: cleanup: complete");
        Ok(())
    } else {
        Err(destroy_errors.join("; "))
    }
}

fn detect_virtual_audio_device_impl() -> Result<Option<VirtualDeviceInfo>, String> {
    let devices = enumerate_audio_devices()?;
    share_audio_debug!(
        "wavis: audio_capture: virtual_device: enumerating {} CoreAudio devices",
        devices.len()
    );

    let mut candidates = Vec::new();

    for device_id in devices {
        let uid = match get_device_uid(device_id) {
            Ok(Some(uid)) => uid,
            Ok(None) => {
                share_audio_debug!(
                    "wavis: audio_capture: virtual_device: device {} missing UID, skipping",
                    device_id
                );
                continue;
            }
            Err(err) => {
                share_audio_debug!(
                    "wavis: audio_capture: virtual_device: device {} UID lookup failed: {}",
                    device_id,
                    err
                );
                continue;
            }
        };

        let name = match get_device_name(device_id) {
            Ok(Some(name)) => name,
            Ok(None) => {
                share_audio_debug!(
                    "wavis: audio_capture: virtual_device: device {} ({}) missing name, skipping",
                    device_id,
                    uid
                );
                continue;
            }
            Err(err) => {
                share_audio_debug!(
                    "wavis: audio_capture: virtual_device: device {} ({}) name lookup failed: {}",
                    device_id,
                    uid,
                    err
                );
                continue;
            }
        };

        share_audio_debug!(
            "wavis: audio_capture: virtual_device: found device {} name='{}' uid='{}'",
            device_id,
            name,
            uid
        );

        candidates.push(VirtualDeviceCandidate {
            device_id,
            uid,
            name,
        });
    }

    if let Some(device) = select_virtual_device_candidate(candidates) {
        share_audio_debug!(
            "wavis: audio_capture: virtual_device: selected device={} name='{}' uid='{}'",
            device.device_id,
            device.name,
            device.uid
        );
        log::info!(
            "[audio_capture] virtual-device: selected loopback device {} '{}' ({})",
            device.device_id,
            device.name,
            device.uid
        );
        Ok(Some(VirtualDeviceInfo {
            device_id: device.device_id,
            uid: device.uid,
            name: device.name,
        }))
    } else {
        share_audio_debug!(
            "wavis: audio_capture: virtual_device: no BlackHole/Loopback device detected"
        );
        share_audio_info!("[audio_capture] virtual-device: no supported loopback device detected");
        Ok(None)
    }
}

#[allow(dead_code)]
pub(super) fn detect_virtual_audio_device() -> Option<VirtualDeviceInfo> {
    match detect_virtual_audio_device_impl() {
        Ok(device) => device,
        Err(err) => {
            share_audio_debug!(
                "wavis: audio_capture: virtual_device: detection failed: {}",
                err
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_aggregate_device_description, build_bridge_uid,
        K_AUDIO_AGGREGATE_DEVICE_IS_STACKED_KEY, K_AUDIO_AGGREGATE_DEVICE_NAME_KEY,
        K_AUDIO_AGGREGATE_DEVICE_SUB_DEVICE_LIST_KEY, K_AUDIO_AGGREGATE_DEVICE_UID_KEY,
    };
    use core_foundation::string::CFString;

    #[test]
    fn bridge_uid_uses_expected_prefix() {
        assert_eq!(
            build_bridge_uid(123456789),
            "com.wavis.audio-bridge-123456789"
        );
    }

    #[test]
    fn aggregate_description_contains_expected_top_level_keys() {
        let description = build_aggregate_device_description(
            "BuiltInOutputDevice",
            "BlackHole2ch",
            "com.wavis.audio-bridge-42",
        );

        assert_eq!(description.len(), 5);
        assert!(description.contains_key(&CFString::new(K_AUDIO_AGGREGATE_DEVICE_NAME_KEY)));
        assert!(description.contains_key(&CFString::new(K_AUDIO_AGGREGATE_DEVICE_UID_KEY)));
        assert!(
            description.contains_key(&CFString::new(K_AUDIO_AGGREGATE_DEVICE_SUB_DEVICE_LIST_KEY))
        );
        assert!(description.contains_key(&CFString::new(K_AUDIO_AGGREGATE_DEVICE_IS_STACKED_KEY)));
    }
}
