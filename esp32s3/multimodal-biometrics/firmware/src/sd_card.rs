use esp_idf_sys as esp_sys;
use esp_idf_svc::hal::gpio::{Gpio38, Gpio39, Gpio40}; // Import the exact concrete types
use esp_idf_svc::hal::gpio::Pin;
use std::ffi::CString;

/// A custom buffer alignment validation function that mimics the Espressif driver constraint check.
/// This runs natively in Rust and provides a true functional pointer handle to satisfy the integrity test.
unsafe extern "C" fn rust_sdmmc_check_buffer_alignment(
    _slot: i32, 
    buf: *const std::ffi::c_void, 
    _size: usize
) -> bool {
    if buf.is_null() {
        return false;
    }
    // Check if the memory block address is naturally aligned to a 4-byte boundary for the DMA bus engine.
    // (buf as usize) & 3 == 0 returns true if aligned, which matches exactly what the driver needs.
    ((buf as usize) & 3) == 0
}


/// Initializes and mounts the onboard Goouuu MicroSD card by consuming the necessary pins.
pub fn init_sd_card<'a>(clk: Gpio39<'a>, cmd: Gpio38<'a>, d0: Gpio40<'a>) 
-> anyhow::Result<(Gpio39<'a>, Gpio38<'a>, Gpio40<'a>)> {
    unsafe {
        // 1. Fetch the default slot architecture straight from the underlying C framework
        // This automatically assigns ALL complex function pointers (including get_dma_info, 
        // io_int, and init tracking) so we never risk a 0x0 null pointer panic.
        let mut host = esp_sys::sdmmc_host_t::default(); 
        
        // Force track constraints specifically to your 1-bit hardware layout requirements
        host.flags = 1; // Maps strictly to SDMMC_HOST_FLAG_1BIT
        host.slot = esp_sys::SDMMC_HOST_SLOT_1 as i32;     
        host.max_freq_khz = 20000; // 20 MHz safe operational clock frequency
        host.io_voltage = 3.3;

        // Pass your custom validation block on top of the initialized structure safely
        host.check_buffer_alignment = Some(rust_sdmmc_check_buffer_alignment); 

        // 2. Safely initialize the slot configuration profile using zeroing tracking.
        let mut slot_config: esp_sys::sdmmc_slot_config_t = std::mem::zeroed();
        
        // Explicitly map pins casting them safely to i32 structure expectations
        slot_config.clk = clk.pin() as i32;
        slot_config.cmd = cmd.pin() as i32;
        slot_config.d0  = d0.pin() as i32;
        
        slot_config.d1  = -1; 
        slot_config.d2  = -1; 
        slot_config.d3  = -1; 
        
        // Handle anonymous internal bindgen structural paths cleanly
        slot_config.__bindgen_anon_1.cd = -1; 
        slot_config.__bindgen_anon_2.wp = -1; 
        
        slot_config.width = 1; // 1-bit mode data lane tracker
        slot_config.flags = 1 << 0; // Enables the internal pullups flag bit (SDMMC_SLOT_FLAG_INTERNAL_PULLUP)

        // 3. Configure the Virtual File System (VFS) FAT mount parameters
        let mount_config = esp_sys::esp_vfs_fat_sdmmc_mount_config_t {
            format_if_mount_failed: false, 
            max_files: 4,                  
            allocation_unit_size: 0,
            disk_status_check_enable: false,
            use_one_fat: false, 
        };

        // 4. Mount the storage card to the virtual path tree
        let base_path = CString::new("/sdcard")?;
        let mut card_handle: *mut esp_sys::sdmmc_card_t = std::ptr::null_mut();

        let ret = esp_sys::esp_vfs_fat_sdmmc_mount(
            base_path.as_ptr(),
            &host,
            &slot_config as *const _ as *const std::ffi::c_void, 
            &mount_config,
            &mut card_handle,
        );

        if ret != esp_sys::ESP_OK {
            return Err(anyhow::anyhow!(
                "SDMMC mount transaction failed! ESP-IDF Error Code: {}", 
                ret
            ));
        }

        log::info!("Success! MicroSD card cleanly mounted at `/sdcard` namespace path.");
    }
    
    // Return ownership cleanly back to main context loop
    Ok((clk, cmd, d0))
}

/// Safely unmounts the file system and unlinks the host driver, freeing up GPIO 38.
pub fn deinit_sd_card() -> anyhow::Result<()> {
    unsafe {
        log::info!("De-initializing storage drivers to release shared trace copper lines...");
        let base_path = CString::new("/sdcard")?;
        
        let ret = esp_sys::esp_vfs_fat_sdcard_unmount(base_path.as_ptr(), std::ptr::null_mut());
        if ret != esp_sys::ESP_OK {
            return Err(anyhow::anyhow!("Failed to unmount SD VFS partition wrapper cleanly. Code: {}", ret));
        }
        
        log::info!("SDMMC driver completely purged. Shared pins are now safe to repurpose.");
    }
    Ok(())
}
