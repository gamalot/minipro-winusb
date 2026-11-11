//! Is the programmer (USB 04D8:E11C) plugged in right now?  Asks the Windows PnP manager for *present*
//! devices only, so it turns false the moment the cable is pulled.

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, DIGCF_ALLCLASSES, DIGCF_PRESENT,
    SP_DEVINFO_DATA,
};

pub fn programmer_present() -> bool {
    let enumerator: Vec<u16> = "USB\\VID_04D8&PID_E11C\0".encode_utf16().collect();
    unsafe {
        let set = SetupDiGetClassDevsW(std::ptr::null(), enumerator.as_ptr(), std::ptr::null_mut(), DIGCF_PRESENT | DIGCF_ALLCLASSES);
        if set as isize == -1 || set as usize == 0 {
            return false;
        }
        let mut data: SP_DEVINFO_DATA = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
        let found = SetupDiEnumDeviceInfo(set, 0, &mut data) != 0;
        SetupDiDestroyDeviceInfoList(set);
        found
    }
}
