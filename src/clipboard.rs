use anyhow::{Result, anyhow};
use log::{error, info, debug};
use std::process::Command;
use std::fs::{self, create_dir_all, File};
use std::io::Write;
use std::io;
use std::process::Stdio;

/// Clipboard helper for Linux
pub struct Clipboard;

impl Clipboard {
    /// Copy text to clipboard using xclip or wl-copy based on environment
    pub fn copy_to_clipboard(text: &str) -> Result<()> {
        info!("Copying text to clipboard");
        
        // First check if we're in Wayland
        let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();
        
        if is_wayland {
            // Use wl-copy for Wayland
            info!("Using wl-copy for Wayland clipboard");
            let result = Command::new("wl-copy")
                .arg(text)
                .status();
                
            match result {
                Ok(status) if status.success() => {
                    info!("Text copied to clipboard (wl-copy)");
                    Ok(())
                },
                Ok(status) => {
                    error!("wl-copy exited with status: {}", status);
                    // Fall back to user clipboard
                    copy_to_user_clipboard(text)
                },
                Err(e) => {
                    // Try xclip as a fallback
                    info!("wl-copy not available ({}), trying xclip", e);
                    match Self::copy_with_xclip(text) {
                        Ok(_) => Ok(()),
                        Err(_) => copy_to_user_clipboard(text),
                    }
                }
            }
        } else {
            // Use xclip for X11
            match Self::copy_with_xclip(text) {
                Ok(_) => Ok(()),
                Err(_) => copy_to_user_clipboard(text),
            }
        }
    }
    
    /// Copy text using xclip
    pub fn copy_with_xclip(text: &str) -> Result<()> {
        debug!("Attempting to copy using xclip");
        
        // Create a child process with piped stdin
        let mut child = Command::new("xclip")
            .arg("-selection")
            .arg("clipboard")
            .stdin(Stdio::piped())
            .spawn()?;
        
        // Get a handle to the stdin of the child process
        if let Some(mut stdin) = child.stdin.take() {
            // Write the text to the child's stdin
            stdin.write_all(text.as_bytes())?;
            // Dropping stdin here closes it, which is necessary to avoid hanging
        }
        
        // Wait for the child process to complete
        let status = child.wait()?;
        
        if status.success() {
            info!("Successfully copied text to clipboard using xclip");
            Ok(())
        } else {
            Err(anyhow!("Failed to copy text to clipboard using xclip"))
        }
    }
    
    /// Copy text using xsel
    pub fn copy_with_xsel(text: &str) -> Result<()> {
        debug!("Attempting to copy using xsel");
        
        // Create a child process with piped stdin
        let mut child = Command::new("xsel")
            .arg("--clipboard")
            .arg("--input")
            .stdin(Stdio::piped())
            .spawn()?;
        
        // Get a handle to the stdin of the child process
        if let Some(mut stdin) = child.stdin.take() {
            // Write the text to the child's stdin
            stdin.write_all(text.as_bytes())?;
            // Dropping stdin here closes it, which is necessary to avoid hanging
        }
        
        // Wait for the child process to complete
        let status = child.wait()?;
        
        if status.success() {
            info!("Successfully copied text to clipboard using xsel");
            Ok(())
        } else {
            Err(anyhow!("Failed to copy text to clipboard using xsel"))
        }
    }
}

/// Copy text to user-specific clipboard file
fn copy_to_user_clipboard(text: &str) -> Result<()> {
    info!("Falling back to user clipboard file");
    
    // Check if we have the user-clipboard.sh script
    let home_dir = dirs::home_dir().ok_or_else(|| anyhow!("Could not determine home directory"))?;
    let script_path = home_dir.join(".local/bin/user-clipboard.sh");
    
    if script_path.exists() {
        // Use the script if it exists
        match Command::new(&script_path)
            .arg("--copy")
            .arg(text)
            .status() {
            Ok(status) if status.success() => {
                info!("Text copied to user clipboard using script");
                Ok(())
            },
            _ => {
                // Fall back to direct file write
                write_to_clipboard_file(text)
            }
        }
    } else {
        // Write directly to file
        write_to_clipboard_file(text)
    }
}

