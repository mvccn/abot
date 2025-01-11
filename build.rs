use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Check if .env file exists
    if !std::path::Path::new(".env").exists() {
        println!("Creating .env file...");
        std::fs::write(".env", "DEEPSEEK_API_KEY=your-api-key-here").expect("Failed to create .env file");
    }

    // Get home directory
    let home = std::env::var("HOME").expect("Failed to get HOME directory");
    let config_bin = PathBuf::from(format!("{}/.config/bin", home));

    // Create ~/.config/bin if it doesn't exist
    std::fs::create_dir_all(&config_bin).expect("Failed to create ~/.config/bin directory");

    // Note: The actual binary copy will happen in a post-build script
    // because during build.rs execution, the binary hasn't been built yet
    println!("cargo:warning=After building, run:");
    println!("cargo:warning=cp target/release/abot ~/.config/bin/");

    // Create an installation script
    let install_script = "#!/bin/bash
if [ ! -d \"$HOME/.config/bin\" ]; then
    mkdir -p \"$HOME/.config/bin\"
fi
cp target/release/abot \"$HOME/.config/bin/\"
chmod +x \"$HOME/.config/bin/abot\"
echo \"Installation complete! Make sure $HOME/.config/bin is in your PATH\"";

    std::fs::write("install.sh", install_script).expect("Failed to create install script");
    Command::new("chmod")
        .arg("+x")
        .arg("install.sh")
        .status()
        .expect("Failed to make install script executable");

    println!("=== Installation Instructions ===");
    println!("1. Build the release version: cargo build --release");
    println!("2. Run the install script: ./install.sh");
    println!("3. Edit .env file and add your DeepSeek API key");
    println!("4. Make sure ~/.config/bin is in your PATH");
    println!("5. Run 'abot' from anywhere!");
} 