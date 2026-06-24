#![cfg(target_os = "linux")]

use nix::sys::signal::{self, Signal, SigSet};
use nix::sys::signalfd::SignalFd;
use nix::unistd::Pid;
use std::process::Command;

/// Spawns a process and monitors it using Linux-specific signalfd.
pub fn spawn_and_monitor(cmd_path: &str) -> Result<i32, String> {
    println!("[Linux Engine] Spawning process: {}", cmd_path);

    // 1. Block SIGCHLD globally on the current thread before spawning the child.
    // If we don't block it, the OS default handler will reap the child before
    // our signalfd can intercept it.
    let mut mask = SigSet::empty();
    mask.add(Signal::SIGCHLD);
    mask.thread_block().map_err(|e| format!("Failed to block SIGCHLD: {}", e))?;

    // 2. Spawn the process
    let child = Command::new(cmd_path)
        .arg("30")
        .spawn()
        .map_err(|e| e.to_string())?;

    let target_pid = child.id() as i32;
    println!("[Linux Engine] Tracked PID: {}. Creating signalfd descriptor...", target_pid);

    // 3. Offload the blocking signal reading to a background thread
    tokio::task::spawn_blocking(move || {
        if let Err(e) = monitor_via_signalfd(mask, target_pid) {
            eprintln!("[Linux Engine Error] Signalfd failed for PID {}: {}", target_pid, e);
        }
    });

    Ok(target_pid)
}

fn monitor_via_signalfd(mask: SigSet, target_pid: i32) -> Result<(), String> {
    // 4. Create the signalfd descriptor bound to our masked SIGCHLD signal
    let mut sfd = SignalFd::new(&mask)
        .map_err(|e| format!("Failed to initialize signalfd: {}", e))?;

    println!("[signalfd Thread] Waiting for kernel to write SIGCHLD to descriptor...");

    // 5. Block and read until a signal structure lands in the descriptor
    loop {
        // read_signal blocks the thread until the kernel passes an event payload
        match sfd.read_signal() {
            Ok(Some(siginfo)) => {
                // Ssi_signo confirms the signal type
                // Ssi_pid tells us WHICH child process actually triggered it
                let triggering_pid = siginfo.ssi_pid as i32;

                if triggering_pid == target_pid {
                    let exit_status = siginfo.ssi_status;
                    println!(
                        "\n💥 [Linux Engine Alert] Process ID {} has exited natively! (Status code: {})",
                        target_pid, exit_status
                    );
                    break; // Exit the loop and thread cleanly
                }
            }
            Ok(None) => continue, // Spurious wake up, keep reading
            Err(e) => return Err(format!("Error reading signal fd: {}", e)),
        }
    }

    Ok(())
}

/// Send a terminating signal to a process using Linux POSIX bindings.
pub fn stop_process(pid: i32) -> Result<(), String> {
    let native_pid = Pid::from_raw(pid);
    signal::kill(native_pid, Signal::SIGTERM)
        .map_err(|e| format!("Failed to signal Linux process: {}", e))
}