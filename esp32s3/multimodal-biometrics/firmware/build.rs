use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=bindings.h");
    
    let project_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let project_path = PathBuf::from(&project_dir);
    
    let camera_dir = project_path.join("esp32-camera");
    let shims_dir = project_path.join("c_components");

    // Combine both paths separated by a semicolon (CMake's list delimiter)
    let combined_components = format!("{};{}", camera_dir.display(), shims_dir.display());

    // 1. Tell CMake where to look for both native component drivers
    println!("cargo:rustc-env=ESP_IDF_EXTRA_COMPONENTS_DIRS={}", combined_components);
    env::set_var("ESP_IDF_EXTRA_COMPONENTS_DIRS", &combined_components);

    // 2. Pass driver header paths directly to bindgen
    println!(
        "cargo:rustc-env=BINDGEN_EXTRA_CLANG_ARGS=-I{}/driver/include -I{}/conversions/include",
        camera_dir.display(),
        camera_dir.display()
    );

    // 3. Let esp-idf-sys complete standard toolchain generation smoothly
    embuild::espidf::sysenv::output();
}
