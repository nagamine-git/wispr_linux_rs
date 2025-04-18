// use std::env;
// use std::process;
// use std::thread::JoinHandle;
// use std::sync::mpsc::{self, Sender};
// use std::path::PathBuf;
// use std::fs;

// use std::thread;

use anyhow::{Result, Context};
use log::{info, error, LevelFilter};
use simple_logger::SimpleLogger;
use gtk;
use clap::Parser;

#[cfg(feature = "tray")]
mod tray;
mod config;
mod api;
mod audio;
mod clipboard;
mod window;
mod text_processor;

/// Wispr Linux - 音声文字起こしアプリケーション
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// 設定ファイルのパス
    #[arg(short, long)]
    config: Option<String>,
}

fn main() -> Result<()> {
    // コマンドライン引数の解析
    let args = Args::parse();

    // Initialize logger
    SimpleLogger::new()
        .with_level(LevelFilter::Info)
        .init()
        .context("Failed to initialize logger")?;

    info!("Starting Wispr Linux");

    // Load configuration with custom path if provided
    let config = config::load_config(args.config)?;
    info!("Configuration loaded");

    // Initialize GTK on the main thread
    if let Err(e) = gtk::init() {
        error!("Failed to initialize GTK: {}", e);
        return Err(anyhow::anyhow!("Failed to initialize GTK"));
    }
    
    // Launch the window application
    let (window_thread, window_sender) = window::run_window_application(config.clone())?;
    info!("Window application started");
    
    #[cfg(feature = "tray")]
    let (tray_thread, tray_sender) = {
        info!("Starting tray application");
        let (thread, sender) = tray::run_tray_application(config.clone())?;
        info!("Tray application started");
        (thread, sender)
    };
    
    // Set up Ctrl+C handler
    #[cfg(not(feature = "tray"))]
    let quit_tx = window_sender.clone();
    #[cfg(feature = "tray")]
    let quit_tx = {
        let window_tx = window_sender.clone();
        let tray_tx = tray_sender.clone();
        move || {
            let _ = window_tx.send(window::WindowMessage::Exit);
            let _ = tray_tx.send(tray::TrayMessage::Exit);
        }
    };
    
    #[cfg(not(feature = "tray"))]
    ctrlc::set_handler(move || {
        info!("Received Ctrl+C, shutting down");
        let _ = quit_tx.send(window::WindowMessage::Exit);
    })
    .context("Failed to set Ctrl+C handler")?;
    
    #[cfg(feature = "tray")]
    ctrlc::set_handler(move || {
        info!("Received Ctrl+C, shutting down");
        quit_tx();
    })
    .context("Failed to set Ctrl+C handler")?;
    
    // Run the GTK main loop on the main thread
    gtk::main();
    
    // Send exit message to all threads
    let _ = window_sender.send(window::WindowMessage::Exit);
    #[cfg(feature = "tray")]
    let _ = tray_sender.send(tray::TrayMessage::Exit);
    
    // Join threads
    if let Err(e) = window_thread.join() {
        error!("Failed to join window thread: {:?}", e);
    }
    
    #[cfg(feature = "tray")]
    if let Err(e) = tray_thread.join() {
        error!("Failed to join tray thread: {:?}", e);
    }
    
    info!("Application shutdown complete");
    Ok(())
} 