/// Write text directly to clipboard file
fn write_to_clipboard_file(text: &str) -> Result<()> {
    info!("Writing text directly to clipboard file");
    let home_dir = dirs::home_dir().ok_or_else(|| anyhow!("Could not determine home directory"))?;
    let cache_dir = home_dir.join(".cache/wispr");
    
    // Ensure cache directory exists
    create_dir_all(&cache_dir)?;
    
    // Write text to clipboard file
    let clipboard_file = cache_dir.join("clipboard.txt");
    let mut file = File::create(&clipboard_file)?;
    file.write_all(text.as_bytes())?;
    
    info!("Text saved to {}", clipboard_file.display());
    Ok(())
}

/// Paste text from clipboard (optional function if needed)
pub fn paste_from_clipboard() -> Result<String> {
    info!("Pasting text from clipboard");
    
    // First check if we're in Wayland
    let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();
    
    if is_wayland {
        // Use wl-paste for Wayland
        match Command::new("wl-paste").output() {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout).to_string();
                Ok(text)
            },
            _ => {
                // Try xclip as fallback
                match paste_with_xclip() {
                    Ok(text) => Ok(text),
                    Err(_) => paste_from_user_clipboard(),
                }
            }
        }
    } else {
        // Use xclip for X11
        match paste_with_xclip() {
            Ok(text) => Ok(text),
            Err(_) => paste_from_user_clipboard(),
        }
    }
}

/// Paste from user clipboard file
fn paste_from_user_clipboard() -> Result<String> {
    info!("Reading from user clipboard file");
    
    // Check if we have the user-clipboard.sh script
    let home_dir = dirs::home_dir().ok_or_else(|| anyhow!("Could not determine home directory"))?;
    let script_path = home_dir.join(".local/bin/user-clipboard.sh");
    
    if script_path.exists() {
        // Use the script if it exists
        match Command::new(&script_path)
            .arg("--paste")
            .output() {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout).to_string();
                Ok(text)
            },
            _ => {
                // Fall back to direct file read
                read_from_clipboard_file()
            }
        }
    } else {
        // Read directly from file
        read_from_clipboard_file()
    }
}

/// Read text directly from clipboard file
fn read_from_clipboard_file() -> Result<String> {
    info!("Reading text directly from clipboard file");
    let home_dir = dirs::home_dir().ok_or_else(|| anyhow!("Could not determine home directory"))?;
    let clipboard_file = home_dir.join(".cache/wispr/clipboard.txt");
    
    if clipboard_file.exists() {
        let content = fs::read_to_string(&clipboard_file)?;
        Ok(content)
    } else {
        Err(anyhow!("Clipboard file does not exist"))
    }
}

/// Paste text using xclip
fn paste_with_xclip() -> Result<String> {
    debug!("Attempting to paste using xclip");
    
    match Command::new("xclip")
        .arg("-selection")
        .arg("clipboard")
        .arg("-out")
        .stdout(Stdio::piped())
        .spawn() {
        Ok(mut child) => {
            let mut output = String::new();
            if let Some(stdout) = &mut child.stdout {
                io::Read::read_to_string(stdout, &mut output)?;
            }
            
            let status = child.wait()?;
            if status.success() {
                Ok(output)
            } else {
                // Try xsel as fallback
                paste_with_xsel()
            }
        },
        Err(_) => {
            // Try xsel as fallback
            paste_with_xsel()
        }
    }
}

/// Paste text using xsel
fn paste_with_xsel() -> Result<String> {
    debug!("Attempting to paste using xsel");
    
    match Command::new("xsel")
        .arg("--clipboard")
        .arg("--output")
        .stdout(Stdio::piped())
        .spawn() {
        Ok(mut child) => {
            let mut output = String::new();
            if let Some(stdout) = &mut child.stdout {
                io::Read::read_to_string(stdout, &mut output)?;
            }
            
            let status = child.wait()?;
            if status.success() {
                Ok(output)
            } else {
                Err(anyhow!("xsel command failed"))
            }
        },
        Err(e) => {
            Err(anyhow!("Failed to execute xsel command: {}", e))
        }
    }
}

/// Simple function to set text to clipboard
pub fn set_text(text: &str) -> Result<()> {
    match Clipboard::copy_to_clipboard(text) {
        Ok(_) => Ok(()),
        Err(_) => copy_to_user_clipboard(text),
    }
}

/// Simple function to get text from clipboard
pub fn get_text() -> Result<String> {
    paste_from_clipboard()
} 