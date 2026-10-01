use memmap2::Mmap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Callbacks {
    pub context: *mut std::ffi::c_void,
    pub read: unsafe extern "C" fn(*mut std::ffi::c_void, *mut u8, usize, *mut usize) -> i32,
    pub write: unsafe extern "C" fn(*mut std::ffi::c_void, *const u8, usize) -> i32,
    pub seek: unsafe extern "C" fn(*mut std::ffi::c_void, i64, i32, *mut u64) -> i32,
    pub flush: unsafe extern "C" fn(*mut std::ffi::c_void) -> i32,
    pub seekable: u32,
}
fn check(status: i32) -> io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::other("Managed Stream callback failed"))
    }
}
pub enum Output {
    File(File),
    Stream(Callbacks),
}
impl Write for Output {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self {
            Self::File(f) => f.write(data),
            Self::Stream(c) => {
                check(unsafe { (c.write)(c.context, data.as_ptr(), data.len()) })?;
                Ok(data.len())
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::File(f) => f.flush(),
            Self::Stream(c) => check(unsafe { (c.flush)(c.context) }),
        }
    }
}
impl Seek for Output {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            Self::File(f) => f.seek(pos),
            Self::Stream(c) => callback_seek(c, pos),
        }
    }
}
fn callback_seek(c: &Callbacks, pos: SeekFrom) -> io::Result<u64> {
    let (offset, origin) = match pos {
        SeekFrom::Start(n) => (
            i64::try_from(n).map_err(|_| io::Error::other("Offset exceeds Int64"))?,
            0,
        ),
        SeekFrom::Current(n) => (n, 1),
        SeekFrom::End(n) => (n, 2),
    };
    let mut result = 0;
    check(unsafe { (c.seek)(c.context, offset, origin, &mut result) })?;
    Ok(result)
}
#[cfg(test)]
thread_local! {
    pub(super) static SYNC_TEST: std::cell::Cell<(u32, bool)> = const { std::cell::Cell::new((0, false)) };
}
impl Output {
    pub fn sync_all(&mut self) -> io::Result<()> {
        match self {
            Self::File(f) => {
                #[cfg(test)]
                SYNC_TEST.with(|state| {
                    let (calls, fail) = state.get();
                    state.set((calls + 1, fail));
                    if fail { Err(io::Error::other("Injected sync failure")) } else { Ok(()) }
                })?;
                f.sync_all()
            },
            Self::Stream(_) => Err(io::Error::new(io::ErrorKind::Unsupported, "Stream persistence is managed by the caller")),
        }
    }
}
pub struct MappedInput { pub mapping: Mmap, pub _file: File, pub _registration: mcap::storage::MappingRegistration }
impl std::ops::Deref for MappedInput { type Target = [u8]; fn deref(&self) -> &[u8] { &self.mapping } }
impl mcap::storage::SharedSource for MappedInput {}
impl AsRef<[u8]> for MappedInput { fn as_ref(&self) -> &[u8] { &self.mapping } }
/// A reader input pin, distinct from the storage retained by mapped payload slices.
pub struct MappingOwner(Option<mcap::charged::ChargedShared<MappedInput>>);
impl MappingOwner {
    pub fn new(mapping: Mmap, file: File, domain: &mcap::storage::BudgetRef) -> super::Outcome<Self> {
        let root=mcap::charged::ChargedShared::new_fixed(MappedInput {
            _registration:domain.register_mapping(mapping.len() as u64)?, mapping, _file:file,
        },domain,mcap::storage::ResourceCategory::Scratch)?;
        root.charge_owner(mcap::storage::OwnerKind::Parser,true);
        Ok(Self(Some(root)))
    }
    pub fn into_shared(mut self) -> mcap::charged::ChargedShared<MappedInput> {
        let root=self.0.take().unwrap();
        root.charge_owner(mcap::storage::OwnerKind::Parser,false);
        root
    }
    pub fn shared(&self, range: std::ops::Range<usize>) -> mcap::storage::SharedBytes {
        mcap::storage::SharedBytes::charged_external(self.0.as_ref().unwrap().clone().into_source(),range)
    }
}
impl std::ops::Deref for MappingOwner {
    type Target=MappedInput;
    fn deref(&self) -> &MappedInput { self.0.as_deref().unwrap() }
}
impl Drop for MappingOwner {
    fn drop(&mut self) {
        if let Some(root)=&self.0 { root.charge_owner(mcap::storage::OwnerKind::Parser,false); }
    }
}
pub enum Input {
    Map {
        mapping: MappingOwner,
        position: usize,
    },
    Stream(Callbacks),
}
impl Read for Input {
    fn read(&mut self, data: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Map {
                mapping, position, ..
            } => {
                let n = data.len().min(mapping.len().saturating_sub(*position));
                data[..n].copy_from_slice(&mapping[*position..*position + n]);
                *position += n;
                Ok(n)
            }
            Self::Stream(c) => {
                let mut n = 0;
                check(unsafe { (c.read)(c.context, data.as_mut_ptr(), data.len(), &mut n) })?;
                if n > data.len() {
                    return Err(io::Error::other("Invalid read count"));
                }
                Ok(n)
            }
        }
    }
}
impl Seek for Input {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Map {
                mapping, position, ..
            } => {
                let n = match pos {
                    SeekFrom::Start(n) => i128::from(n),
                    SeekFrom::Current(n) => *position as i128 + i128::from(n),
                    SeekFrom::End(n) => mapping.len() as i128 + i128::from(n),
                };
                if n < 0 || n > mapping.len() as i128 {
                    return Err(io::Error::other("Invalid seek"));
                }
                *position = n as usize;
                Ok(*position as u64)
            }
            Self::Stream(c) => callback_seek(c, pos),
        }
    }
}
impl Input {
    pub fn seekable(&self) -> bool {
        match self {
            Self::Map { .. } => true,
            Self::Stream(c) => c.seekable != 0,
        }
    }
}

