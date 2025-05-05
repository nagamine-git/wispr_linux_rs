// use std::env;
// use std::process;
// use std::thread::JoinHandle;
// use std::sync::mpsc::{self, Sender};
// use std::path::PathBuf;
// use std::fs;

// use std::thread;

use anyhow::{Result, Context};
use log::{info, error, LevelFilter};
use gtk;
use clap::Parser;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::path::Path;
use log4rs::append::rolling_file::policy::compound::trigger::size::SizeTrigger;
use log4rs::append::rolling_file::policy::compound::roll::fixed_window::FixedWindowRoller;

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

    // Initialize logger with log4rs
    let config_path = Path::new("log4rs.yaml");
    if config_path.exists() {
        log4rs::init_file(config_path, Default::default())
            .context("Failed to initialize logger from config file")?;
    } else {
        // ホームディレクトリにログディレクトリを作成
        let home_dir = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
        let log_dir = home_dir.join(".local/log");
        std::fs::create_dir_all(&log_dir).context("Failed to create log directory")?;
        
        // 設定ファイルが存在しない場合は、プログラム内で設定
        let log_file = log_dir.join("wispr.log");
        
        // ファイルアペンダー設定
        let file_appender = log4rs::append::rolling_file::RollingFileAppender::builder()
            .encoder(Box::new(log4rs::encode::pattern::PatternEncoder::new("{d(%Y-%m-%d %H:%M:%S)} {h({l})} {t} - {m}{n}")))
            .build(log_file, Box::new(log4rs::append::rolling_file::policy::compound::CompoundPolicy::new(
                Box::new(SizeTrigger::new(10 * 1024 * 1024)), // 10MB
                Box::new(FixedWindowRoller::builder()
                    .build(&log_dir.join("wispr.{}.log.gz").to_string_lossy(), 5)
                    .context("Failed to build roller")?)
            )))
            .context("Failed to build file appender")?;
            
        // コンソールアペンダー設定
        let console_appender = log4rs::append::console::ConsoleAppender::builder()
            .encoder(Box::new(log4rs::encode::pattern::PatternEncoder::new("{d(%Y-%m-%d %H:%M:%S)} {h({l})} {t} - {m}{n}")))
            .build();
            
        // ロガー設定
        let config = log4rs::Config::builder()
            .appender(log4rs::config::Appender::builder().build("file", Box::new(file_appender)))
            .appender(log4rs::config::Appender::builder().build("console", Box::new(console_appender)))
            .build(log4rs::config::Root::builder()
                .appender("file")
                .appender("console")
                .build(LevelFilter::Info))
            .context("Failed to build log config")?;
            
        log4rs::init_config(config).context("Failed to initialize logger from built config")?;
    }

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