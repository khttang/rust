use edge_executor::LocalExecutor; 
use esp_idf_svc::hal::peripherals::Peripherals; 
use esp_idf_svc::http::server::{Configuration, EspHttpServer}; 
use esp_idf_svc::sys as esp_sys; 
use esp_idf_sys::camera as esp_camera; 
//use esp_idf_svc::log::EspLogger; 
use esp_idf_svc::eventloop::EspSystemEventLoop; 
use esp_idf_svc::nvs::EspDefaultNvsPartition; 
use esp_idf_svc::wifi::{AsyncWifi, AuthMethod, ClientConfiguration, Configuration as WifiConfiguration, EspWifi}; 
use esp_idf_svc::timer::EspTaskTimerService; 
use futures::executor::block_on; 
use log::{error, info, warn}; 
use std::time::Duration; 
//use std::ffi::c_char; 
use std::thread; 

const WIFI_SSID: &str = "SpectrumSetup-AC"; 
const WIFI_PASS: &str = "T@ngn3t2025"; 

// --- CAMERA PIN MAP FOR ESP32-S3 --- 
const PIN_D0: i32 = 11; 
const PIN_D1: i32 = 9; 
const PIN_D2: i32 = 8; 
const PIN_D3: i32 = 10; 
const PIN_D4: i32 = 12; 
const PIN_D5: i32 = 14; 
const PIN_D6: i32 = 17; 
const PIN_D7: i32 = 16; 
const PIN_XCLK: i32 = 15; 
const PIN_PCLK: i32 = 13; 
const PIN_VSYNC: i32 = 42; 
const PIN_HREF: i32 = 18; 
const PIN_SDA: i32 = 4; 
const PIN_SCL: i32 = 5; 
const PIN_RESET: i32 = -1; 
const PIN_PWDN: i32 = -1; 

// A wrapper to safely send raw pointer framebuffers across threads
struct SendPtr(*mut esp_camera::camera_fb_t);
unsafe impl Send for SendPtr {}

struct SendSender(async_channel::Sender<SendPtr>);
unsafe impl Send for SendSender {}

