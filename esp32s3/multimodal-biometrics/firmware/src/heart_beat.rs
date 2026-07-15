use esp_idf_svc::hal::gpio::{PinDriver, AnyOutputPin};
use std::thread; 
use std::time::Duration; 

// 1. Create a zero-cost wrapper structure to hold our raw pointer
pub struct SendRawPtr(pub *mut AnyOutputPin<'static>);

// 2. Explicitly tell the compiler that transferring this raw address between threads is safe
unsafe impl Send for SendRawPtr {}

pub fn spawn_basic_heartbeat(pin: AnyOutputPin) -> anyhow::Result<()> {
    // 3. Transmute or unsafe cast the pin's local lifetime to a static one 
    // so it can safely live inside an independent background thread.
    let static_pin: AnyOutputPin<'static> = unsafe { std::mem::transmute(pin) };

    // 4. Box it up and turn it into a raw pointer
    let boxed_pin = Box::new(static_pin);
    let raw_pin_ptr = Box::into_raw(boxed_pin);
    
    // 5. Wrap the pointer inside our thread-safe Send token container
    let thread_safe_token = SendRawPtr(raw_pin_ptr);

    thread::Builder::new()
        .name("heartbeat_task".to_string())
        .stack_size(2048)
        .spawn(move || {
            // 6. Extract the pointer from our wrapped token and reconstruct the owned pin
            let token = thread_safe_token;
            let owned_pin = unsafe { *Box::from_raw(token.0) };

            let mut led = match PinDriver::output(owned_pin) {
                Ok(driver) => driver,
                Err(e) => {
                    log::error!("Failed to instantiate basic LED driver: {:?}", e);
                    return;
                }
            };

            log::info!("Era 2: Running non-blocking basic bit-toggle heartbeat on GPIO 38.");

            loop {
                if let Err(e) = led.toggle() {
                    log::error!("LED toggle error: {:?}", e);
                }
                thread::sleep(Duration::from_millis(500));
            }
        })?;

    Ok(())
}