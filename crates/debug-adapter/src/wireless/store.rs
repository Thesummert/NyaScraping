use super::*;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
};

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SavedDevice {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub token: String,
}
impl SavedDevice {
    pub fn public(&self) -> WirelessDevice {
        WirelessDevice {
            id: self.id.clone(),
            name: self.name.clone(),
            endpoint: self.endpoint.clone(),
        }
    }
}

pub(super) struct Store {
    path: PathBuf,
    pub devices: Vec<SavedDevice>,
    _lock: File,
}
impl Store {
    pub fn open(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory).map_err(error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(error)?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(directory.join("pairing.lock"))
            .map_err(error)?;
        lock.lock().map_err(error)?;
        let path = directory.join("paired-devices.dat");
        let devices = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&protect(&bytes, false)?).map_err(error)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(error(e)),
        };
        Ok(Self {
            path,
            devices,
            _lock: lock,
        })
    }
    pub fn save(&self) -> Result<()> {
        let directory = self.path.parent().unwrap();
        let bytes = protect(&serde_json::to_vec(&self.devices).map_err(error)?, true)?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory).map_err(error)?;
        temporary.write_all(&bytes).map_err(error)?;
        temporary.as_file().sync_all().map_err(error)?;
        temporary.persist(&self.path).map_err(error)?;
        Ok(())
    }
}

#[cfg(not(windows))]
fn protect(bytes: &[u8], _encrypt: bool) -> Result<Vec<u8>> {
    Ok(bytes.to_vec())
}

#[cfg(windows)]
fn protect(bytes: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len().try_into().map_err(error)?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // Match EmberMobile's CLI: bind credentials to the current Windows user.
    unsafe {
        let success = if encrypt {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if success == 0 {
            return Err(error(std::io::Error::last_os_error()));
        }
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        Ok(bytes)
    }
}