fn main() -> anyhow::Result<()> { 
    esp_idf_svc::log::EspLogger::initialize_default(); 
    info!("Initializing ESP32-S3 async Wi-Fi system..."); 

    let peripherals = Peripherals::take()?; 
    let sys_loop = EspSystemEventLoop::take()?; 
    let nvs = EspDefaultNvsPartition::take()?; 
    let timer_service = EspTaskTimerService::new()?; 

    info!("Initializing camera subsystem..."); 
    //init_camera()?; 

    // --- DECOUPLING: ASYNC CHANNEL BOUNDED TO 1 FRAME ---
    // If the browser drops frames, the background thread drops frames automatically
    // instead of accumulating latency or consuming PSRAM.
    let (tx, rx) = async_channel::bounded::<SendPtr>(1);

    // --- TASK A: THE BACKGROUND PRODUCER THREAD ---
    // Moves the camera frame collection loop completely clear of the Wi-Fi stack
    unsafe {
        let boxed_sender = Box::new(SendSender(tx));
        let param_ptr = Box::into_raw(boxed_sender) as *mut core::ffi::c_void;
        let task_name = std::ffi::CString::new("native_cam_task").unwrap();
        let mut task_handle: esp_idf_sys::TaskHandle_t = std::ptr::null_mut();

        let result = esp_idf_sys::xTaskCreatePinnedToCore(
            Some(native_camera_producer_task), // Task implementation function
            task_name.as_ptr(),               // Diagnostic name string
            8192,                              // Allocate plenty of stack memory for Rust vectors
            param_ptr,                         // Pass the raw boxed async channel pointer
            24,                                // PRIORITY: Higher than Wi-Fi driver task (23)
            &mut task_handle,                  // Task handle pointer allocation
            1,                                 // CORE ID: Bind to Core 1 (Wi-Fi operates on Core 0)
        );

        if result != 1 { // FreeRTOS pdPASS = 1
            panic!("Fatal: Failed to spawn high-priority camera worker task!");
        }
    }

    let mut wifi = AsyncWifi::wrap( 
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?, 
        sys_loop.clone(), 
        timer_service 
    )?; 

    let executor: LocalExecutor = edge_executor::LocalExecutor::new(); 

    block_on(executor.run(Box::pin(async { 
        if let Err(e) = connect_wifi(&mut wifi).await { 
            warn!("Failed to establish network connection: {:?}", e); 
        } else { 
            info!("Wi-Fi cycle successfully completed!"); 
        } 
    }))); 

    let mut server = EspHttpServer::new(&Configuration::default())?; 
    info!("HTTP Server running on port 80. Path: /stream"); 

    // --- TASK B: THE ROUTER CONSUMER TASK ---
    server.fn_handler("/stream", esp_idf_svc::http::Method::Get, move |request| { 
        let mut response = request.into_response( 
            200, 
            None, 
            &[ 
                ("Content-Type", "multipart/x-mixed-replace; boundary=123456789000000000000987654321"), 
                ("Access-Control-Allow-Origin", "*"), 
            ], 
        ).map_err(|e| e.0)?; 
        info!("Laptop client connected to video stream."); 

        loop { 
            // Block on the channel receiver asynchronously until a fresh frame arrives.
            // This implicitly yields the thread back to the edge_executor, preventing bottleneck drops!
            if let Ok(SendPtr(fb)) = block_on(rx.recv()) {
                unsafe {
                    // 1. Map raw pointer slice fields cleanly into local scope 
                    let frame_bytes = std::slice::from_raw_parts((*fb).buf, (*fb).len); 

                    // 2. IMMEDIATE DATA CLEANSING: Extract the image bytes into a native heap Vector.
                    // This creates an independent allocation, eliminating pointer lifetime violations!
                    let biometric_target_frame: Vec<u8> = frame_bytes.to_vec();

                    // 3. OFFLOAD IMMEDIATE TRANSFORMATION TASK
                    // Pass the cleansed vector block straight into your facial detector algorithm.
                    // If it errors, we log it but keep the laptop video stream running smoothly!
                    if let Err(e) = cleanse_and_detect_face(&biometric_target_frame) {
                        error!("Biometric transformation pass failed: {:?}", e);
                    }

                    // 4. Build standard HTTP boundary containers
                    let part_header = format!( 
                        "\r\n--123456789000000000000987654321\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", 
                        frame_bytes.len() 
                    ); 

                    // 5. Pipe frame out to laptop browser
                    if response.write(part_header.as_bytes()).map_err(|e| e.0).is_err() 
                        || response.write(frame_bytes).map_err(|e| e.0).is_err() 
                        || response.write(b"\r\n").map_err(|e| e.0).is_err() 
                    { 
                        esp_camera::esp_camera_fb_return(fb); 
                        warn!("Stream connection dropped by host browser."); 
                        break; 
                    } 

                    esp_camera::esp_camera_fb_return(fb); 
                } 
            } else {
                break; // Channel closed
            }
        } 
        Ok::<(), esp_idf_svc::sys::EspError>(())
    })?; 

    loop { 
        thread::sleep(Duration::from_secs(1)); 
    } 
} 

