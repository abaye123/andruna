//! Virtual memory: placeholders, private allocations, commit and protection.
//!
//! All addresses are passed as `usize`. The wrappers never dereference them;
//! Windows validates the ranges and fails the call when they do not describe
//! memory in the expected state. Callers ([`crate::mem`]) only pass ranges
//! they own in their address-space table, so these calls cannot change
//! memory that other code in the process owns.

use core::ffi::c_void;

use windows::Win32::System::Memory::{
    MEM_COMMIT, MEM_DECOMMIT, MEM_RELEASE, MEM_REPLACE_PLACEHOLDER, MEM_RESERVE,
    MEM_RESERVE_PLACEHOLDER, MEMORY_BASIC_INFORMATION, PAGE_NOACCESS, PAGE_PROTECTION_FLAGS,
    VIRTUAL_ALLOCATION_TYPE, VIRTUAL_FREE_TYPE, VirtualAlloc2, VirtualFree, VirtualProtect,
    VirtualQuery,
};

use crate::errno::{Errno, SysResult};

/// `MEM_PRESERVE_PLACEHOLDER` for `VirtualFree` (the crate types it for
/// `UnmapViewOfFile2`).
const MEM_PRESERVE_PLACEHOLDER: u32 = 0x2;

fn last_error() -> Errno {
    Errno::from(windows::core::Error::from_thread())
}

/// Reserves `size` bytes as one placeholder, at `addr` or anywhere.
pub fn reserve_placeholder(addr: Option<usize>, size: usize) -> SysResult<usize> {
    // SAFETY: reserving address space does not touch existing memory; with a
    // fixed address Windows fails if any part of the range is in use.
    let p = unsafe {
        VirtualAlloc2(
            None,
            addr.map(|a| a as *const c_void),
            size,
            MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
            PAGE_NOACCESS.0,
            None,
        )
    };
    if p.is_null() {
        Err(last_error())
    } else {
        Ok(p as usize)
    }
}

/// Splits the placeholder that starts at `addr` so that `[addr, addr+size)`
/// becomes a placeholder of its own.
pub fn split_placeholder(addr: usize, size: usize) -> SysResult<()> {
    free(addr, size, MEM_RELEASE.0 | MEM_PRESERVE_PLACEHOLDER)
}

/// Replaces the placeholder `[addr, addr+size)` with a private allocation,
/// committed with `protect` when it is given, reserved only otherwise.
pub fn replace_placeholder(
    addr: usize,
    size: usize,
    protect: Option<PAGE_PROTECTION_FLAGS>,
) -> SysResult<()> {
    let (ty, prot) = match protect {
        Some(p) => (MEM_RESERVE | MEM_COMMIT | MEM_REPLACE_PLACEHOLDER, p),
        None => (MEM_RESERVE | MEM_REPLACE_PLACEHOLDER, PAGE_NOACCESS),
    };
    alloc_at(addr, size, ty, prot)
}

/// Turns the whole private allocation `[addr, addr+size)` back into a
/// placeholder.
pub fn release_to_placeholder(addr: usize, size: usize) -> SysResult<()> {
    free(addr, size, MEM_RELEASE.0 | MEM_PRESERVE_PLACEHOLDER)
}

/// Releases the placeholder (or allocation) that starts at `addr`.
pub fn release(addr: usize) -> SysResult<()> {
    free(addr, 0, MEM_RELEASE.0)
}

/// Commits pages of a reserved private allocation (zero-filled).
pub fn commit(addr: usize, size: usize, protect: PAGE_PROTECTION_FLAGS) -> SysResult<()> {
    alloc_at(addr, size, MEM_COMMIT, protect)
}

/// Decommits pages of a private allocation; their contents are discarded.
pub fn decommit(addr: usize, size: usize) -> SysResult<()> {
    free(addr, size, MEM_DECOMMIT.0)
}

/// Changes the protection of committed pages.
pub fn protect(addr: usize, size: usize, protect: PAGE_PROTECTION_FLAGS) -> SysResult<()> {
    let mut old = PAGE_PROTECTION_FLAGS(0);
    // SAFETY: Windows validates that the range is committed memory; a
    // protection change does not move or free memory.
    unsafe { VirtualProtect(addr as *const c_void, size, protect, &mut old) }.map_err(Errno::from)
}

/// What `VirtualQuery` reports for the region containing an address.
#[derive(Clone, Copy, Debug)]
pub struct Region {
    pub base: usize,
    pub size: usize,
    pub state: VIRTUAL_ALLOCATION_TYPE,
    pub protect: PAGE_PROTECTION_FLAGS,
}

/// `VirtualQuery`, or `None` for an address outside the user address space.
pub fn query(addr: usize) -> Option<Region> {
    let mut info = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: `info` is a valid out buffer of the size passed.
    let n = unsafe {
        VirtualQuery(
            Some(addr as *const c_void),
            &mut info,
            size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    (n != 0).then_some(Region {
        base: info.BaseAddress as usize,
        size: info.RegionSize,
        state: info.State,
        protect: info.Protect,
    })
}

fn alloc_at(
    addr: usize,
    size: usize,
    ty: VIRTUAL_ALLOCATION_TYPE,
    prot: PAGE_PROTECTION_FLAGS,
) -> SysResult<()> {
    // SAFETY: the range is reserved by lx (see the module documentation);
    // committing or replacing a placeholder does not affect other memory.
    let p = unsafe { VirtualAlloc2(None, Some(addr as *const c_void), size, ty, prot.0, None) };
    if p.is_null() {
        Err(last_error())
    } else {
        debug_assert_eq!(p as usize, addr);
        Ok(())
    }
}

fn free(addr: usize, size: usize, ty: u32) -> SysResult<()> {
    // SAFETY: the range is owned by lx (see the module documentation);
    // Windows rejects ranges that do not match the requested operation.
    unsafe { VirtualFree(addr as *mut c_void, size, VIRTUAL_FREE_TYPE(ty)) }.map_err(Errno::from)
}
