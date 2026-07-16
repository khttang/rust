use std::ffi::CString;
use esp_idf_sys as esp_sys;
use esp_idf_svc::hal::gpio::{Gpio38, Gpio39, Gpio40};
use esp_idf_svc::hal::gpio::Pin; // Activates clk.pin() trait methods
use anyhow::anyhow;
use log::info;

/// Initializes and mounts the onboard Goouuu MicroSD card by consuming the necessary pins.
pub fn init_sd_card<'a>(
    clk: Gpio39<'a>, 
    cmd: Gpio38<'a>, 
    d0: Gpio40<'a>
) -> anyhow::Result<(Gpio39<'a>, Gpio38<'a>, Gpio40<'a>)> {
    
    info!("Era 1 Bootstrap: Initializing 1-bit SDMMC host partition via CMake component shim...");

    unsafe {
        // 1. Fetch the pre-hydrated template structure straight from your isolated component namespace!
        // This natively copies ALL hidden internal callbacks, stopping the 0x0 crash for good.
        let mut host = esp_sys::sd_shim::get_c_sdmmc_host_default();
        
        // Force the configuration constraints for your specific 1-bit layout
        host.flags = 1; // SDMMC_HOST_FLAG_1BIT
        host.slot = esp_sys::SDMMC_HOST_SLOT_1 as i32;     
        host.max_freq_khz = 20000; // Safe 20 MHz mode to prevent line crosstalk
        host.io_voltage = 3.3;

        // 2. Clear and construct the Slot Properties
        let mut slot_config: esp_sys::sdmmc_slot_config_t = std::mem::zeroed();
        slot_config.clk = clk.pin() as i32;
        slot_config.cmd = cmd.pin() as i32;
        slot_config.d0  = d0.pin() as i32;
        slot_config.d1  = -1; 
        slot_config.d2  = -1; 
        slot_config.d3  = -1; 
        
        // Route through bindgen's anonymous union fields cleanly
        slot_config.__bindgen_anon_1.cd = -1; 
        slot_config.__bindgen_anon_2.wp = -1; 
        slot_config.width = 1; 
        
        // Keep pullups disabled (0) to eliminate high-speed 80MHz Octal PSRAM data bleeding
        slot_config.flags = 0; 

        // 3. Configure the Virtual File System (VFS) FAT mount parameters
        let mount_config = esp_sys::esp_vfs_fat_sdmmc_mount_config_t {
            format_if_mount_failed: false, 
            max_files: 4,                  
            allocation_unit_size: 0,
            disk_status_check_enable: false,
            use_one_fat: false, 
        };

        // 4. Secure Mount Transaction Execution
        let base_path = CString::new("/sdcard")?;
        let mut card_handle: *mut esp_sys::sdmmc_card_t = std::ptr::null_mut();

        // FIX (E0308): Transmute the sd_shim namespaced reference to the global raw pointer type expected by the VFS
        let global_host_ptr: *const esp_sys::sdmmc_host_t = std::mem::transmute(&host);

        // Direct reference works flawlessly now because types align perfectly under the global namespace
        let ret = esp_sys::esp_vfs_fat_sdmmc_mount(
            base_path.as_ptr(),
            global_host_ptr, 
            &slot_config as *const _ as *const std::ffi::c_void, 
            &mount_config,
            &mut card_handle,
        );

        if ret != esp_sys::ESP_OK {
            return Err(anyhow!("SDMMC mount transaction failed! ESP-IDF Error Code: {}", ret));
        }

        info!("Success! MicroSD card cleanly mounted at `/sdcard` namespace path.");
    }
    
    Ok((clk, cmd, d0))
}

/// Safely unmounts the file system and unlinks the host driver, freeing up GPIO 38.
pub fn deinit_sd_card() -> anyhow::Result<()> {
    unsafe {
        info!("De-initializing storage drivers to release shared trace copper lines...");
        
        let ret = esp_sys::esp_vfs_fat_sdmmc_unmount();
        if ret != esp_sys::ESP_OK {
            return Err(anyhow!("Failed to unmount SD VFS partition wrapper cleanly. Code: {}", ret));
        }
        
        info!("SDMMC driver completely purged. Shared pins are now safe to repurpose.");
    }
    Ok(())
}