async fn connect_wifi(wifi: &mut AsyncWifi<EspWifi<'static>>) -> anyhow::Result<()> { 
    info!("Setting up Wi-Fi configurations..."); 
    let auth_modes = [ 
        (AuthMethod::WPA2WPA3Personal, "WPA2/WPA3 Mixed (With PMF)"), 
        (AuthMethod::WPA2Personal, "WPA2 Personal (Standard)"), 
    ]; 
    for (method, description) in auth_modes { 
        info!("Attempting connection using mode: {}", description); 
        let wifi_configuration = WifiConfiguration::Client(ClientConfiguration { 
            ssid: WIFI_SSID.try_into().unwrap(), 
            password: WIFI_PASS.try_into().unwrap(), 
            auth_method: method, 
            ..Default::default() 
        }); 

        wifi.set_configuration(&wifi_configuration)?; 
        if !wifi.is_started()? { 
            wifi.start().await?; 
        } 
        info!("Connecting to access point..."); 
        if let Err(e) = wifi.connect().await { 
            warn!("Physical handshake failed with mode {}: {:?}", description, e); 
            let _ = wifi.disconnect().await; 
            std::thread::sleep(std::time::Duration::from_secs(2)); 
            continue; 
        } 
        info!("Physical link established! Waiting for DHCP address assignment..."); 
        if let Err(e) = wifi.wait_netif_up().await { 
            warn!("DHCP lease failed using mode {}: {:?}", description, e); 
            let _ = wifi.disconnect().await; 
            std::thread::sleep(std::time::Duration::from_secs(2)); 
            continue; 
        } 
        let netif = wifi.wifi().sta_netif(); 
        let ip_info = netif.get_ip_info()?; 
        info!("Successfully connected! IP Details: {:?}", ip_info); 
        return Ok(()); 
    } 
    Err(anyhow::anyhow!("Could not connect using any supported authentication standard")) 
} 

fn init_camera() -> anyhow::Result<()> { 
    unsafe { 
        let config = esp_camera::camera_config_t { 
            pin_pwdn: PIN_PWDN, 
            pin_reset: PIN_RESET, 
            pin_xclk: PIN_XCLK, 
            // Fix: Map your I2C pins into the modern C-Union anonymous struct mappings
            __bindgen_anon_1: esp_camera::camera_config_t__bindgen_ty_1 {
                pin_sccb_sda: PIN_SDA,
            },
            __bindgen_anon_2: esp_camera::camera_config_t__bindgen_ty_2 {
                pin_sccb_scl: PIN_SCL,
            },
            pin_d7: PIN_D7, 
            pin_d6: PIN_D6, 
            pin_d5: PIN_D5, 
            pin_d4: PIN_D4, 
            pin_d3: PIN_D3, 
            pin_d2: PIN_D2, 
            pin_d1: PIN_D1, 
            pin_d0: PIN_D0, 
            pin_vsync: PIN_VSYNC, 
            pin_href: PIN_HREF, 
            pin_pclk: PIN_PCLK, 
            xclk_freq_hz: 10_000_000, // Safe 10 MHz profile optimal for OV3660 stability
            ledc_timer: esp_camera::ledc_timer_t_LEDC_TIMER_0, 
            ledc_channel: esp_camera::ledc_channel_t_LEDC_CHANNEL_0, 
            pixel_format: esp_camera::pixformat_t_PIXFORMAT_JPEG, 
            frame_size: esp_camera::framesize_t_FRAMESIZE_SVGA, 
            jpeg_quality: 10,         // Higher number = smaller frame payloads = happy DMA bus 
            fb_count: 2,              // Increase to 3 to provide DMA node breathing room
            fb_location: esp_camera::camera_fb_location_t_CAMERA_FB_IN_PSRAM, 

            // --- CHANGE THIS FROM CAMERA_GRAB_WHEN_EMPTY TO CAMERA_GRAB_LATEST ---
            grab_mode: esp_camera::camera_grab_mode_t_CAMERA_GRAB_LATEST,
            sccb_i2c_port: 0,
            ..Default::default() 
        }; 

        let err = esp_camera::esp_camera_init(&config); 
        if err != esp_sys::ESP_OK { 
            return Err(anyhow::anyhow!("Failed to initialize camera device OV3660, error code: {}", err)); 
        }

        // This directly alters the S3's internal LCD_CAM hardware configuration register 1
        // to add a 1-cycle sampling delay on the physical PCLK line input trace
        let lcd_cam_reg = 0x60040000 as *mut u32; 
        if !lcd_cam_reg.is_null() {
            let current_val = std::ptr::read_volatile(lcd_cam_reg);
            std::ptr::write_volatile(lcd_cam_reg, current_val | (1 << 26)); // Enable PCLK rx delay gate
        }

        let sensor = esp_camera::esp_camera_sensor_get();
        if !sensor.is_null() {
            ((*sensor).set_vflip.unwrap())(sensor, 1);
            ((*sensor).set_hmirror.unwrap())(sensor, 1);
        }

        info!("OV3660 camera sensor calibrated and running inside its own module namespace!"); 
    } 
    Ok(()) 
}

