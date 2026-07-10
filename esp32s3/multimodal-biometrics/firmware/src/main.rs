// 1. Standard ESP-IDF System Drivers
use esp_idf_svc::sys as esp_sys; 
use esp_idf_sys::camera as esp_camera; 

use edge_executor::LocalExecutor; 
use esp_idf_svc::hal::peripherals::Peripherals; 
use esp_idf_svc::http::server::{Configuration, EspHttpServer}; 
use esp_idf_svc::eventloop::EspSystemEventLoop; 
use esp_idf_svc::nvs::EspDefaultNvsPartition; 
use esp_idf_svc::wifi::{AsyncWifi, AuthMethod, ClientConfiguration, Configuration as WifiConfiguration, EspWifi}; 
use esp_idf_svc::timer::EspTaskTimerService;
use futures::executor::block_on; 
use log::{error, info, warn}; 
use std::time::Duration; 
use std::thread; 

const WIFI_SSID: &str = "SpectrumSetup-AC"; 
const WIFI_PASS: &str = "T@ngn3t2025"; 

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
const PIN_RESET: i32 = -1; 
const PIN_PWDN: i32 = -1; 

fn main() -> anyhow::Result<()> { 
    // 1. Initialize the system logging framework 
    esp_idf_svc::log::EspLogger::initialize_default(); 
    info!("Initializing ESP32-S3 async Wi-Fi system..."); 

    // 2. Take ownership of vital system peripherals 
    let peripherals = Peripherals::take()?; 
    let sys_loop = EspSystemEventLoop::take()?; 
    let nvs = EspDefaultNvsPartition::take()?; 
    let timer_service = EspTaskTimerService::new()?; 

    // 3. Initialize the C-camera subsystem via unsafe bindings 
    info!("Initializing camera subsystem..."); 
    init_camera()?; 

    // 4. Create the standard Wi-Fi controller driver instance 
    let mut wifi = AsyncWifi::wrap( 
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?, 
        sys_loop.clone(), 
        timer_service 
    )?; 

    // 5. Instantiate a local asynchronous executor thread 
    let executor: LocalExecutor = edge_executor::LocalExecutor::new(); 

    // 6. Spawn and block the main execution thread inside our async network task 
    block_on(executor.run(Box::pin(async { 
        if let Err(e) = connect_wifi(&mut wifi).await { 
            warn!("Failed to establish network connection: {:?}", e); 
        } else { 
            info!("Wi-Fi cycle successfully completed!"); 
        } 
    }))); 

    // 7. Instantiate an HTTP server on Port 80 
    let mut server = EspHttpServer::new(&Configuration::default())?; 
    info!("HTTP Server running on port 80. Path: /stream"); 

    // 8. Register the MJPEG stream handler endpoint 
    server.fn_handler("/stream", esp_idf_svc::http::Method::Get, |request| { 
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
            unsafe { 
                // Fix 1: Changed `esp_cam::` to your explicit import `esp_camera::`
                // Fix 2: Removed nested duplicate `unsafe {` block which broke scoping syntax
                let fb = esp_camera::esp_camera_fb_get(); 
                if fb.is_null() { 
                    error!("Failed to capture camera frame buffer."); 
                    thread::sleep(Duration::from_millis(100)); 
                    continue; 
                } 

                // Slice the raw native C byte buffer cleanly into Rust scope memory 
                let frame_bytes = std::slice::from_raw_parts((*fb).buf, (*fb).len); 

                // Build standard HTTP MJPEG container parts 
                let part_header = format!( 
                    "\r\n--123456789000000000000987654321\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", 
                    frame_bytes.len() 
                ); 

                // Write the image metadata header, raw frame contents, and frame spacing rules 
                if response.write(part_header.as_bytes()).is_err() || response.write(frame_bytes).is_err() || response.write(b"\r\n").is_err() { 
                    // Fix 3: Changed `esp_cam::` to `esp_camera::`
                    esp_camera::esp_camera_fb_return(fb); 
                    warn!("Stream connection dropped by host browser."); 
                    break; 
                } 

                // Fix 4: Changed `esp_sys::` to `esp_camera::` since return is a camera-module function
                esp_camera::esp_camera_fb_return(fb); 
            } 
            // Control framerate throttle payload balance (~15 FPS) 
            thread::sleep(Duration::from_millis(65)); 
        } 
        Ok::<(), esp_idf_svc::sys::EspError>(()) 
    })?; 

    // Keep main thread alive indefinitely alongside the active server thread context 
    loop { 
        thread::sleep(Duration::from_secs(1)); 
    } 
} 

/// Asynchronous function handling the connection state engine 
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

/// Helper function implementing unsafe FFI structures to pipe pin profiles into C 
fn init_camera() -> anyhow::Result<()> { 
    unsafe { 
        // Fix 5: Resolves directly through esp_sys
        let config = esp_camera::camera_config_t { 
            pin_pwdn: PIN_PWDN, 
            pin_reset: PIN_RESET, 
            pin_xclk: PIN_XCLK, 
            //pin_sccb_sda: PIN_SDA, 
            //pin_sccb_scl: PIN_SCL, 
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
            xclk_freq_hz: 20000000, 
            ledc_timer: esp_sys::ledc_timer_t_LEDC_TIMER_0, 
            ledc_channel: esp_sys::ledc_channel_t_LEDC_CHANNEL_0, 
            pixel_format: esp_camera::pixformat_t_PIXFORMAT_JPEG, 
            frame_size: esp_camera::framesize_t_FRAMESIZE_VGA, 
            jpeg_quality: 12, 
            fb_count: 2, 
            fb_location: esp_camera::camera_fb_location_t_CAMERA_FB_IN_PSRAM, 
            grab_mode: esp_camera::camera_grab_mode_t_CAMERA_GRAB_WHEN_EMPTY, 
            ..Default::default() 
        }; 

        // Fix 6: Call directly via unified system path
        let err = esp_camera::esp_camera_init(&config); 
        if err != esp_sys::ESP_OK { 
            return Err(anyhow::anyhow!("Failed to initialize camera device OV3660, error code: {}", err)); 
        } 

        // Fix 7: Changed `esp_cam::` to `esp_camera::`
        let sensor = esp_camera::esp_camera_sensor_get(); 
        if !sensor.is_null() { 
            ((*sensor).set_vflip.unwrap())(sensor, 1); 
            ((*sensor).set_hmirror.unwrap())(sensor, 1); 
        } 
        info!("OV3660 camera sensor calibrated and running inside its own module namespace!"); 
    } 
    Ok(()) 
}
