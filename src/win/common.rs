use core::fmt;
use std::ptr::null_mut;

use winapi::{
    shared::{
        guiddef,
        ksmedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, KSDATAFORMAT_SUBTYPE_PCM},
        winerror::S_OK,
    },
    um::winbase::{
        FormatMessageA, LocalFree, FORMAT_MESSAGE_ALLOCATE_BUFFER,
        FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS,
    },
};

#[macro_export]
macro_rules! read_unaligned {
    ($v:ident $(. $field:ident)*) => {
        std::ptr::addr_of!((*$v) $(.$field)* ).read_unaligned()
    };
}

pub struct WinError(pub i32);

impl fmt::Debug for WinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WinError(id: {:x}, {})", self.0, error_to_string(self.0))
    }
}

#[track_caller]
pub fn winapi_result(hresult: i32) -> Result<(), WinError> {
    if hresult == S_OK {
        Ok(())
    } else {
        Err(WinError(hresult))
    }
}

fn error_to_string(code: i32) -> String {
    let mut buffer: *mut i8 = null_mut();
    unsafe {
        let size = FormatMessageA(
            FORMAT_MESSAGE_ALLOCATE_BUFFER
                | FORMAT_MESSAGE_FROM_SYSTEM
                | FORMAT_MESSAGE_IGNORE_INSERTS,
            null_mut(),
            code as u32,
            0,
            &mut buffer as *mut _ as *mut i8,
            0,
            null_mut(),
        );
        let slice = std::slice::from_raw_parts(buffer as _, size as usize);
        let str = std::str::from_utf8(slice).unwrap();
        let string = str.to_string();
        LocalFree(buffer as _);
        string
    }
}

#[derive(PartialEq, Eq)]
pub struct Guid(u32, u16, u16, [u8; 8]);

impl Guid {
    pub const fn from_winapi(guid: guiddef::GUID) -> Self {
        Self(guid.Data1, guid.Data2, guid.Data3, guid.Data4)
    }
}

impl From<guiddef::GUID> for Guid {
    fn from(guid: guiddef::GUID) -> Self {
        Self::from_winapi(guid)
    }
}

pub const _AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM: u32 = 0x80000000;
pub const _AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY: u32 = 0x08000000;

pub const DATAFORMAT_SUBTYPE_PCM: Guid =
    Guid::from_winapi(KSDATAFORMAT_SUBTYPE_PCM);
pub const DATAFORMAT_SUBTYPE_IEEE_FLOAT: Guid =
    Guid::from_winapi(KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
