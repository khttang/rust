#![cfg(target_os = "macos")]

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use std::process::Command;
use std::ptr;

/// Spawns a process and monitors it using macOS-specific APIs.
pub fn spawn_and_monitor(cmd_path: &str) -> Result<i32, String> {
    println!("[macOS Engine] Spawning process: {}", cmd_path);

    // Spawn a dummy process (passing a 30-second sleep argument)
    let child = Command::new(cmd_path)
        .arg("30")
        .spawn()
        .map_err(|e| e.to_string())?;

    let pid = child.id() as i32;
    println!("[macOS Engine] Tracked PID: {}. Registering with kqueue...", pid);

    // Offload the blocking kqueue monitoring to a background thread
    tokio::task::spawn_blocking(move || {
        if let Err(e) = monitor_via_kqueue(pid) {
            eprintln!("[macOS Engine Error] Kqueue failed for PID {}: {}", pid, e);
        }
    });
    
    Ok(pid)
}

fn monitor_via_kqueue(target_pid: i32) -> Result<(), String> {
    use nix::libc::{kqueue, kevent};
    use nix::libc::{EVFILT_PROC, EV_ADD, EV_ENABLE, EV_CLEAR, NOTE_EXIT};

    // 1. Create a new kqueue instance
    let kq = unsafe { kqueue() };
    if kq < 0 {
        return Err("Failed to create kqueue descriptor".to_string());
    }

    // 2. Define the event structure we want the kernel to listen to
    // We target EVFILT_PROC, asking for a notification when the target_pid exits (NOTE_EXIT)
    let change_event = kevent {
        ident: target_pid as usize,
        filter: EVFILT_PROC,
        flags: EV_ADD | EV_ENABLE | EV_CLEAR,
        fflags: NOTE_EXIT,
        data: 0,
        udata: ptr::null_mut(),
    };

    let mut trigger_event = kevent {
        ident: 0,
        filter: 0,
        flags: 0,
        fflags: 0,
        data: 0,
        udata: ptr::null_mut(),
    };

    println!("[kqueue Thread] Waiting for PID {} to exit...", target_pid);

    // 3. Block until the kernel catches the process exit event
    // Passing null for the timeout structure makes this block indefinitely until triggered
    let change_count = unsafe {
        kevent(
            kq,
            &change_event,
            1,
            &mut trigger_event,
            1,
            ptr::null(),
        )
    };

    if change_count < 0 {
        return Err("Kqueue tracking system call failed".to_string());
    }

    // 4. Handle the triggered event
    if trigger_event.fflags & NOTE_EXIT != 0 {
        let status_code = { trigger_event.data };
        println!(
            "\n💥 [macOS Engine Alert] Process ID {} has exited natively! (Status code: {})",
            target_pid, status_code
        );
    }

    // Clean up the descriptor
    unsafe { nix::libc::close(kq) };
    Ok(())
}

/// Send a terminating signal to a process using Mac POSIX bindings.
pub fn stop_process(pid: i32) -> Result<(), String> {
    let native_pid = Pid::from_raw(pid);
    signal::kill(native_pid, Signal::SIGTERM)
        .map_err(|e| format!("Failed to signal Mac process: {}", e))
}