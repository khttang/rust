mod sd_card;
mod camera;
mod wifi;
mod audio;
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
use sd_card::{init_sd_card, deinit_sd_card};
use anyhow::anyhow;

use crate::camera::{SendPtr, native_camera_producer_task};
use crate::audio::{native_audio_mic_pump_task, native_audio_spk_pump_task};
use crate::wifi::connect_wifi;
use crate::heart_beat::spawn_basic_heartbeat;

fn main() -> anyhow::Result<()> { 
    esp_idf_svc::log::EspLogger::initialize_default(); 
    info!("Initializing ESP32-S3 async Wi-Fi system..."); 

    let peripherals = Peripherals::take().map_err(|_| {
        anyhow!("Peripherals allocation failed — Core resources already consumed!")
    })?; 
    let sys_loop = EspSystemEventLoop::take()?; 
    let nvs = EspDefaultNvsPartition::take()?; 
    let timer_service = EspTaskTimerService::new()?; 
    let mut net_pwd: String = String::new();

    // Read Knowledge Base from SD Card
    let freed_gpio38 = {
        // Isolate pins needed for the 1-bit SDMMC execution block
        let clk = peripherals.pins.gpio39;
        let cmd = peripherals.pins.gpio38;
        let d0  = peripherals.pins.gpio40;

        info!("Mounting SD card to load face vectors into RAM...");
        // during this time, must ensure that RGB LED is not exercised

        let (_returned_clk, returned_cmd, _returned_d0) = init_sd_card(clk, cmd, d0)?; 
        
        let data = read_embeddings_from_file()?;
        if let Ok(pwd) = read_password_from_file() {
            net_pwd = pwd;
        }
        
        // Critical Step: Cleanly unmount and release the SDMMC driver 
        // to return GPIO 38/40 back to the unallocated hardware pool.
        deinit_sd_card()?; 
        info!("SD Card unmounted. Pins released.");
        
        // Return the cmd pin, which is now available for use by the LED driver, 
        // along with loaded data vectors and network password out of the temporary block
        returned_cmd 
    };

    // Now that the SD card is safely turned off, same physical pin GPIO 38/40 becomes available 
    // to drive the LED status indicator without any conflicts.
    let heartbeat_pin = AnyOutputPin::from(freed_gpio38);

    // Launch the indicator task thread to blink the LED at a rate of 1 Hz
    spawn_basic_heartbeat(heartbeat_pin)?;

    info!("Initializing camera subsystem..."); 

    // --- DECOUPLING: ASYNC CHANNEL BOUNDED TO 1 FRAME ---
    // If the browser drops frames, the background thread drops frames automatically
    // instead of accumulating latency or consuming PSRAM.
    let (video_tx, video_rx) = async_channel::bounded::<SendPtr>(3);

    // --- TASK A: THE BACKGROUND PRODUCER THREAD ---
    // Moves the camera frame collection loop completely clear of the Wi-Fi stack
    unsafe {
        let param_ptr = Box::into_raw(Box::new(video_tx));
        let task_name = b"native_cam_task\0";
        let mut task_handle: esp_idf_sys::TaskHandle_t = std::ptr::null_mut();

        let result = esp_idf_sys::xTaskCreatePinnedToCore(
            Some(native_camera_producer_task),    // Task implementation function
            task_name.as_ptr() as *const core::ffi::c_char,  // Diagnostic name string
            16384,                                // Bounded upward to 16KB to insulate Rust runtime allocations
            param_ptr as *mut core::ffi::c_void,  // Pass the raw boxed async channel pointer
            24,                                   // PRIORITY: Higher than Wi-Fi driver task (23)
            &mut task_handle,                     // Task handle pointer allocation
            1,                                    // CORE ID: Bind to Core 1 (Wi-Fi operates on Core 0)
        );

        if result != 1 { // FreeRTOS pdPASS = 1
            panic!("Fatal: Failed to spawn high-priority camera worker task!");
        }
    }

    // --- AUDIO SUBSYSTEM INITIALIZATION ---
    let audio_system = audio::init_audio_subsystem().expect("Fatal: Failed to initialize hardware I2S audio driver!");

    // Allocate async channels for passing voice buffers across threads
    let (audio_tx, audio_rx) = async_channel::bounded::<Vec<i16>>(10); // Standard 16-bit PCM voice frames

    // --- TASK B: THE BACKGROUND MICROPHONE PRODUCER TASK ---
    unsafe {
        let rx_handle_raw = audio_system.rx_handle as *mut core::ffi::c_void;
        let boxed_audio_tx = Box::new(audio_tx);
        let param_ptr = Box::into_raw(Box::new((rx_handle_raw, boxed_audio_tx)));
        
        let task_name = b"native_mic_task\0";
        let mut task_handle: esp_idf_sys::TaskHandle_t = std::ptr::null_mut();
        
        let result = esp_idf_sys::xTaskCreatePinnedToCore(
            Some(native_audio_mic_pump_task),
            task_name.as_ptr() as *const core::ffi::c_char,
            8192, // 8KB stack space for audio frame buffering
            param_ptr as *mut core::ffi::c_void,
            24,   // PRIORITY: Matching high-priority camera loop for synchronized capture
            &mut task_handle,
            1,    // CORE ID: Pinned to Core 1 to bypass Wi-Fi interrupts
        );
        
        if result != 1 {
            panic!("Fatal: Failed to spawn high-priority audio microphone task!");
        }
    }

    // --- TASK C: THE BACKGROUND SPEAKER PLAYBACK CONSUMER TASK ---
    unsafe {
        let tx_handle_raw = audio_system.tx_handle;
        let task_name = b"native_spk_task\0";
        let mut task_handle: esp_idf_sys::TaskHandle_t = std::ptr::null_mut();
        
        let result = esp_idf_sys::xTaskCreatePinnedToCore(
            Some(native_audio_spk_pump_task),
            task_name.as_ptr() as *const core::ffi::c_char,
            4096, 
            tx_handle_raw as *mut core::ffi::c_void,
            22,   // PRIORITY: Slightly lower than Wi-Fi (23) so the network engine can feed it safely
            &mut task_handle,
            0,    // CORE ID: Bind to Core 0 next to the network socket processors
        );
        
        if result != 1 {
            panic!("Fatal: Failed to spawn speaker playback worker task!");
        }
    }

    let mut wifi = AsyncWifi::wrap( 
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?, 
        sys_loop.clone(), 
        timer_service 
    )?; 

    let executor: LocalExecutor = edge_executor::LocalExecutor::new(); 

    block_on(executor.run(Box::pin(async { 
        if let Err(e) = connect_wifi(&mut wifi, &net_pwd).await { 
            warn!("Failed to establish network connection: {:?}", e); 
        } else { 
            info!("Wi-Fi cycle successfully completed!"); 
        }

        unsafe {
            // Force Wi-Fi to run at full power with zero power-saving sleep cycles
            esp_idf_sys::esp_wifi_set_ps(0); // ESP_WIFI_PS_NONE = 0
        } 

        // Deploy the Async Biometric Voice Inference Task (Right before server)
        let audio_consumer_rx = audio_rx.clone();
        executor.spawn(async move {
            info!("Asynchronous voice feature matrix analyzer loop deployed.");
            while let Ok(pcm_samples) = audio_consumer_rx.recv().await {
                let sample_count = pcm_samples.len();
                if sample_count > 0 {
                    let sum_squares: f64 = pcm_samples.iter()
                        .map(|&s| { let val = s as f64; val * val })
                        .sum();
                    let rms_energy = (sum_squares / sample_count as f64).sqrt();
                    
                    if rms_energy > 1500.0 { 
                        info!("Acoustic wake pattern detected! RMS Energy: {:.2}", rms_energy);
                        // Place your voice embedding/KWS inference entry point here
                    }
                }
            }
        }).detach();

        executor.spawn(async move {
            info!("Asynchronous independent facial recognition loop deployed.");
            
            loop {
                unsafe {
                    // Pull a snapshot directly from the camera driver block
                    let fb = esp_camera::esp_camera_fb_get();
                    if !fb.is_null() {
                        if (*fb).len > 0 {
                            let frame_bytes = std::slice::from_raw_parts((*fb).buf, (*fb).len);
                            
                            // Create an isolated heap copy on Core 0 for facial recognition math
                            let biometric_frame = frame_bytes.to_vec();
                            
                            if let Err(e) = cleanse_and_detect_face(&biometric_frame) {
                                error!("Biometric analysis frame error: {:?}", e);
                            }
                        }
                        // Return the frame pointer immediately to keep the hardware pipeline clear
                        esp_camera::esp_camera_fb_return(fb);
                    }
                    
                    // Pacing Interval: Only run facial recognition check once every 200ms
                    // This stops your 80MHz PSRAM bus from overloading!
                    esp_idf_sys::vTaskDelay(20); 
                }
            }
        }).detach();

        let mut server = EspHttpServer::new(&Configuration::default())?; 
        info!("HTTP Server running on port 80. Path: /stream"); 

        // THE ROUTER CONSUMER TASK
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
                if let Ok(SendPtr(fb)) = block_on(video_rx.recv()) {
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

        // Prevent the async block from returning instantly.
        // This keeps the server running and processing on Core 0!
        loop {
            //let _: () = futures_util::future::pending().await;
            futures_util::pending!(); // Yields execution gracefully to the other spawned tasks
        }
    })))
}  

/// Independent biometric transformation worker function.
/// Takes a cleanly copied JPEG vector array from the active camera queue.
fn cleanse_and_detect_face(jpeg_data: &[u8]) -> anyhow::Result<()> {
    // 1. In a production pipeline (e.g., using tfmicro or esp-who), 
    // you would decode the JPEG payload into a raw RGB888 pixel array.
    if jpeg_data.is_empty() {
        return Err(anyhow!("Received completely empty frame buffer node"));
    }

    // 2. Mocking the signature verification processing logic:
    // scan_matrix_landmarks(jpeg_data); 
    // extract_128d_embedding_vector();

    // 3. For validation, let's verify the JPEG integrity markers
    if jpeg_data.len() > 4 && jpeg_data[0] == 0xFF && jpeg_data[1] == 0xD8 {
        // Valid JPEG head structure found!
        // Compare with the static '_data' array loaded from your SD Card partition
        return Ok(());
    }

    Err(anyhow!("Frame payload corrupted or missing standard SOI markers"))
}


use std::fs::File;
use std::io::Read;

/// Reads the raw text string profile data from your card using standard Rust IO.
fn read_embeddings_from_file() -> anyhow::Result<String> {
    info!("Opening database file from filesystem store...");
    
    // Target your file directly inside the mounted directory namespace tree
    let mut file = File::open("/sdcard/EMBEDS.JSN")
        .map_err(|e| anyhow!("Could not find EMBEDS.JSN on the root of your SD card: {:?}", e))?;
        
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    
    Ok(contents)
}

fn read_password_from_file() -> anyhow::Result<String> {
    info!("Reading network password from filesystem store...");

    let mut file = File::open("/sdcard/NETWORK.PWD")
        .map_err(|e| anyhow!("Could not find NETWORK.PWD on the root of your SD card: {:?}", e))?;
        
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    
    Ok(contents)
}