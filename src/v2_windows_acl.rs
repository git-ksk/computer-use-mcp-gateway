//! Windows ACL validation for V2 key and trust material.
//!
//! The runtime trusts only the current process identity (the configured service
//! identity), LocalSystem, and BUILTIN\Administrators. Inherited allow ACEs are
//! evaluated exactly like explicit allow ACEs. An untrusted owner, null DACL,
//! unsupported allow-ACE shape, or security API failure fails closed.

use std::{ffi::c_void, io, mem, os::windows::ffi::OsStrExt, path::Path, ptr};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_SUCCESS, GENERIC_ALL, GENERIC_READ, GENERIC_WRITE, HANDLE, LocalFree,
    },
    Security::{
        ACCESS_ALLOWED_ACE, ACL_SIZE_INFORMATION, AclSizeInformation,
        Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT},
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation, GetTokenInformation,
        IsWellKnownSid, OWNER_SECURITY_INFORMATION, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
        WinBuiltinAdministratorsSid, WinLocalSystemSid,
    },
    Storage::FileSystem::{
        DELETE, FILE_APPEND_DATA, FILE_DELETE_CHILD, FILE_READ_ATTRIBUTES, FILE_READ_DATA,
        FILE_READ_EA, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA, WRITE_DAC,
        WRITE_OWNER,
    },
    System::{
        SystemServices::{
            ACCESS_ALLOWED_ACE_TYPE, ACCESS_ALLOWED_CALLBACK_ACE_TYPE,
            ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE, ACCESS_ALLOWED_COMPOUND_ACE_TYPE,
            ACCESS_ALLOWED_OBJECT_ACE_TYPE,
        },
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AclSubject {
    SecretFile,
    PublicTrustFile,
    ParentDirectory,
}

#[derive(Debug)]
pub(crate) enum AclCheckError {
    Io(io::Error),
    UntrustedOwner,
    UntrustedAccess,
}

pub(crate) fn validate_acl(path: &Path, subject: AclSubject) -> Result<(), AclCheckError> {
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let current_sid = CurrentProcessSid::load().map_err(AclCheckError::Io)?;
    let mut owner: PSID = ptr::null_mut();
    let mut dacl = ptr::null_mut();
    let mut descriptor = ptr::null_mut();

    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(AclCheckError::Io(io::Error::from_raw_os_error(
            status as i32,
        )));
    }
    let descriptor = LocalSecurityDescriptor(descriptor);

    if owner.is_null() || !is_trusted_sid(owner, current_sid.sid()) {
        return Err(AclCheckError::UntrustedOwner);
    }
    if dacl.is_null() {
        return Err(AclCheckError::UntrustedAccess);
    }

    let mut info = ACL_SIZE_INFORMATION::default();
    let ok = unsafe {
        GetAclInformation(
            dacl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast::<c_void>(),
            mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    if ok == 0 {
        return Err(AclCheckError::Io(io::Error::last_os_error()));
    }

    for index in 0..info.AceCount {
        let mut raw_ace = ptr::null_mut();
        if unsafe { GetAce(dacl, index, &mut raw_ace) } == 0 {
            return Err(AclCheckError::Io(io::Error::last_os_error()));
        }
        if raw_ace.is_null() {
            return Err(AclCheckError::UntrustedAccess);
        }
        let header = unsafe { &*(raw_ace.cast::<windows_sys::Win32::Security::ACE_HEADER>()) };
        let ace_type = header.AceType as u32;
        if ace_type == ACCESS_ALLOWED_ACE_TYPE {
            let ace = unsafe { &*(raw_ace.cast::<ACCESS_ALLOWED_ACE>()) };
            let sid = ptr::addr_of!(ace.SidStart).cast::<c_void>() as PSID;
            if !is_trusted_sid(sid, current_sid.sid()) && mask_is_sensitive(ace.Mask, subject) {
                return Err(AclCheckError::UntrustedAccess);
            }
        } else if is_other_allow_ace(ace_type) {
            // Object/callback/compound allow ACEs have different SID layouts. They are
            // uncommon for ordinary NTFS key files; accepting them without evaluating
            // their trustee would create a bypass, so fail closed instead.
            return Err(AclCheckError::UntrustedAccess);
        }
    }

    drop(descriptor);
    Ok(())
}

fn mask_is_sensitive(mask: u32, subject: AclSubject) -> bool {
    let write = FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | FILE_WRITE_EA
        | FILE_WRITE_ATTRIBUTES
        | DELETE
        | WRITE_DAC
        | WRITE_OWNER
        | GENERIC_WRITE
        | GENERIC_ALL;
    match subject {
        AclSubject::SecretFile => {
            let read =
                FILE_READ_DATA | FILE_READ_EA | FILE_READ_ATTRIBUTES | GENERIC_READ | GENERIC_ALL;
            mask & (read | write) != 0
        }
        AclSubject::PublicTrustFile => mask & write != 0,
        AclSubject::ParentDirectory => mask & (write | FILE_DELETE_CHILD) != 0,
    }
}

fn is_other_allow_ace(ace_type: u32) -> bool {
    matches!(
        ace_type,
        ACCESS_ALLOWED_COMPOUND_ACE_TYPE
            | ACCESS_ALLOWED_OBJECT_ACE_TYPE
            | ACCESS_ALLOWED_CALLBACK_ACE_TYPE
            | ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE
    )
}

fn is_trusted_sid(candidate: PSID, current: PSID) -> bool {
    if candidate.is_null() {
        return false;
    }
    unsafe {
        EqualSid(candidate, current) != 0
            || IsWellKnownSid(candidate, WinLocalSystemSid) != 0
            || IsWellKnownSid(candidate, WinBuiltinAdministratorsSid) != 0
    }
}

struct CurrentProcessSid {
    token: HANDLE,
    buffer: Vec<u8>,
}

impl CurrentProcessSid {
    fn load() -> io::Result<Self> {
        let mut token: HANDLE = ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut needed = 0_u32;
        unsafe {
            GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut needed);
        }
        if needed < mem::size_of::<TOKEN_USER>() as u32 {
            unsafe {
                CloseHandle(token);
            }
            return Err(io::Error::last_os_error());
        }

        let mut buffer = vec![0_u8; needed as usize];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast::<c_void>(),
                needed,
                &mut needed,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            unsafe {
                CloseHandle(token);
            }
            return Err(error);
        }
        Ok(Self { token, buffer })
    }

    fn sid(&self) -> PSID {
        unsafe { (*(self.buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid }
    }
}

impl Drop for CurrentProcessSid {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.token);
        }
    }
}

struct LocalSecurityDescriptor(windows_sys::Win32::Security::PSECURITY_DESCRIPTOR);

impl Drop for LocalSecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0.cast::<c_void>());
            }
        }
    }
}
