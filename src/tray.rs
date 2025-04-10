use std::thread::{self, JoinHandle};
use std::sync::{Arc, Mutex};
use gtk;
use gtk::prelude::*;
use log::{info, error};
use std::sync::mpsc::{self, Sender, Receiver};
use anyhow::{Result, anyhow};
use tray_icon::{TrayIconBuilder, Icon, menu::{Menu, MenuItem, MenuId}};
use crate::config::Config;

/// Application status representation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppStatus {
    /// Application is idle
    Idle,
    /// Application is recording
    Recording,
    /// Application is transcribing
    Transcribing,
}

impl AppStatus {
    /// Get the icon name based on the status
    pub fn icon_name(&self) -> &'static str {
        match self {
            AppStatus::Idle => "microphone-sensitivity-muted-symbolic",
            AppStatus::Recording => "microphone-sensitivity-high-symbolic",
            AppStatus::Transcribing => "system-run-symbolic",
        }
    }
    
    /// Get tooltip based on status
    pub fn tooltip(&self) -> &'static str {
        match self {
            AppStatus::Idle => "Wispr - Click to start recording",
            AppStatus::Recording => "Wispr - Recording... Click to stop",
            AppStatus::Transcribing => "Wispr - Processing audio...",
        }
    }
    
    /// Get menu item label based on status
    pub fn menu_item_label(&self) -> &'static str {
        match self {
            AppStatus::Idle => "Start Recording",
            AppStatus::Recording => "Stop Recording",
            AppStatus::Transcribing => "Processing...",
        }
    }
}

/// Application state
#[derive(Debug)]
struct AppState {
    pub status: AppStatus,
    config: Config,
    tx_main: Sender<TrayMessage>,
}

impl AppState {
    fn new(config: Config, tx_main: Sender<TrayMessage>) -> Self {
        Self {
            status: AppStatus::Idle,
            config: config.clone(),
            tx_main,
        }
    }
    
    fn toggle_recording(&mut self) {
        match self.status {
            AppStatus::Idle => {
                self.status = AppStatus::Recording;
                let _ = self.tx_main.send(TrayMessage::StartRecording);
            },
            AppStatus::Recording => {
                self.status = AppStatus::Transcribing;
                let _ = self.tx_main.send(TrayMessage::StopRecording);
            },
            AppStatus::Transcribing => { /* Do nothing while processing */ }
        }
    }

    fn show_transcript(&mut self) {
        let _ = self.tx_main.send(TrayMessage::ShowTranscript);
    }

    fn quit(&mut self) {
        let _ = self.tx_main.send(TrayMessage::Exit);
    }
}

/// Messages that can be sent to the tray
pub enum TrayMessage {
    /// Start recording
    StartRecording,
    /// Stop recording and process
    StopRecording,
    /// Show transcript
    ShowTranscript,
    /// Update UI with new status
    UpdateStatus(AppStatus),
    /// Request to exit the application
    Exit,
}

/// Runs the tray application and returns a join handle and a sender for communication
pub fn run_tray_application(config: Config) -> Result<(JoinHandle<Result<()>>, Sender<TrayMessage>)> {
    // Channel for communication with the main thread
    let (tx_main, _rx_main) = mpsc::channel();
    let (tx_handler, rx_handler) = mpsc::channel();
    
    // Set up app state
    let app_state = Arc::new(Mutex::new(AppState {
        status: AppStatus::Idle,
        config: config.clone(),
        tx_main: tx_main.clone(),
    }));
    
    // Create and setup the tray icon in the main thread
    setup_tray_icon(app_state.clone(), tx_handler.clone())?;
    
    // Create a thread to handle commands
    let handler_thread = create_handler_thread(app_state.clone(), rx_handler, tx_main.clone());
    
    Ok((handler_thread, tx_handler))
}

