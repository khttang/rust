use tokio::net::{TcpListener, UnixListener};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[cfg(target_os = "macos")]
mod os_macos;
#[cfg(target_os = "macos")]
use os_macos as os_engine;

#[cfg(target_os = "linux")]
mod os_linux;
#[cfg(target_os = "linux")]
use os_linux as os_engine;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Define the strict payload boundary at compile time
    const MAX_PAYLOAD_SIZE: u64 = 16;
    let tcp_addr = "127.0.0.1:8080";
    let uds_path = "/tmp/watchdog.sock";

    // Clean up any leftover socket file from previous runs
    let _ = std::fs::remove_file(uds_path);

    let tcp_listener = TcpListener::bind(tcp_addr).await?;
    let uds_listener = UnixListener::bind(uds_path)?;

    println!("--- Overlord Autonomous Process Supervisor ---");

    // Shared State 1: Atomic flag for high-speed heartbeat checks
    let is_healthy = Arc::new(AtomicBool::new(true));

    // Shared State 2: Active PID lock so the Watchdog can find and kill the zombie process
    let active_pid = Arc::new(Mutex::new(None));

    // Spawn initial worker and save its PID
    let pid = os_engine::spawn_and_monitor("/bin/sleep")?;
    {
        let mut pid_guard = active_pid.lock().await;
        *pid_guard = Some(pid);
    }

    // --- TASK 1: The Asynchronous Watchdog Timer Loop ---
    let health_flag = Arc::clone(&is_healthy);
    let pid_lock_watchdog = Arc::clone(&active_pid);
    tokio::spawn(async move {
        let timeout_duration = Duration::from_millis(10000); // 15-second deadline
        println!("[Watchdog Timer] Monitoring started. Expecting heartbeats every {}ms...", timeout_duration.as_millis());

        loop {
            tokio::time::sleep(timeout_duration).await;

            if !health_flag.swap(false, Ordering::Relaxed) {
                println!("\n🚨 [WATCHDOG ALERT] Missed heartbeat deadline!");

                // Acquire the PID lock to safely execute recovery sequence
                let mut pid_guard = pid_lock_watchdog.lock().await;
                if let Some(zombie_pid) = *pid_guard {
                    println!("[Recovery Step 1/2] Terminating unresponsive PID {}...", zombie_pid);
                    let _ = os_engine::stop_process(zombie_pid);
                }

                println!("[Recovery Step 2/2] Respawning a fresh worker image...");
                match os_engine::spawn_and_monitor("/bin/sleep") {
                    Ok(new_pid) => {
                        println!("[Recovery Success] New worker online with PID: {:?}. System restored.", new_pid);
                        *pid_guard = Some(new_pid);
                        health_flag.store(true, Ordering::Relaxed);
                    }
                    Err(e) => {
                        eprintln!("[Critical Failure] Could not restore autonomy: {}", e);
                        *pid_guard = None;
                    }
                }
            } else {
                println!("[Watchdog Timer] Check passed. Worker is verified alive.");
            }
        }
    });

    // --- TASK 2: Unix Domain Socket Listener (Receives Heartbeats) ---
    let health_flag_uds = Arc::clone(&is_healthy);
    tokio::spawn(async move {
        loop {
            if let Ok((stream, _)) = uds_listener.accept().await {
                let health_flag_clone = Arc::clone(&health_flag_uds);

                // Spawn a dedicated thread per connection to avoid blocking the listener loop
                tokio::spawn(async move {
                    // 1. Allocate a fixed-size array on the stack (No Heap!)
                    let mut stack_buf = [0u8; MAX_PAYLOAD_SIZE as usize];

                    // Create a bounded adapter that will refuse to read more than allocated bytes.
                    // This protects our stack memory from being choked by huge payloads.
                    let mut bounded_stream = stream.take(MAX_PAYLOAD_SIZE);

                    // 2. Read raw bytes directly into our stack buffer
                    match bounded_stream.read(&mut stack_buf).await {
                        Ok(bytes_read) if bytes_read > 0 => {
                            let raw_payload = &stack_buf[..bytes_read];
                            // 3. Perform a zero-copy, heap-free match against raw byte literals
                            // This eliminates String parsing and dynamic allocation entirely

                            // Use standard library slice methods to strip trailing whitespace bytes safely
                            let trimmed_payload = match raw_payload.strip_suffix(b"\r\n") {
                                Some(s) => s,
                                None => match raw_payload.strip_suffix(b"\n") {
                                    Some(s) => s,
                                    None => raw_payload,
                                },
                            };

                            if trimmed_payload == b"HEARTBEAT" {
                                println!("[UDS Server] [ZERO-ALLOCATION] Verified valid HEARTBEAT byte-array payload!");
                                health_flag_clone.store(true, Ordering::Relaxed);
                            } else {
                                println!("[UDS Server] Received unknown bytes payload.");
                            }
                        }
                        _ => {}
                    }
                    // The 'bounded_stream' and 'stream' variables go out of scope here.
                    // Rust automatically drops them, closing the socket file descriptor.
                    // This forces the OS to instantly discard any remaining unread over-sized data,
                    // resetting the buffer cleanly for the next client connection.
                });
            }
        }
    });

    // --- TASK 3: Regular TCP Control Plane Loop (For Admin Overrides) ---
    loop {
        let (mut socket, _) = tcp_listener.accept().await?;
        tokio::spawn(async move {
            let mut buf = [0; 1024];
            if let Ok(n) = socket.read(&mut buf).await {
                let inbound_command = String::from_utf8_lossy(&buf[0..n]).trim().to_string();
                if inbound_command.starts_with("STOP ") {
                    if let Some(pid_str) = inbound_command.split_whitespace().nth(1) {
                        if let Ok(pid) = pid_str.parse::<i32>() {
                            let _ = os_engine::stop_process(pid);
                            let _ = socket.write_all(b"SUCCESS\n").await;
                        }
                    }
                }
            }
        });
    }
}

// TESTING:
// First terminal: cargo run
// Second terminal: echo "HEARTBEAT" | nc -U /tmp/watchdog.sock