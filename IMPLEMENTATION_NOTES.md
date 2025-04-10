# Tray Icon Implementation Notes

This document outlines how the system tray icon was implemented for the Wispr Linux application.

## Technology Stack

- **GTK**: Used for the UI toolkit and main loop integration
- **tray-icon**: Crate for cross-platform tray icon implementation
- **glib**: Used for event loop and thread management

## Thread Safety Considerations

When implementing the tray icon, we encountered several thread safety issues:

1. The `TrayIcon` and `Menu` structs from the tray-icon crate use internal `Rc<RefCell<>>` types which are not `Send` or `Sync`.
2. This meant they cannot be safely passed between threads.

### Solutions:

1. **Keep GUI components on the main thread**: All UI components (tray icon, menu) are created and managed within the same thread.
2. **Push thread default context**: Used `gtk_context.push_thread_default()` to ensure GTK operations happen on the correct thread.
3. **Message passing**: Implemented a channel-based communication system to send messages between the application logic and the UI thread.

## Tray Icon State Management

The application defines three states:
- **Idle**: Blue icon, ready to start recording
- **Recording**: Red icon, currently recording audio
- **Transcribing**: Orange icon, processing the recorded audio

Each state affects:
- The tray icon color
- The menu item labels
- The behavior when clicked

## Custom Icons

For simplicity, we generate solid-colored icons programmatically:

```rust
fn create_default_icon(r: u8, g: u8, b: u8) -> Result<Icon> {
    // Create a simple colored icon
    let width = 22;
    let height = 22;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    
    for _ in 0..(width * height) {
        rgba.push(r);    // Red
        rgba.push(g);    // Green
        rgba.push(b);    // Blue
        rgba.push(255);  // Alpha (opaque)
    }
    
    Icon::from_rgba(rgba, width, height)
        .map_err(|e| anyhow!("Icon error: {}", e))
}
```

## Event Handling

The tray application handles the following events:

1. **Left click on tray icon**: Toggle recording state
2. **Menu item clicks**: Start/stop recording, show transcript, or quit
3. **Application state changes**: Update UI elements based on state

## Practical Tips

1. When working with GTK on Linux, always initialize GTK before creating any UI components.
2. Use the glib main loop for event handling to ensure proper integration with the GTK toolkit.
3. Be careful with thread safety when working with UI elements - they should generally stay on one thread.
4. Consider using channels or other message-passing mechanisms to communicate with the UI thread.
5. Use Mutex instead of RefCell for thread-safe state sharing.

## Dependencies Required

For the tray icon to work on Linux with GTK:

```toml
[dependencies]
tray-icon = "0.12"
gtk = "0.18"
glib = "0.18"
```

And the system libraries:

```bash
# For Debian/Ubuntu
sudo apt-get install libgtk-3-dev

# For Arch Linux/Manjaro
sudo pacman -S gtk3

# For Fedora
sudo dnf install gtk3-devel
```