mod sd_card;
mod camera;
mod wifi;
mod heart_beat;

use esp_idf_sys::camera as esp_camera; 
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::gpio::AnyOutputPin;
use esp_idf_svc::http::server::{Configuration, EspHttpServer}; 
use esp_idf_svc::eventloop::EspSystemEventLoop; 
use esp_idf_svc::nvs::EspDefaultNvsPartition; 
use esp_idf_svc::wifi::{AsyncWifi, EspWifi}; 
use esp_idf_svc::timer::EspTaskTimerService; 
use edge_executor::LocalExecutor; 
use futures::executor::block_on; 
use log::{error, info, warn}; 
use std::time::Duration; 
use std::thread; 
use sd_card::{init_sd_card, deinit_sd_card};

use crate::camera::{SendPtr, SendSender, native_camera_producer_task};
use crate::wifi::connect_wifi;
use crate::heart_beat::spawn_basic_heartbeat;

fn main() -> anyhow::Result<()> { 
    esp_idf_svc::log::EspLogger::initialize_default(); 
    info!("Initializing ESP32-S3 async Wi-Fi system..."); 

    let peripherals = Peripherals::take().map_err(|_| {
        anyhow::anyhow!("Peripherals allocation failed — Core resources already consumed!")
    })?; 
    let sys_loop = EspSystemEventLoop::take()?; 
    let nvs = EspDefaultNvsPartition::take()?; 
    let timer_service = EspTaskTimerService::new()?; 

    // Read Knowledge Base from SD Card
    let freed_gpio38 = {
        // Isolate pins needed for the 1-bit SDMMC execution block
        let clk = peripherals.pins.gpio39;
        let cmd = peripherals.pins.gpio38;
        let d0  = peripherals.pins.gpio40;

        log::info!("Mounting SD card to load face vectors into RAM...");
        // during this time, must ensure that RGB LED is not exercised

        let (_returned_clk, returned_cmd, _returned_d0) = init_sd_card(clk, cmd, d0)?; 
        
        let data = read_embeddings_from_file()?;
        
        // Critical Step: Cleanly unmount and release the SDMMC driver 
        // to return GPIO 38/40 back to the unallocated hardware pool.
        deinit_sd_card()?; 
        log::info!("SD Card unmounted. Pins released.");
        
        returned_cmd // Return the loaded data vectors out of the temporary block
    };

    // Now that the SD card is safely turned off, you can reuse the exact 
    // same physical pin to drive your status indicator without any conflicts.
    let heartbeat_pin = AnyOutputPin::from(freed_gpio38);
    // Launch the indicator task thread using the freed copper trace
    spawn_basic_heartbeat(heartbeat_pin)?;

    info!("Initializing camera subsystem..."); 

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

use std::fs::File;
use std::io::Read;

/// Reads the raw text string profile data from your card using standard Rust IO.
pub fn read_embeddings_from_file() -> anyhow::Result<String> {
    log::info!("Opening database file from filesystem store...");
    
    // Target your file directly inside the mounted directory namespace tree
    let mut file = File::open("/sdcard/EMBEDS.JSN")
        .map_err(|e| anyhow::anyhow!("Could not find EMBEDS.JSN on the root of your SD card: {:?}", e))?;
        
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    
    Ok(contents)
}
