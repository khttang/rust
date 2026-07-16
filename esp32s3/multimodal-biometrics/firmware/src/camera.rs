use esp_idf_svc::sys as esp_sys; 
use esp_idf_sys::camera as esp_camera;
use anyhow::{anyhow, Result};
use log::{info, error};

// --- CAMERA PIN MAP FOR ESP32-S3 --- 
const PIN_D0: i32 = 11; 
const PIN_D1: i32 = 9; 
const PIN_D2: i32 = 8; 
const PIN_D3: i32 = 10; 
const PIN_D4: i32 = 12; 
const PIN_D5: i32 = 18; 
const PIN_D6: i32 = 17; 
const PIN_D7: i32 = 16; 
const PIN_XCLK: i32 = 15; 
const PIN_PCLK: i32 = 13; 
const PIN_VSYNC: i32 = 6; 
const PIN_HREF: i32 = 7; 
const PIN_SDA: i32 = 4; 
const PIN_SCL: i32 = 5; 
const PIN_RESET: i32 = -1; 
const PIN_PWDN: i32 = -1; 

// A wrapper to safely send raw pointer framebuffers across threads
pub struct SendPtr(pub *mut esp_camera::camera_fb_t);
unsafe impl Send for SendPtr {}

pub fn init_camera() -> Result<()> { 
    unsafe {
        // 1. Explicitly zero out the C struct to safely handle all hidden bindgen padding/unions
        let mut config: esp_camera::camera_config_t = std::mem::zeroed();

        // 2. Assign control and clock lines
        config.pin_pwdn = PIN_PWDN;
        config.pin_reset = PIN_RESET;
        config.pin_xclk = PIN_XCLK;
        
        // 3. Map I2C pins into the modern C-Union anonymous struct mappings
        config.__bindgen_anon_1 = esp_camera::camera_config_t__bindgen_ty_1 {
            pin_sccb_sda: PIN_SDA,
        };
        config.__bindgen_anon_2 = esp_camera::camera_config_t__bindgen_ty_2 {
            pin_sccb_scl: PIN_SCL,
        };

        // 4. Assign the corrected parallel data bus and synchronization lines
        config.pin_d7 = PIN_D7;
        config.pin_d6 = PIN_D6;
        config.pin_d5 = PIN_D5;
        config.pin_d4 = PIN_D4;
        config.pin_d3 = PIN_D3;
        config.pin_d2 = PIN_D2;
        config.pin_d1 = PIN_D1;
        config.pin_d0 = PIN_D0;
        config.pin_vsync = PIN_VSYNC;
        config.pin_href = PIN_HREF;
        config.pin_pclk = PIN_PCLK;

        // 5. DMA Engine and Frequency Tunings
        config.xclk_freq_hz = 13_000_000; // Safe 13 MHz profile optimal for OV3660 stability
        config.jpeg_quality = 15;
        config.fb_count = 2;              // Increased to 3 to provide DMA node breathing room
        config.ledc_timer = esp_camera::ledc_timer_t_LEDC_TIMER_0;
        config.ledc_channel = esp_camera::ledc_channel_t_LEDC_CHANNEL_0;
        config.pixel_format = esp_camera::pixformat_t_PIXFORMAT_JPEG;
        config.frame_size = esp_camera::framesize_t_FRAMESIZE_SVGA;
        config.fb_location = esp_camera::camera_fb_location_t_CAMERA_FB_IN_PSRAM;
        config.grab_mode = esp_camera::camera_grab_mode_t_CAMERA_GRAB_LATEST;
        
        // Set to -1 to force the driver to use its internal software I2C engine, 
        // preventing conflicts with external esp-idf-hal I2C drivers on Port 0.
        config.sccb_i2c_port = -1; 

        // 6. Initialize the hardware driver
        let err = esp_camera::esp_camera_init(&config);
        if err != esp_sys::ESP_OK {
            return Err(anyhow!("Failed to initialize camera device OV3660, error code: {}", err));
        }

        // 7. Inject the 1-cycle sampling delay on the physical PCLK line input trace
        let lcd_cam_reg = 0x60040000 as *mut u32;
        if !lcd_cam_reg.is_null() {
            let current_val = std::ptr::read_volatile(lcd_cam_reg);
            std::ptr::write_volatile(lcd_cam_reg, current_val | (1 << 26)); // Enable PCLK rx delay gate
        }

        // 8. Configure image orientation
        let sensor = esp_camera::esp_camera_sensor_get();
        if !sensor.is_null() {
            ((*sensor).set_vflip.unwrap())(sensor, 0);
            ((*sensor).set_hmirror.unwrap())(sensor, 1);
        }
        
        info!("OV3660 camera sensor calibrated and running inside its own module namespace!");
    }
    Ok(())
}

pub unsafe extern "C" fn native_camera_producer_task(params: *mut core::ffi::c_void) {
    let tx_wrapper = unsafe { Box::from_raw(params as *mut async_channel::Sender<SendPtr>) };
    let video_tx = *tx_wrapper;
    
    if let Err(e) = init_camera() {
        error!("Fatal Error: Camera hardware initialization failed on Core 1: {:?}", e);
        unsafe { esp_idf_sys::vTaskDelete(std::ptr::null_mut()); }
        return;
    }
    
    let mut frame_counter: u32 = 0;

    loop {
        unsafe {
            let fb = esp_camera::esp_camera_fb_get();
            if !fb.is_null() {
                frame_counter += 1;

                // Frame skip: pass every 4th frame out to the video streaming channel
                if frame_counter % 4 == 0 && (*fb).len > 0 {
                    if video_tx.is_full() {
                        // Discard the frame immediately. 
                        // Because fb_count=2, the hardware alternates slots safely!
                        esp_camera::esp_camera_fb_return(fb);
                    } else if let Err(_) = video_tx.try_send(SendPtr(fb)) {
                        // If the channel is full, clear the frame immediately
                        esp_camera::esp_camera_fb_return(fb);                   
                    }
                } else {
                    esp_camera::esp_camera_fb_return(fb);
                }
                esp_idf_sys::vTaskDelay(2);
            } else {
                esp_idf_sys::vTaskDelay(5);
            }
        }
    }
}
