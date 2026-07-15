use core::convert::TryInto;

use esp_idf_svc::wifi::{AsyncWifi, AuthMethod, ClientConfiguration, Configuration as WifiConfiguration, EspWifi}; 
use log::{info, warn}; 

const WIFI_SSID: &str = "SpectrumSetup-AC";

pub async fn connect_wifi(wifi: &mut AsyncWifi<EspWifi<'static>>, net_pwd: &str) -> anyhow::Result<()> { 
    info!("Setting up Wi-Fi configurations..."); 
    let auth_modes = [ 
        (AuthMethod::WPA2WPA3Personal, "WPA2/WPA3 Mixed (With PMF)"), 
        (AuthMethod::WPA2Personal, "WPA2 Personal (Standard)"), 
    ]; 
    for (method, description) in auth_modes { 
        info!("Attempting connection using mode: {}", description); 
        let wifi_configuration = WifiConfiguration::Client(ClientConfiguration { 
            ssid: WIFI_SSID.try_into().unwrap(), 
            password: net_pwd.try_into().unwrap(), 
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