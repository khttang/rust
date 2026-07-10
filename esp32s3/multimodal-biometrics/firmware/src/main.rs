use edge_executor::LocalExecutor;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{AsyncWifi, AuthMethod, ClientConfiguration, Configuration, EspWifi};
use esp_idf_svc::timer::EspTaskTimerService;
use futures::executor::block_on;
use log::{info, warn};
use std::time::Duration;

const WIFI_SSID: &str = "SpectrumSetup-AC";
const WIFI_PASS: &str = "T@ngn3t2025";

fn main() -> anyhow::Result<()> {
    // 1. Initialize the system logging framework
    EspLogger::initialize_default();
    info!("Initializing ESP32-S3 async Wi-Fi system...");

    // 2. Take ownership of vital system peripherals
    let peripherals = Peripherals::take()?;
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let timer_service = EspTaskTimerService::new()?;

    // 3. Create the standard Wi-Fi controller driver instance
    let mut wifi = AsyncWifi::wrap(
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?,
        sys_loop.clone(), timer_service
    )?;

    // 4. Instantiate a local asynchronous executor thread
    let executor: LocalExecutor = edge_executor::LocalExecutor::new();

    // 5. Spawn and block the main execution thread inside our async network task
    // BOX THE FUTURE: This moves the entire massive async state machine 
    // off the stack frame and onto the heap safely.
    block_on(executor.run(Box::pin(async {
        if let Err(e) = connect_wifi(&mut wifi).await {
            warn!("Failed to establish network connection: {:?}", e);
        } else {
            info!("Wi-Fi cycle successfully completed!");
        }
    })));

    Ok(())
}

/// Asynchronous function handling the connection state engine
async fn connect_wifi(wifi: &mut AsyncWifi<EspWifi<'static>>) -> anyhow::Result<()> {
    info!("Setting up Wi-Fi configurations...");

    // 1. Array of authentication methods to cycle through
    let auth_modes = [
        (AuthMethod::WPA2WPA3Personal, "WPA2/WPA3 Mixed (With PMF)"),
        (AuthMethod::WPA2Personal, "WPA2 Personal (Standard)"),
    ];

    for (method, description) in auth_modes {
        info!("Attempting connection using mode: {}", description);

        let wifi_configuration = Configuration::Client(ClientConfiguration {
            ssid: WIFI_SSID.try_into().unwrap(),
            password: WIFI_PASS.try_into().unwrap(),
            auth_method: method, // Dynamically assigned mode
            ..Default::default()
        });

        // Apply config
        wifi.set_configuration(&wifi_configuration)?;

        // Ensure the subsystem is fresh and running
        if !wifi.is_started()? {
            wifi.start().await?;
        }

        info!("Connecting to access point...");
        
        // 2. Catch failures on the connection attempt
        if let Err(e) = wifi.connect().await {
            warn!("Physical handshake failed with mode {}: {:?}", description, e);
            // Disconnect and sleep briefly before falling back
            //  FIX: Uses the native ESP-IDF driver delay mechanism
            let _ = wifi.disconnect().await;
            std::thread::sleep(std::time::Duration::from_secs(2));
            continue; // Jump to the next authentication method in our list
        }

        info!("Physical link established! Waiting for DHCP address assignment...");
        
        // 3. Catch failures on the DHCP assignment stage
        if let Err(e) = wifi.wait_netif_up().await {
            warn!("DHCP lease failed using mode {}: {:?}", description, e);
            let _ = wifi.disconnect().await;
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        // Both physical link and IP allocation succeeded!
        let netif = wifi.wifi().sta_netif();
        let ip_info = netif.get_ip_info()?;
        info!("Successfully connected! IP Details: {:?}", ip_info);
        return Ok(());
    }

    // If the loop finishes without returning, both modes failed
    Err(anyhow::anyhow!("Could not connect using any supported authentication standard"))
}
