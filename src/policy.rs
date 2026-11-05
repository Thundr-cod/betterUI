// IPolicyConfig: the (undocumented but long-stable) COM interface Windows'
// own Sound settings use to change the default audio device. Used the same
// way by EarTrumpet, SoundSwitch and AudioDeviceCmdlets. Only
// SetDefaultEndpoint is called; the earlier methods exist only to keep the
// vtable layout correct.

#![allow(non_snake_case)]

use windows::core::*;
use windows::Win32::Media::Audio::ERole;

pub const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

#[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
pub unsafe trait IPolicyConfig: IUnknown {
    fn GetMixFormat(&self) -> HRESULT;
    fn GetDeviceFormat(&self) -> HRESULT;
    fn ResetDeviceFormat(&self) -> HRESULT;
    fn SetDeviceFormat(&self) -> HRESULT;
    fn GetProcessingPeriod(&self) -> HRESULT;
    fn SetProcessingPeriod(&self) -> HRESULT;
    fn GetShareMode(&self) -> HRESULT;
    fn SetShareMode(&self) -> HRESULT;
    fn GetPropertyValue(&self) -> HRESULT;
    fn SetPropertyValue(&self) -> HRESULT;
    fn SetDefaultEndpoint(&self, device_id: PCWSTR, role: ERole) -> HRESULT;
    fn SetEndpointVisibility(&self) -> HRESULT;
}

/// Make `device_id` the default endpoint for every role (apps, media, calls).
/// Lives in this module because the interface's methods are private to it.
pub fn set_default_endpoint(device_id: &str) {
    use windows::Win32::Media::Audio::{eCommunications, eConsole, eMultimedia};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
    let wide = HSTRING::from(device_id);
    unsafe {
        match CoCreateInstance::<_, IPolicyConfig>(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) {
            Ok(pc) => {
                for role in [eConsole, eMultimedia, eCommunications] {
                    let hr = pc.SetDefaultEndpoint(PCWSTR(wide.as_ptr()), role);
                    if hr.is_err() {
                        crate::log::write(&format!("SetDefaultEndpoint failed: {hr:?}"));
                    }
                }
            }
            Err(e) => crate::log::write(&format!("PolicyConfig unavailable: {e:?}")),
        }
    }
}
