#![no_std]
#![no_main]

use cortex_m_rt::entry;
use rtt_target::{rprintln, rtt_init_print};
use sha2::{Sha256, Digest};
use ed25519_dalek::{Verifier, VerifyingKey, Signature};

#[repr(C)]
pub struct FirmwareHeader {
    pub magic: u32,         // Unique identifier (e.g., 0x53424F4F for 'SBOOT')
    pub version: u32,       // Monotonic version number (prevents anti-rollback attacks)
    pub image_size: u32,    // Raw length of the executable payload
    pub reserved: [u32; 5], // Future expansion padding
}

// KHIEM Testing
const MY_MEMORY: *mut u32 = 0x4001080C as *mut u32;

// 1. Root of Trust: Embed your deployment public key directly into the bootloader Flash.
// The matching private key remains strictly confidential on your secure build server.
const ROT_PUBLIC_KEY_BYTES: [u8; 32] = [/* Your 32-byte Ed25519 Public Key */];

pub unsafe fn verify_and_flash_image(
    header: &FirmwareHeader,
    payload_flash_address: *const u8,
    signature_bytes: &[u8; 64]
) -> Result<(), &'static str> {

    // KHIEM Testing
    unsafe {
        core::ptr::write_volatile(MY_MEMORY, 1 << 5);
    }

    // Initialize the Root of Trust Verifying Key
    let public_key = VerifyingKey::from_bytes(&ROT_PUBLIC_KEY_BYTES)
        .map_err(|_| "Invalid RoT Public Key layout")?;

    let signature = Signature::from_bytes(signature_bytes);

    // 2. Cryptographic Hash: Read the exact size of the payload from Flash and compute SHA-256
    let mut hasher = Sha256::new();
    let payload_slice = core::slice::from_raw_parts(payload_flash_address, header.image_size as usize);
    hasher.update(payload_slice);
    let hash_result = hasher.finalize();

    // 3. Verification: Ensure the hash signature matches our RoT
    public_key.verify(&hash_result, &signature)
        .map_err(|_| "Signature Verification Failed! Rejecting Binary.")?;

    Ok(())
}

// The #[entry] macro safely claims the Reset_Handler sequence behind the scenes
#[entry]
fn main() -> ! {
    // Initialize the debug printing channels
    rtt_init_print!();
    rprintln!("Bootloader/Application successfully initialized via Rust!");

    loop {
        // Prevent the CPU from melting using a 'Wait For Interrupt' instruction
        cortex_m::asm::wfi();
    }
}

// Define the required fallback routine if your code ever panics
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    rprintln!("CRASH: {:?}", info);
    loop {
        cortex_m::asm::bkpt(); // Trigger a hardware breakpoint
    }
}
