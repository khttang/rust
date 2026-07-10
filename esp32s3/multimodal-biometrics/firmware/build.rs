use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=bindings.h");

    let project_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let camera_dir = PathBuf::from(&project_dir).join("esp32-camera");
    let camera_dir_os = camera_dir.as_os_str();

    // 1. Tell CMake where to find the C component folder
    println!("cargo:rustc-env=ESP_IDF_EXTRA_COMPONENTS_DIRS={}", camera_dir.display());
    env::set_var("ESP_IDF_EXTRA_COMPONENTS_DIRS", camera_dir_os);

    // 2. Fix: Pass the driver header paths directly to bindgen
    // These match the internal folder structure of the clone repository
    println!("cargo:rustc-env=BINDGEN_EXTRA_CLANG_ARGS=-I{}/driver/include -I{}/conversions/include", 
        camera_dir.display(), 
        camera_dir.display()
    );

    embuild::espidf::sysenv::output();
}
