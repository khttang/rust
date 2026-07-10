use edge_executor::LocalExecutor;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{AsyncWifi, AuthMethod, ClientConfiguration, Configuration, EspWifi};
use esp_idf_svc::timer::EspTaskTimerService;
use futures::executor::block_on;
use log::{info, warn};
//use std::sync::Arc;

const WIFI_SSID: &str = "Your_WiFi_Name";
const WIFI_PASS: &str = "Your_WiFi_Password";

fn main() -> anyhow::Result<()> {
    // 1. Initialize the system logging framework
    EspLogger::initialize_default();
    info!("Initializing ESP32-S3 async Wi-Fi system...");

    // 2. Take ownership of vital system peripherals
    let peripherals = Peripherals::take()?;
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    // 3. Initialize the required background task timer service 
    let timer_service = EspTaskTimerService::new()?;

    // 3. Create the standard Wi-Fi controller driver instance
    let mut wifi = AsyncWifi::wrap(
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?,
        sys_loop.clone(), timer_service
    )?;

    // 4. Instantiate a local asynchronous executor thread
    let executor: LocalExecutor = edge_executor::LocalExecutor::new();

    // 5. Spawn and block the main execution thread inside our async network task
    block_on(executor.run(async {
        if let Err(e) = connect_wifi(&mut wifi).await {
            warn!("Failed to establish network connection: {:?}", e);
        } else {
            info!("Wi-Fi cycle successfully completed!");
        }
    }));

    Ok(())
}

/// Asynchronous function handling the connection state engine
async fn connect_wifi(wifi: &mut AsyncWifi<EspWifi<'static>>) -> anyhow::Result<()> {
    info!("Setting up Wi-Fi configurations...");

    let wifi_configuration = Configuration::Client(ClientConfiguration {
        ssid: WIFI_SSID.try_into().unwrap(),
        password: WIFI_PASS.try_into().unwrap(),
        auth_method: AuthMethod::WPA2WPA3Personal, // Adaptive matching fallback
        ..Default::default()
    });

    // Apply configuration settings
    wifi.set_configuration(&wifi_configuration)?;

    info!("Starting Wi-Fi subsystems...");
    wifi.start().await?;

    info!("Scanning and connecting to access point: {}...", WIFI_SSID);
    wifi.connect().await?;

    info!("Waiting for DHCP network assignment to complete...");
    wifi.wait_netif_up().await?;

    // Fetch and display IP configuration assignments
    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
    info!("Network interface status: {:?}", ip_info);

    Ok(())
}


/*
fn main() {
    // It is necessary to call this function once. Otherwise, some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("Hello, world!");
}
*/