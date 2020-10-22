//! This module handles the actual boot.

use alloc::vec::Vec;

use core::convert::{identity, TryInto};

use uefi::prelude::*;
use uefi::proto::media::file::Directory;
use uefi::table::boot::{AllocateType, MemoryType};

use log::{debug, info, error};

use multiboot1::{Addresses, Metadata};

use crate::config::Entry;

/// Prepare an entry for boot.
///
/// What this means:
/// 1. load the kernel into memory
/// 2. try to parse the Multiboot information
/// 3. move the kernel to where it wants to be
/// 4. load the modules
/// 5. make the framebuffer ready
/// 6. create the Multiboot information for the kernel
///
/// Return a `PreparedEntry` which can be used to actually boot.
/// This is non-destructive and will always return.
pub(crate) fn prepare_entry<'a>(
    entry: &'a Entry, volume: &mut Directory, systab: &SystemTable<Boot>
) -> Result<PreparedEntry<'a>, Status> {
    let kernel_vec = crate::read_file(&entry.image, volume)?;
    let metadata = multiboot1::parse(kernel_vec.as_slice()).map_err(|e| {
        error!("invalid Multiboot header: {:?}", e);
        Status::LOAD_ERROR
    })?;
    debug!("loaded kernel: {:?}", metadata);
    let addresses = match &metadata.addresses {
        Addresses::Multiboot(addr) => addr,
        Addresses::Elf(elf) => todo!("handle ELF addresses")
    };
    
    // try to allocate the memory where to load the kernel and move the kernel there
    // TODO: maybe optimize this so that we at first read just the beginning of the kernel
    // and then read the whole kernel into the right place directly
    // The current implementation is fast enough
    // (we're copying just a few megabytes through memory),
    // but in some cases we could block the destination with the source and this would be bad.
    info!("moving the kernel to its desired location...");
    // allocate
    let kernel_length: usize = {
        if addresses.bss_end_address == 0 {addresses.load_end_address - addresses.load_address}
        else {addresses.bss_end_address - addresses.load_address}
    }.try_into().unwrap();
    let kernel_pages = (kernel_length / 4096) + 1; // TODO: this may allocate a page too much
    let kernel_ptr = systab.boot_services().allocate_pages(
        AllocateType::Address(addresses.load_address.try_into().unwrap()),
        MemoryType::LOADER_DATA,
        kernel_pages.try_into().unwrap() // page size
    ).map_err(|e| {
        error!("failed to allocate memory to place the kernel: {:?}", e);
        Status::LOAD_ERROR
    })?.unwrap();
    let kernel_buf = unsafe {
        core::slice::from_raw_parts_mut(kernel_ptr as *mut u8, kernel_length)
    };
    // copy from beginning of text to end of data segment and fill the rest with zeroes
    kernel_buf.iter_mut().zip(
        kernel_vec.iter()
        .skip(addresses.load_offset.try_into().unwrap())
        .take((addresses.load_end_address - addresses.load_address).try_into().unwrap())
        .chain(core::iter::repeat(&0))
    )
    .for_each(|(dst,src)| *dst = *src);
    // drop the old vector
    core::mem::drop(kernel_vec);
    
    // Load all modules, fail completely if one fails to load.
    let modules_vec: Vec<Vec<u8>> = entry.modules.iter().flat_map(identity).map(|module|
        crate::read_file(&module.image, volume)
    ).collect::<Result<Vec<_>, _>>()?;
    info!("loaded {} modules", modules_vec.len());
    
    
    // TODO: Steps 5 and 6
    Ok(PreparedEntry { entry, kernel_ptr, kernel_pages, metadata, modules_vec })
}

pub(crate) struct PreparedEntry<'a> {
    entry: &'a Entry,
    // this has been allocated via allocate_pages(), so it's not tracked by Rust
    // we have to explicitly take care of disposing this if a boot fails
    kernel_ptr: u64,
    kernel_pages: usize,
    metadata: Metadata,
    modules_vec: Vec<Vec<u8>>,
    // TODO: framebuffer and Multiboot information
}

impl Drop for PreparedEntry<'_> {
    /// Abort the boot.
    ///
    /// Disposes the loaded kernel and modules and restores the framebuffer.
    fn drop(&mut self) {
        // We can't free memory after we've exited boot services.
        // But this only happens in `PreparedEntry::boot` and this function doesn't return.
        let systab_ptr = uefi_services::system_table();
        let systab = unsafe { systab_ptr.as_ref() };
        systab.boot_services().free_pages(self.kernel_ptr, self.kernel_pages)
        // let's just panic if we can't free
        .expect("failed to free the allocated memory for the kernel").unwrap();
        // TODO: restore the framebuffer
    }
}

impl PreparedEntry<'_> {
    /// Actuelly boot an entry.
    ///
    /// What this means:
    /// 1. exit BootServices
    /// 2. when on x64_64: switch to x86
    /// 3. jump!
    ///
    /// This function won't return.
    pub(crate) fn boot(&self, image: Handle, systab: SystemTable<Boot>) {
        match &self.entry.name {
            Some(n) => info!("booting '{}'...", n),
            None => info!("booting..."),
        }
        
        // allocate memory for the memory map
        // also, keep a bit of room
        info!("exiting boot services...");
        let mut mmap_vec = Vec::<u8>::new();
        mmap_vec.resize(systab.boot_services().memory_map_size() + 100, 0);
        let (systab, mmap_iter) = systab.exit_boot_services(image, mmap_vec.as_mut_slice())
        .expect("failed to exit boot services").unwrap();
        // now, write! won't work anymore. Also, we can't allocate any memory.
        
        // TODO: Step 2
        
        let addresses = match &self.metadata.addresses {
            Addresses::Multiboot(addr) => addr,
            Addresses::Elf(elf) => todo!("handle ELF addresses")
        };
        // TODO: Not sure whether this works. We don't get any errors.
        let entry_ptr = unsafe {core::mem::transmute::<_, fn()>(addresses.entry_address as usize)};
        entry_ptr();
        unreachable!();
    }
}
