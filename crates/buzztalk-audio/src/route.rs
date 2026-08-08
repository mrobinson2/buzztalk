//! Output-route detection: is playback currently going to headphones (no
//! acoustic echo path) or to loudspeakers (an echo path exists and AEC is
//! required)?
//!
//! On macOS this queries CoreAudio directly via `objc2-core-audio`
//! (`AudioObjectGetPropertyData`). On every other platform it returns
//! [`OutputRoute::Unknown`] — nothing is wired up there yet, and callers
//! already treat `Unknown` conservatively (see
//! [`OutputRoute::has_echo_path`]).

use buzztalk_core::OutputRoute;

/// Best-effort detection of where system audio output is currently routed.
#[cfg(target_os = "macos")]
pub fn detect_output_route() -> OutputRoute {
    macos::detect()
}

/// Best-effort detection of where system audio output is currently routed.
///
/// No CoreAudio (or equivalent) query is implemented for this platform, so
/// this always returns [`OutputRoute::Unknown`], which callers should treat
/// as "assume an echo path exists" — see [`OutputRoute::has_echo_path`].
#[cfg(not(target_os = "macos"))]
pub fn detect_output_route() -> OutputRoute {
    OutputRoute::Unknown
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use objc2_core_audio::{
        kAudioDevicePropertyDataSource, kAudioDevicePropertyTransportType,
        kAudioDeviceTransportTypeAirPlay, kAudioDeviceTransportTypeBluetooth,
        kAudioDeviceTransportTypeBluetoothLE, kAudioDeviceTransportTypeBuiltIn,
        kAudioDeviceTransportTypeDisplayPort, kAudioDeviceTransportTypeHDMI,
        kAudioDeviceTransportTypeUSB, kAudioHardwarePropertyDefaultOutputDevice,
        kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject, AudioObjectGetPropertyData,
        AudioObjectID, AudioObjectPropertyAddress,
    };
    use std::ffi::c_void;
    use std::mem::MaybeUninit;
    use std::ptr::NonNull;

    // Four-char-code `DataSource` values Apple's built-in audio driver
    // reports for `kAudioDevicePropertyDataSource`. These aren't part of
    // the public HAL header as named constants (they're a driver
    // convention, not part of the CoreAudio API surface), so they're
    // spelled out here from their ASCII bytes:
    //   'ispk' = internal speaker, 'hdpn' = headphones, 'hdst' = headset.
    const DATA_SOURCE_INTERNAL_SPEAKER: u32 = 0x6973_706b;
    const DATA_SOURCE_HEADPHONES: u32 = 0x6864_706e;
    const DATA_SOURCE_HEADSET: u32 = 0x6864_7374;

    pub(super) fn detect() -> OutputRoute {
        let Some(device) = default_output_device() else {
            return OutputRoute::Unknown;
        };
        let Some(transport) = get_property::<u32>(
            device,
            kAudioDevicePropertyTransportType,
            kAudioObjectPropertyScopeGlobal,
        ) else {
            return OutputRoute::Unknown;
        };

        if transport == kAudioDeviceTransportTypeBluetooth
            || transport == kAudioDeviceTransportTypeBluetoothLE
        {
            // Bluetooth speakers exist and would be misclassified here, but
            // Bluetooth output is overwhelmingly headphones/earbuds in
            // practice and CoreAudio's transport type alone can't tell the
            // two apart. Verify against real Bluetooth speakers if that
            // matters for this product.
            return OutputRoute::Headphones;
        }
        if transport == kAudioDeviceTransportTypeBuiltIn {
            return built_in_route(device);
        }
        if transport == kAudioDeviceTransportTypeUSB {
            return usb_route();
        }
        if transport == kAudioDeviceTransportTypeHDMI
            || transport == kAudioDeviceTransportTypeDisplayPort
            || transport == kAudioDeviceTransportTypeAirPlay
        {
            // TV, monitor, or AirPlay receiver: treat as loudspeakers, an
            // acoustic loop is real.
            return OutputRoute::Speakers;
        }
        OutputRoute::Unknown
    }

    /// The built-in device covers both the internal speaker and the
    /// headphone jack under one transport type; CoreAudio distinguishes
    /// them via the device's current `DataSource`, not transport type.
    fn built_in_route(device: AudioObjectID) -> OutputRoute {
        match get_property::<u32>(
            device,
            kAudioDevicePropertyDataSource,
            kAudioObjectPropertyScopeOutput,
        ) {
            Some(DATA_SOURCE_HEADPHONES) | Some(DATA_SOURCE_HEADSET) => OutputRoute::Headphones,
            Some(DATA_SOURCE_INTERNAL_SPEAKER) => OutputRoute::Speakers,
            _ => OutputRoute::Unknown,
        }
    }

    /// USB transport covers everything from a USB headset to a USB audio
    /// interface driving studio monitors; CoreAudio gives no property to
    /// tell those apart, so fall back to a name-hint heuristic using the
    /// same default output device cpal would pick.
    fn usb_route() -> OutputRoute {
        use cpal::traits::HostTrait;
        let name = cpal::default_host()
            .default_output_device()
            .map(|d| d.to_string())
            .unwrap_or_default()
            .to_lowercase();
        const HEADPHONE_HINTS: [&str; 5] =
            ["headphone", "headset", "earbud", "airpods", "earphone"];
        if HEADPHONE_HINTS.iter().any(|hint| name.contains(hint)) {
            OutputRoute::Headphones
        } else {
            OutputRoute::Speakers
        }
    }

    fn default_output_device() -> Option<AudioObjectID> {
        get_property::<AudioObjectID>(
            kAudioObjectSystemObject as AudioObjectID,
            kAudioHardwarePropertyDefaultOutputDevice,
            kAudioObjectPropertyScopeGlobal,
        )
    }

    /// Read a fixed-size CoreAudio property. Returns `None` on any failure
    /// (missing property, size mismatch, non-zero `OSStatus`) rather than
    /// panicking — route detection is a best-effort hint, never a hard
    /// dependency.
    fn get_property<T: Copy>(object: AudioObjectID, selector: u32, scope: u32) -> Option<T> {
        let address = AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: scope,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut size = std::mem::size_of::<T>() as u32;
        let mut value = MaybeUninit::<T>::uninit();

        // SAFETY: `address` and `size` are valid local values we own for
        // the duration of the call; `value` is a local buffer sized to
        // exactly `size_of::<T>()` bytes for CoreAudio to write into, and
        // we only read it back once `status == 0` confirms it wrote a full
        // `T`. `T: Copy` rules out any drop-glue concerns with a partially
        // initialized value.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                NonNull::from(&address),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new(value.as_mut_ptr() as *mut c_void)?,
            )
        };

        if status == 0 && size as usize == std::mem::size_of::<T>() {
            Some(unsafe { value.assume_init() })
        } else {
            None
        }
    }
}
