//! Small operating-system primitives shared by persistence and installers.

use std::fs;
use std::io;
use std::path::Path;

#[cfg(unix)]
pub(crate) fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(unix)]
pub(crate) fn move_new_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::hard_link(source, destination)?;
    fs::remove_file(source)
}

#[cfg(windows)]
pub(crate) fn move_new_file(source: &Path, destination: &Path) -> io::Result<()> {
    // A same-directory rename preserves create-new semantics on Windows without
    // requiring hard-link support (for example on FAT/exFAT volumes or shares).
    fs::rename(source, destination)
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(windows)]
pub(crate) fn sync_directory(_path: &Path) -> io::Result<()> {
    // Windows does not allow opening a directory with the portable File API.
    // MoveFileExW with MOVEFILE_WRITE_THROUGH provides the durability boundary.
    Ok(())
}
#[cfg(unix)]
pub(crate) fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{addr_of_mut, null_mut};
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAceEx, GetLengthSid,
        GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor, SE_DACL_PROTECTED,
        SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorControl,
        SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{CreateDirectoryW, FILE_ALL_ACCESS};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let mut size = 0;
        unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut size) };
        // usize storage gives TOKEN_USER its required pointer alignment.
        let mut user = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
        if unsafe {
            GetTokenInformation(token, TokenUser, user.as_mut_ptr().cast(), size, &mut size)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let sid = unsafe { (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid };
        let sid_size = unsafe { GetLengthSid(sid) } as usize;
        let acl_size =
            size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + sid_size;
        let mut acl = vec![0u32; acl_size.div_ceil(size_of::<u32>())];
        let acl = acl.as_mut_ptr().cast::<ACL>();
        // OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE: context files inherit the
        // single current-user grant rather than a permissive temp-folder ACL.
        if unsafe { InitializeAcl(acl, acl_size as u32, ACL_REVISION) } == 0
            || unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION, 3, FILE_ALL_ACCESS, sid) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut descriptor: SECURITY_DESCRIPTOR = unsafe { zeroed() };
        let descriptor_pointer = addr_of_mut!(descriptor).cast();
        if unsafe { InitializeSecurityDescriptor(descriptor_pointer, 1) } == 0
            || unsafe { SetSecurityDescriptorDacl(descriptor_pointer, 1, acl, 0) } == 0
            || unsafe {
                SetSecurityDescriptorControl(
                    descriptor_pointer,
                    SE_DACL_PROTECTED,
                    SE_DACL_PROTECTED,
                )
            } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor_pointer,
            bInheritHandle: 0,
        };
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe { CreateDirectoryW(path.as_ptr(), &attributes) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    })();
    unsafe { CloseHandle(token) };
    result
}

/// TLS defaults for clients using the platform's compiled HTTPS backend.
pub(crate) fn tls_config() -> ureq::tls::TlsConfig {
    let builder = ureq::tls::TlsConfig::builder();
    #[cfg(windows)]
    let builder = builder
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier);
    builder.build()
}