extern "C" fn native_camera_producer_task(params: *mut core::ffi::c_void) {
    info!("Real-time high-priority FreeRTOS camera capture pump active on Core 1.");
    
    let tx_wrapper = unsafe { Box::from_raw(params as *mut SendSender) };
    let tx = tx_wrapper.0;

    info!("Initializing camera subsystem hardware directly on Core 1...");
    if let Err(e) = init_camera() {
        error!("Fatal Error: Camera hardware initialization failed on Core 1: {:?}", e);
        unsafe { esp_idf_sys::vTaskDelete(std::ptr::null_mut()); }
        return;
    }
    info!("Camera ISR and hardware pipeline cleanly mapped to Core 1.");

    // --- FORCE HARDWARE WAKE & START STREAMING ---
    unsafe {
        let sensor = esp_camera::esp_camera_sensor_get();
        if !sensor.is_null() {
            // Force the OV3660 internal power-down register to 0 (Fully Awake Operational Mode)
            if let Some(set_reg) = (*sensor).set_reg {
                // Register 0x3008 handles sleep modes on the OV3660 array matrix
                // Writing 0x02 or 0x00 wakes up the digital core lines forcefully
                set_reg(sensor, 0x3008, 0xFF, 0x02); 
            }
            
            // Re-assert the target framesize directly inside the sensor control registers
            if let Some(set_framesize) = (*sensor).set_framesize {
                set_framesize(sensor, esp_camera::framesize_t_FRAMESIZE_SVGA);
            }
            
            info!("OV3660 Digital Engine awakened. Parallel lines active.");
        }
    }
    info!("Camera ISR and hardware pipeline cleanly mapped to Core 1.");

    loop {
        unsafe {
            let fb = esp_camera::esp_camera_fb_get();
            
            if !fb.is_null() {
                if (*fb).len > 0 {
                    if let Err(_) = tx.try_send(SendPtr(fb)) {
                        esp_camera::esp_camera_fb_return(fb);
                        esp_idf_sys::vTaskDelay(1);
                    }
                } else {
                    esp_camera::esp_camera_fb_return(fb);
                    esp_idf_sys::vTaskDelay(1);
                }
            } else {
                // If a frame drops or times out, pulse the wake register to ensure it stays online
                let sensor = esp_camera::esp_camera_sensor_get();
                if !sensor.is_null() {
                    if let Some(set_reg) = (*sensor).set_reg {
                        set_reg(sensor, 0x3008, 0xFF, 0x02);
                    }
                }
                esp_idf_sys::vTaskDelay(5);
            }
        }
    }
}

/// Independent biometric transformation worker function.
/// Takes a cleanly copied JPEG vector array from the active camera queue.
fn cleanse_and_detect_face(jpeg_data: &[u8]) -> anyhow::Result<()> {
    // For biometrics validation, we enforce basic size sanity filters
    if jpeg_data.len() < 1024 {
        return Err(anyhow::anyhow!("Frame payload too small, corrupted sensor block."));
    }

    // --- NEXT ADVANCED PROCESSING PHASE SKELETON ---
    // 1. Unpack JPEG data into structural RGB/Grayscale image matrices
    // 2. Normalize histograms to adjust face illumination variances 
    // 3. Feed the array to your facial landmark detection pipeline
    
    // Log verification check
    // info!("Cleansed biometric packet received. Size: {} bytes", jpeg_data.len());
    
    Ok(())
}
