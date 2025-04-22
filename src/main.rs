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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
    
    // トレイ機能がある場合とない場合で分岐
    #[cfg(feature = "tray")]
    let (window_thread, window_sender, tray_thread, tray_sender) = {
        info!("Starting tray application");
        let (tray_thread, tray_sender) = tray::run_tray_application(config.clone())?;
        info!("Tray application started");
        
        info!("Starting window application with tray");
        let (window_thread, window_sender) = window::run_window_application(config.clone(), tray_sender.clone())?;
        info!("Window application started");
        
        (window_thread, window_sender, tray_thread, tray_sender)
    };

    #[cfg(not(feature = "tray"))]
    let (window_thread, window_sender) = {
        info!("Starting window application");
        let result = window::run_window_application(config.clone())?;
        info!("Window application started");
        result
    };
    
    // Set up Ctrl+C handler - 確実に一度だけ終了メッセージを送信するためのフラグ
    let shutdown_initiated = Arc::new(AtomicBool::new(false));
    let shutdown_initiated_clone = shutdown_initiated.clone();
    
    let quit_tx = window_sender.clone();
    
    #[cfg(feature = "tray")]
    let tray_sender_clone = tray_sender.clone();
    
    ctrlc::set_handler(move || {
        // 既に終了処理が開始されていたら何もしない
        if shutdown_initiated.swap(true, Ordering::SeqCst) {
            return;
        }
        
        info!("Received Ctrl+C, shutting down");
        
        // 先にウィンドウを終了
        let _ = quit_tx.send(window::WindowMessage::Exit);
        
        // トレイは少し遅延させて終了
        #[cfg(feature = "tray")]
        {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let _ = tray_sender_clone.send(tray::TrayMessage::Exit);
        }
    })
    .context("Failed to set Ctrl+C handler")?;
    
    // Run the GTK main loop on the main thread
    gtk::main();
    
    // GTKのメインループが終了した後の処理
    info!("GTK main loop exited, cleaning up resources");
    
    // 既に終了処理が開始されていたら追加の終了メッセージを送信しない
    if !shutdown_initiated_clone.swap(true, Ordering::SeqCst) {
        // メインループが終了したら終了メッセージを送信
        let _ = window_sender.send(window::WindowMessage::Exit);
        
        #[cfg(feature = "tray")]
        {
            // トレイは少し遅延させて終了
            std::thread::sleep(std::time::Duration::from_millis(500));
            let _ = tray_sender.send(tray::TrayMessage::Exit);
        }
    }
    
    // スレッドの終了を待機
    info!("Waiting for threads to complete...");
    
    // スレッドの終了をタイムアウト付きで待機
    use std::time::Duration;
    let _timeout = Duration::from_secs(5);
    
    let window_handle = std::thread::spawn(move || {
        if let Err(e) = window_thread.join() {
            error!("Failed to join window thread: {:?}", e);
        }
    });
    
    #[cfg(feature = "tray")]
    let tray_handle = std::thread::spawn(move || {
        if let Err(e) = tray_thread.join() {
            error!("Failed to join tray thread: {:?}", e);
        }
    });
    
    // タイムアウト付きでウィンドウスレッドの終了を待機
    match window_handle.join() {
        Ok(_) => info!("Window thread joined successfully"),
        Err(e) => error!("Error joining window thread: {:?}", e),
    }
    
    #[cfg(feature = "tray")]
    {
        // タイムアウト付きでトレイスレッドの終了を待機
        match tray_handle.join() {
            Ok(_) => info!("Tray thread joined successfully"),
            Err(e) => error!("Error joining tray thread: {:?}", e),
        }
    }
    
    info!("Application shutdown complete");
    Ok(())
} 