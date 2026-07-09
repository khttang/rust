use anyhow::Result;
use esp_idf_svc::hal::peripherals::Peripherals;
//use esp_idf_svc::prelude::*;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::http::server::{Configuration, EspHttpServer};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{AuthMethod, ClientConfiguration, Configuration as WifiConfig, EspWifi};
use std::thread::sleep;
use std::time::Duration;

// Bindings to native camera structs managed via esp-idf-sys
use esp_idf_sys::{
    camera_config_t, esp_camera_fb_get, esp_camera_fb_return, esp_camera_init, pixformat_t,
};

const WIFI_SSID: &str = "YOUR_WIFI_NAME";
const WIFI_PASS: &str = "YOUR_WIFI_PASSWORD";

fn main() -> Result<()> {
    esp_idf_svc::log::EspLogger::initialize_default();
    
    let peripherals = Peripherals::take()?;
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    // 1. Initialize Wi-Fi at a high level
    let mut wifi = EspWifi::new(peripherals.modem, sys_loop, Some(nvs))?;
    wifi.set_configuration(&WifiConfig::Client(ClientConfiguration {
        ssid: WIFI_SSID.try_into().unwrap(),
        password: WIFI_PASS.try_into().unwrap(),
        auth_method: AuthMethod::WPA2Personal,
        ..Default::default()
    }))?;

    wifi.start()?;
    wifi.connect()?;
    while !wifi.is_connected()? {
        sleep(Duration::from_millis(500));
    }
    println!("Wi-Fi Connected! IP Info: {:?}", wifi.sta_netif().get_ip_info()?);

    // 2. High-Level OV3660 Camera Configuration
    // Assign pins according to your specific ESP32-S3-CAM dev board board schematic
    let cam_config = camera_config_t {
        pin_pwdn: -1,     // Update with your board's physical pin numbers
        pin_reset: -1,
        pin_xclk: 10,
        pin_sscb_sda: 40,
        pin_sscb_scl: 39,
        pin_d7: 13, pin_d6: 12, pin_d5: 11, pin_d4: 10,
        pin_d3: 9,  pin_d2: 8,  pin_d1: 7,  pin_d0: 6,
        pin_vsync: 5, pin_href: 4, pin_pclk: 3,
        xclk_freq_hz: 20000000,
        ledc_timer: esp_idf_svc::ledc_timer_t_LEDC_TIMER_0,
        ledc_channel: esp_idf_svc::ledc_channel_t_LEDC_CHANNEL_0,
        pixel_format: pixformat_t_PIXFORMAT_JPEG, // Let the camera module handle compressed JPEG
        frame_size: esp_idf_svc::framesize_t_FRAMESIZE_VGA, // 640x480 resolution
        jpeg_quality: 12, // 0-63 (lower numbers equal higher quality)
        fb_count: 2,      // Double-buffering prevents frame tearing
        fb_in_psram: true, // Use the board's external 8MB PSRAM memory
        ..Default::default()
    };

    // Initialize the native C driver framework
    unsafe {
        let err = esp_camera_init(&cam_config);
        if err != esp_idf_svc::ESP_OK {
            return Err(anyhow::anyhow!("Camera Init Failed with code {}", err));
        }
    }

    // 3. Launch HTTP MJPEG Streaming Server
    let mut server = EspHttpServer::new(&Configuration::default())?;
    
    server.fn_handler("/stream", esp_idf_svc::http::Method::Get, |request| {
        let mut response = request.into_response(
            200,
            Some("OK"),
            &[
                ("Content-Type", "multipart/x-mixed-replace; boundary=123456789000000000000987654321"),
                ("Connection", "close"),
            ],
        )?;

        loop {
            unsafe {
                // Fetch a raw frame buffer from the driver queue
                let fb = esp_camera_fb_get();
                if !fb.is_null() {
                    let frame_data = std::slice::from_raw_parts((*fb).buf, (*fb).len);
                    
                    // Format the HTTP multipart boundaries wrapping the raw JPEG image bytes
                    let mut part_header = format!(
                        "\r\n--123456789000000000000987654321\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                        frame_data.len()
                    );
                    
                    if response.write(part_header.as_bytes()).is_err() {
                        esp_camera_fb_return(fb);
                        break; // Connection lost or closed by browser client
                    }
                    if response.write(frame_data).is_err() {
                        esp_camera_fb_return(fb);
                        break;
                    }
                    
                    // Return buffer pointer back to hardware stack 
                    esp_camera_fb_return(fb);
                }
            }
            sleep(Duration::from_millis(30)); // Caps stream output to ~30 FPS
        }
        Ok(())
    })?;

    // Keep the main thread alive indefinitely while background HTTP service runs
    loop {
        sleep(Duration::from_secs(1));
    }
}


/*fn main() {
    // It is necessary to call this function once. Otherwise, some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("Hello, world!");
}*/
