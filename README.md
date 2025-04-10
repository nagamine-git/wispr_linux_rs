# Wispr Linux

A voice-to-text application for Linux with a system tray icon.

## Features

- System tray icon with GTK integration
- Voice recording with one-click operation
- Transcription of audio to text
- Automatic clipboard paste support
- Customizable keyboard shortcuts

## Requirements

### Linux Dependencies

For the tray icon to work on Linux with GTK, you need to install:

```bash
# For Debian/Ubuntu
sudo apt-get install libgtk-3-dev

# For Arch Linux/Manjaro
sudo pacman -S gtk3

# For Fedora
sudo dnf install gtk3-devel
```

## Configuration

The application uses a TOML configuration file located at:

```
~/.config/wispr/wispr_linux_rs/config.toml
```

You can specify a custom configuration path with the `--config` flag.

Example configuration:

```toml
# OpenAI API key (required for transcription)
api_key = "your-api-key-here"

# Recording settings
[recording]
max_duration_secs = 60
sample_rate = 44100
play_sounds = true

# UI settings
[ui]
dark_mode = true
show_notifications = true

# Keyboard shortcut settings
[shortcuts]
toggle_recording = "Control+Alt+R"
auto_paste = true
```

## Usage

Run the application:

```bash
./run_wispr.sh
```

Or with cargo:

```bash
cargo run --features tray
```

### System Tray

- Left-click on the tray icon to start/stop recording
- Right-click to open the menu with additional options
- The tray icon changes color based on the current status:
  - Blue: Idle
  - Red: Recording
  - Orange: Transcribing

## Development

This application is built with:

- Rust
- GTK for the user interface
- tray-icon and muda for system tray integration
- tokio for async operations

## License

MIT