/// Create a thread to handle commands from the main application
fn create_handler_thread(app_state: Arc<Mutex<AppState>>, rx: Receiver<TrayMessage>, tx_main: Sender<TrayMessage>) -> JoinHandle<Result<()>> {
    thread::spawn(move || -> Result<()> {
        loop {
            // Receive message from tray icon
            match rx.recv() {
                Ok(msg) => {
                    match msg {
                        TrayMessage::Exit => {
                            info!("Exiting tray application");
                            break;
                        },
                        TrayMessage::StartRecording => {
                            info!("Starting recording");
                            update_tray_status(app_state.clone(), AppStatus::Recording);
                            // Forward to main thread
                            let _ = tx_main.send(TrayMessage::StartRecording);
                        },
                        TrayMessage::StopRecording => {
                            info!("Stopping recording");
                            update_tray_status(app_state.clone(), AppStatus::Transcribing);
                            // Forward to main thread
                            let _ = tx_main.send(TrayMessage::StopRecording);
                        },
                        TrayMessage::ShowTranscript => {
                            info!("Showing transcript");
                            // Forward to main thread
                            let _ = tx_main.send(TrayMessage::ShowTranscript);
                        },
                        TrayMessage::UpdateStatus(status) => {
                            update_tray_status(app_state.clone(), status);
                        },
                    }
                },
                Err(e) => {
                    error!("Error receiving message: {}", e);
                    break;
                }
            }
        }
        
        Ok(())
    })
}

/// Setup the tray icon in a separate function
fn setup_tray_icon(app_state: Arc<Mutex<AppState>>, tx: Sender<TrayMessage>) -> Result<()> {
    // This needs to run on the main thread
    if !gtk::is_initialized() {
        return Err(anyhow!("GTK not initialized. Call gtk::init() in main thread before setting up the tray."));
    }

    // Create tray menu
    let menu = Menu::new();
    
    // Record item
    let record_item = MenuItem::new("Start Recording", true, None);
    let record_id = record_item.id().clone();
    let _ = menu.append(&record_item);
    
    // Transcript item
    let transcript_item = MenuItem::new("Show Transcript", true, None);
    let transcript_id = transcript_item.id().clone();
    let _ = menu.append(&transcript_item);
    
    // Quit item
    let quit_item = MenuItem::new("Quit", true, None);
    let quit_id = quit_item.id().clone();
    let _ = menu.append(&quit_item);
    
    // Create tray icon
    let idle_icon = create_default_icon(0, 0, 255, 255);
    let icon = Icon::from_rgba(idle_icon.data, idle_icon.width, idle_icon.height).unwrap();
    
    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Wispr Voice-to-Text")
        .with_icon(icon)
        .build()?;
    
    // Set up menu item event handlers using the menu channel
    let menu_channel = tray_icon::menu::MenuEvent::receiver();
    let tx_clone = tx.clone();
    let app_state_clone = app_state.clone();
    
    // Handle menu events in a separate thread
    thread::spawn(move || {
        while let Ok(event) = menu_channel.recv() {
            if *event.id() == record_id {
                let mut state = app_state_clone.lock().unwrap();
                match state.status {
                    AppStatus::Idle => {
                        let _ = tx_clone.send(TrayMessage::StartRecording);
                    },
                    AppStatus::Recording => {
                        let _ = tx_clone.send(TrayMessage::StopRecording);
                    },
                    _ => {}
                }
            } else if *event.id() == transcript_id {
                let _ = tx_clone.send(TrayMessage::ShowTranscript);
            } else if *event.id() == quit_id {
                let _ = tx_clone.send(TrayMessage::Exit);
                gtk::main_quit();
            }
        }
    });
    
    Ok(())
}

fn update_tray_status(app_state: Arc<Mutex<AppState>>, status: AppStatus) {
    let mut state = app_state.lock().unwrap();
    state.status = status;
    
    // Update tray icon based on status
    // This is a placeholder - the actual implementation would update the icon
    // through GTK's main thread
    info!("Tray status updated to: {:?}", status);
}

struct IconData {
    data: Vec<u8>,
    width: u32,
    height: u32,
    channels: u32,
}

fn create_default_icon(r: u8, g: u8, b: u8, a: u8) -> IconData {
    let width = 22;
    let height = 22;
    
    // Create a simple colored icon
    let mut data = Vec::new();
    for _ in 0..width * height {
        data.push(r);
        data.push(g);
        data.push(b);
        data.push(a);
    }
    
    IconData {
        data,
        width,
        height,
        channels: 4,
    }
}