#[cfg(test)]
mod mapping_tests {
    use super::*;
    use mcap::storage::{BudgetLimits, OwnerKind};
    #[test]
    fn input_pin_releases_before_last_mapped_lease() {
        let path=std::env::temp_dir().join(format!("fizzy-input-map-control-{}.bin",std::process::id()));
        std::fs::write(&path,[9;128]).unwrap();
        let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let file=File::open(&path).unwrap();
        let map=unsafe { Mmap::map(&file).unwrap() };
        let owner=MappingOwner::new(map,file,&domain).unwrap();
        let capacity=domain.workload_statistics().current;
        let lease=owner.shared(3..7).clone_for(OwnerKind::Lease);
        assert_eq!(domain.ownership_statistics().bytes[OwnerKind::Parser as usize],capacity);
        assert_eq!(domain.ownership_statistics().externally_releasable,0);
        assert_eq!(domain.workload_detailed_statistics().lease_payload_bytes,0);
        drop(owner);
        assert_eq!(domain.ownership_statistics().bytes[OwnerKind::Parser as usize],0);
        assert_eq!(domain.ownership_statistics().externally_releasable,capacity);
        assert_eq!(domain.mapped_logical_bytes(),128);
        assert_eq!(lease.as_ref(),&[9;4]);
        drop(lease);
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.mapped_logical_bytes(),0);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn refused_mapping_control_rolls_back_mapping_registration() {
        let path=std::env::temp_dir().join(format!("fizzy-refused-map-control-{}.bin",std::process::id()));
        std::fs::write(&path,[9;128]).unwrap();
        let domain=mcap::storage::BudgetRef::new(BudgetLimits { total:(1) + mcap::storage::BudgetRef::allocation_size(), block:1, retained:0 }).unwrap();
        let file=File::open(&path).unwrap();
        let map=unsafe { Mmap::map(&file).unwrap() };
        assert!(MappingOwner::new(map,file,&domain).is_err());
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.mapped_logical_bytes(),0);
        assert_eq!(domain.ownership_statistics(),Default::default());
        std::fs::remove_file(path).unwrap();
    }
}
