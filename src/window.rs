use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::sync::mpsc::{self, Sender};
use gtk::{self, prelude::*};
use gtk::{Button, Label, Window, WindowType, Box as GtkBox, Orientation, ScrolledWindow, TextView, TextBuffer};
use gtk::{ComboBoxText, LevelBar, Frame, ToggleButton};
use glib;
use glib::ControlFlow;
use gdk;
use log::{info, error};
use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::config::Config;
use crate::audio::AudioRecorder;
use crate::api::TranscriptionAPI;
use crate::clipboard;
use crate::text_processor::TranscriptionProcessor;

#[cfg(feature = "tray")]
use crate::tray;

// Global static to hold the audio recorder between messages
static mut GLOBAL_RECORDER: Option<AudioRecorder> = None;
// Global flag for audio monitoring
static AUDIO_MONITORING: AtomicBool = AtomicBool::new(false);
// Global flag to track if shortcut key is currently pressed
static SHORTCUT_KEY_PRESSED: AtomicBool = AtomicBool::new(false);
// Global audio level for monitoring (shared between threads)
lazy_static::lazy_static! {
    static ref AUDIO_LEVEL: Arc<Mutex<f64>> = Arc::new(Mutex::new(0.0));
    static ref RECORDING_START_TIME: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    static ref BUTTON_UPDATE_TIMER_ID: Arc<Mutex<Option<glib::SourceId>>> = Arc::new(Mutex::new(None));
    static ref PROCESSING_STATUS_TIMER_ID: Arc<Mutex<Option<glib::SourceId>>> = Arc::new(Mutex::new(None));
    static ref PROCESSING_DOTS: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppStatus {
    Idle,
    Recording,
    Transcribing,
}

#[derive(Debug, Clone)]
pub enum WindowMessage {
    /// Exit the application
    Exit,
    /// Start recording
    StartRecording,
    /// Stop recording and process
    StopRecording,
    /// Show transcript
    ShowTranscript,
    /// Update UI with new status
    UpdateStatus(AppStatus),
    /// Update transcript text
    UpdateTranscript(String),
    /// Stop processing timer
    StopProcessingTimer,
}

/// Shared state that is thread-safe and can be sent between threads
struct ThreadSafeState {
    status: AppStatus,
    config: Config,
    transcript: String,
    api: TranscriptionAPI,
}

/// UI state that contains GTK widgets and cannot be sent between threads
struct UiState {
    state: Arc<Mutex<ThreadSafeState>>,
    tx_main: Sender<WindowMessage>,
    record_button: Button,
    transcript_buffer: TextBuffer,
    device_combo: ComboBoxText,
    audio_level: LevelBar,
    device_box: GtkBox,
    shortcut_frame: Frame,
    dict_frame: Frame,
    dict_buffer: TextBuffer,
    timer_label: Label,
}

impl ThreadSafeState {
    fn is_recording(&self) -> bool {
        self.status == AppStatus::Recording
    }
    
    fn start(&mut self) -> Result<()> {
        if self.is_recording() {
            return Ok(());
        }
        
        self.status = AppStatus::Recording;
        
        Ok(())
    }
    
    fn stop(&mut self) -> Result<Option<String>> {
        if !self.is_recording() {
            return Ok(None);
        }
        
        self.status = AppStatus::Transcribing;
        
        Ok(None) // This will be handled in the message handler
    }
    
    fn transcribe(&mut self, recording_path: &str) -> Result<String> {
        // 文字起こし処理と同時に整形まで行う
        let transcript = self.api.transcribe_with_processing(recording_path)?;
        
        // Always copy to clipboard regardless of auto_paste setting
        match clipboard::set_text(&transcript) {
            Ok(_) => info!("Auto-copied transcript to clipboard"),
            Err(e) => error!("Failed to copy to clipboard: {}", e),
        }
        
        Ok(transcript)
    }
}

/// Runs the window application and returns a join handle and a sender for communication
#[cfg(not(feature = "tray"))]
pub fn run_window_application(config: Config) -> Result<(JoinHandle<()>, Sender<WindowMessage>)> {
    run_window_application_internal(config)
}

#[cfg(feature = "tray")]
pub fn run_window_application(config: Config, _tray_sender: Sender<tray::TrayMessage>) -> Result<(JoinHandle<()>, Sender<WindowMessage>)> {
    run_window_application_internal(config)
}

// 内部実装（トレイ機能の有無に関わらず共通）
fn run_window_application_internal(config: Config) -> Result<(JoinHandle<()>, Sender<WindowMessage>)> {
    // Initialize GTK
    if gtk::init().is_err() {
        return Err(anyhow::anyhow!("Failed to initialize GTK."));
    }
    
    // Channel for communication with the main thread
    let (tx_main, rx_main) = mpsc::channel();
    
    // Create the main window
    let window = Window::new(WindowType::Toplevel);
    window.set_title("Wispr");
    window.set_default_size(400, 300);
    window.set_position(gtk::WindowPosition::Center);
    
    // Create UI components
    let main_box = GtkBox::new(Orientation::Vertical, 5);
    main_box.set_margin(5);
    
    // --- トグルボタン & レコードボタン セクション --- 
    let control_toggle_box = GtkBox::new(Orientation::Horizontal, 5);
    let device_toggle_button = ToggleButton::with_label("⚙"); // アイコンのみに
    let shortcut_toggle_button = ToggleButton::with_label("⌨"); // アイコンのみに
    let dict_toggle_button = ToggleButton::with_label("📚"); // 辞書トグルボタン追加
    let record_button = Button::with_label("● 録音"); // Recordボタンをここに移動し、ラベル変更
    
    control_toggle_box.pack_start(&device_toggle_button, false, false, 0);
    control_toggle_box.pack_start(&shortcut_toggle_button, false, false, 0);
    control_toggle_box.pack_start(&dict_toggle_button, false, false, 0); // 辞書ボタン追加
    control_toggle_box.pack_start(&record_button, true, true, 0); // Recordボタンを中央寄せに
    main_box.pack_start(&control_toggle_box, false, false, 0);
    // --- ここまで --- 
    
    // Audio device section
    let device_box = GtkBox::new(Orientation::Horizontal, 5);
    let device_label = Label::new(Some("Device:"));
    let device_combo = ComboBoxText::new();
    
    // Populate audio devices
    populate_audio_devices(&device_combo);
    
    device_box.pack_start(&device_label, false, false, 0);
    device_box.pack_start(&device_combo, true, true, 0);
    
    main_box.pack_start(&device_box, false, false, 0);
    
    // Audio level monitoring
    let level_box = GtkBox::new(Orientation::Horizontal, 5);
    let level_label = Label::new(Some("Level:"));
    let audio_level = LevelBar::new();
    audio_level.set_min_value(0.0);
    audio_level.set_max_value(1.0);
    
    level_box.pack_start(&level_label, false, false, 0);
    level_box.pack_start(&audio_level, true, true, 0);
    
    main_box.pack_start(&level_box, false, false, 0);
    
    // --- ショートカット情報 (復活) ---
    let shortcut_frame = Frame::new(None); // ラベルなし
    let shortcut_vbox = GtkBox::new(Orientation::Vertical, 2);
    shortcut_vbox.set_margin(5);
    let shortcut_label = Label::new(None);
    shortcut_label.set_markup(&format!(
        "<small>Record: <b>Press and hold {}</b>\nRelease to transcribe.\nClear: <b>{}</b>\nCopy: <b>{}</b></small>",
        config.shortcuts.toggle_recording,
        config.shortcuts.clear_transcript,
        config.shortcuts.copy_to_clipboard
    ));
    shortcut_label.set_halign(gtk::Align::Start);
    shortcut_vbox.pack_start(&shortcut_label, false, false, 0);
    shortcut_frame.add(&shortcut_vbox);
    main_box.pack_start(&shortcut_frame, false, false, 0);
    // --- ここまで ---

    // --- 辞書表示フレーム --- 
    let dict_frame = Frame::new(None);
    let dict_vbox = GtkBox::new(Orientation::Vertical, 5);
    dict_vbox.set_margin(5);

    // 辞書ヘッダー
    let dict_header_box = GtkBox::new(Orientation::Horizontal, 5);
    let dict_label = Label::new(Some("登録済み単語"));
    dict_label.set_halign(gtk::Align::Start);
    dict_label.set_hexpand(true);

    // 単語登録ボタン
    let add_word_button = Button::with_label("+ 単語登録");

    dict_header_box.pack_start(&dict_label, true, true, 0);
    dict_header_box.pack_start(&add_word_button, false, false, 0);
    dict_vbox.pack_start(&dict_header_box, false, false, 0);

    // 辞書リスト表示用スクロールウィンドウ
    let dict_scroll = ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    dict_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    dict_scroll.set_min_content_height(100);
    dict_scroll.set_max_content_height(150);

    // 辞書リスト表示用テキストビュー
    let dict_view = TextView::new();
    dict_view.set_editable(false);
    dict_view.set_cursor_visible(false);
    dict_view.set_wrap_mode(gtk::WrapMode::Word);
    let dict_buffer = dict_view.buffer().unwrap();
    dict_buffer.set_text("辞書が読み込まれていません...");

    dict_scroll.add(&dict_view);
    dict_vbox.pack_start(&dict_scroll, true, true, 0);
    dict_frame.add(&dict_vbox);
    main_box.pack_start(&dict_frame, false, false, 0);
    // --- ここまで ---
    
    // Transcript section
    let scrolled_window = ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    scrolled_window.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scrolled_window.set_vexpand(true);
    
    let transcript_view = TextView::new();
    transcript_view.set_editable(true);
    transcript_view.set_wrap_mode(gtk::WrapMode::Word);
    
    let transcript_buffer = transcript_view.buffer().unwrap();
    transcript_buffer.set_text("Record audio to see transcription here...");
    
    scrolled_window.add(&transcript_view);
    main_box.pack_start(&scrolled_window, true, true, 0);
    
    // Control buttons
    let control_box = GtkBox::new(Orientation::Horizontal, 5);
    
    // タイマーラベルをここに追加
    let timer_label = Label::new(Some("00:00"));
    timer_label.set_width_chars(14); // 幅を確保
    timer_label.set_halign(gtk::Align::Start);
    timer_label.set_margin_start(5);
    
    let copy_button = Button::with_label("Copy");
    let clear_button = Button::with_label("Clear");
    
    // タイマーはコントロールボックスの左側、残りのボタンは右側に
    control_box.pack_start(&timer_label, true, true, 0);
    control_box.pack_end(&clear_button, false, false, 0);
    control_box.pack_end(&copy_button, false, false, 0);
    
    main_box.pack_start(&control_box, false, false, 0);
    
    // Add everything to the window
    window.add(&main_box);
    window.show_all();
    
    // Set up thread-safe state
    let thread_safe_state = Arc::new(Mutex::new(ThreadSafeState {
        status: AppStatus::Idle,
        config: config.clone(),
        transcript: String::new(),
        api: TranscriptionAPI::new(config.clone()),
    }));
    
    // Set up UI state
    let ui_state = UiState {
        state: thread_safe_state.clone(),
        tx_main: tx_main.clone(),
        record_button: record_button.clone(),
        transcript_buffer: transcript_buffer.clone(),
        device_combo: device_combo.clone(),
        audio_level: audio_level.clone(),
        device_box: device_box.clone(),
        shortcut_frame: shortcut_frame.clone(),
        dict_frame: dict_frame.clone(),
        dict_buffer: dict_buffer.clone(),
        timer_label: timer_label.clone(),
    };
    
    // --- トグルボタンの初期状態と接続 ---
    device_box.set_visible(false);
    shortcut_frame.set_visible(false);
    dict_frame.set_visible(false);
    device_toggle_button.set_active(false);
    shortcut_toggle_button.set_active(false);
    dict_toggle_button.set_active(false);

    let device_box_clone = device_box.clone();
    device_toggle_button.connect_toggled(move |btn| {
        device_box_clone.set_visible(btn.is_active());
    });

    let shortcut_frame_clone = shortcut_frame.clone();
    shortcut_toggle_button.connect_toggled(move |btn| {
        shortcut_frame_clone.set_visible(btn.is_active());
    });

    let dict_frame_clone = dict_frame.clone();
    let thread_safe_state_clone = thread_safe_state.clone();
    let dict_buffer_clone = dict_buffer.clone();
    dict_toggle_button.connect_toggled(move |btn| {
        dict_frame_clone.set_visible(btn.is_active());
        
        // 辞書ボタンをアクティブにしたときに辞書内容を更新
        if btn.is_active() {
            let config_clone = thread_safe_state_clone.lock().unwrap().config.clone();
            update_dictionary_view(&dict_buffer_clone, &config_clone);
        }
    });
    // --- ここまで ---
    
    // Set up Ctrl+C handler
    #[cfg(not(feature = "tray"))]
    let _quit_tx = tx_main.clone();
    
    #[cfg(feature = "tray")]
    let _quit_tx = tx_main.clone();

    // Connect window close event
    let tx_clone = tx_main.clone();
    window.connect_delete_event(move |_, _| {
        let _ = tx_clone.send(WindowMessage::Exit);
        AUDIO_MONITORING.store(false, Ordering::SeqCst);
        gtk::main_quit();
        glib::Propagation::Stop
    });
    
    // Connect record button
    let _tx_clone = tx_main.clone();
    let state_clone = thread_safe_state.clone();
    record_button.connect_clicked(move |_| {
        let status = state_clone.lock().unwrap().status;
        match status {
            AppStatus::Idle => {
                let _ = _tx_clone.send(WindowMessage::StartRecording);
            },
            AppStatus::Recording => {
                let _ = _tx_clone.send(WindowMessage::StopRecording);
            },
            AppStatus::Transcribing => {
                // Do nothing during transcription
            }
        }
    });
    
    // Connect device combo box
    let tx_clone = tx_main.clone();
    device_combo.connect_changed(move |combo| {
        if let Some(device_id) = combo.active_text() {
            info!("Selected audio device: {}", device_id);
            // You would store this selection for use in audio recording
        }
    });
    
    // Connect copy button
    let state_clone = thread_safe_state.clone();
    copy_button.connect_clicked(move |_| {
        let state = state_clone.lock().unwrap();
        if !state.transcript.is_empty() {
            match clipboard::set_text(&state.transcript) {
                Ok(_) => {
                    info!("Transcript copied to clipboard");
                },
                Err(e) => {
                    error!("Failed to copy to clipboard: {}", e);
                }
            }
        }
    });
    
    // Connect clear button
    let state_clone = thread_safe_state.clone();
    let transcript_buffer_clone = transcript_buffer.clone();
    clear_button.connect_clicked(move |_| {
        let mut state = state_clone.lock().unwrap();
        state.transcript = String::new();
        update_transcript_text(&transcript_buffer_clone, "");
    });
    
    // Add simplified keyboard shortcuts
    setup_keyboard_shortcuts(&window, &config, tx_main.clone());
    
    // Set up a timer to check for messages
    let ui_state_arc = Arc::new(Mutex::new(ui_state));
    let ui_state_arc_clone = ui_state_arc.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        process_messages(&rx_main, &ui_state_arc_clone)
    });
    
    // Set up a timer to update the audio level bar
    let audio_level_clone = audio_level.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        if let Ok(level) = AUDIO_LEVEL.lock() {
            audio_level_clone.set_value(*level);
        }
        ControlFlow::Continue
    });
    
    // Set up a timer to update the timer label during recording
    let ui_state_arc_for_timer = ui_state_arc.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(1000), move || {
        let ui_state = ui_state_arc_for_timer.lock().unwrap();
        let state = ui_state.state.lock().unwrap();
        
        if state.status == AppStatus::Recording {
            if let Ok(mut start_time) = RECORDING_START_TIME.lock() {
                // 開始時間がなければ現在時刻を設定
                if start_time.is_none() {
                    *start_time = Some(Instant::now());
                }
                
                // 経過時間を計算
                if let Some(start) = *start_time {
                    let elapsed = start.elapsed();
                    let seconds = elapsed.as_secs();
                    let minutes = seconds / 60;
                    let remaining_seconds = seconds % 60;
                    
                    // 残り時間も計算（設定した最大時間から）
                    let max_duration = state.config.recording.max_duration_secs;
                    let remaining = if seconds < max_duration {
                        max_duration - seconds
                    } else {
                        0
                    };
                    let remaining_minutes = remaining / 60;
                    let remaining_seconds_mod = remaining % 60;
                    
                    // タイマーテキスト更新
                    ui_state.timer_label.set_text(&format!(
                        "{:02}:{:02} / {:02}:{:02}",
                        minutes, remaining_seconds,
                        remaining_minutes, remaining_seconds_mod
                    ));
                }
            }
        } else {
            // 録音していないときはタイマーをリセット
            if let Ok(mut start_time) = RECORDING_START_TIME.lock() {
                *start_time = None;
            }
            ui_state.timer_label.set_text("00:00");
        }
        
        ControlFlow::Continue
    });
    
    // Start audio level monitoring using a separate thread
    AUDIO_MONITORING.store(true, Ordering::SeqCst);
    thread::spawn(move || {
        monitor_audio_input();
    });
    
    // Create a thread that will be joined when the application exits
    let handler_thread = thread::spawn(move || {
        // Just a placeholder thread that does nothing but can be joined
        info!("Handler thread started");
        
        // Sleep until application exit
        loop {
            std::thread::sleep(std::time::Duration::from_secs(10));
        }
    });
    
    Ok((handler_thread, tx_main))
}

/// Process incoming messages from the UI and other threads
fn process_messages(rx: &mpsc::Receiver<WindowMessage>, ui_state_arc: &Arc<Mutex<UiState>>) -> ControlFlow {
    // Try to receive a message without blocking
    match rx.try_recv() {
        Ok(message) => {
            let ui_state = ui_state_arc.lock().unwrap();
            let state_arc = ui_state.state.clone();
            
            match message {
                WindowMessage::Exit => {
                    info!("Exiting window application");
                    // Ensure we stop recording if active
                    if let Ok(mut state) = state_arc.lock() {
                        if state.is_recording() {
                            let _ = state.stop();
                        }
                    }
                    // 確実にオーディオモニタリングを停止
                    AUDIO_MONITORING.store(false, Ordering::SeqCst);
                    
                    // 既存のタイマーが存在すれば削除
                    if let Ok(mut timer_id) = BUTTON_UPDATE_TIMER_ID.lock() {
                        if let Some(id) = timer_id.take() {
                            // source_removeではなく、SourceIdのメソッドを使用
                            id.remove();
                        }
                    }
                    
                    // Exit the application
                    gtk::main_quit();
                    return ControlFlow::Break;
                },
                WindowMessage::StartRecording => {
                    info!("Starting recording");
                    update_ui_status(&ui_state, AppStatus::Recording);
                    
                    // Get selected device
                    let selected_device = ui_state.device_combo.active_text()
                        .map(|text| {
                            info!("Using selected audio device: {}", text);
                            if text.contains("(Default)") {
                                None // Use default device
                            } else {
                                Some(text.to_string())
                            }
                        })
                        .unwrap_or(None);
                    
                    // Create and start a new recorder
                    let mut recorder = AudioRecorder::new(state_arc.lock().unwrap().config.clone());
                    
                    if let Ok(mut state) = state_arc.lock() {
                        match state.start() {
                            Ok(_) => {
                                match recorder.start_with_device(selected_device) {
                                    Ok(_) => {
                                        info!("Recording started successfully");
                                        
                                        // Store recorder in global static
                                        unsafe {
                                            GLOBAL_RECORDER = Some(recorder);
                                        }
                                        
                                        // Spawn a new thread to wait for stop signal
                                        let tx_clone = ui_state.tx_main.clone();
                                        let max_duration = state.config.recording.max_duration_secs;
                                        std::thread::spawn(move || {
                                            // Wait for maximum recording duration
                                            std::thread::sleep(std::time::Duration::from_secs(max_duration));
                                            
                                            // Send signal to stop recording after timeout
                                            info!("Sending auto-stop signal after {} seconds", max_duration);
                                            let _ = tx_clone.send(WindowMessage::StopRecording);
                                        });
                                    },
                                    Err(e) => {
                                        error!("Failed to start recording: {}", e);
                                        update_ui_status(&ui_state, AppStatus::Idle);
                                    }
                                }
                            },
                            Err(e) => {
                                error!("Failed to update state: {}", e);
                                update_ui_status(&ui_state, AppStatus::Idle);
                            }
                        }
                    }
                },
                WindowMessage::StopRecording => {
                    info!("Stopping recording");
                    update_ui_status(&ui_state, AppStatus::Transcribing);
                    
                    // Get recording path from the global recorder
                    let recording_path = unsafe {
                        if let Some(mut recorder) = GLOBAL_RECORDER.take() {
                            match recorder.stop() {
                                Ok(Some(path)) => {
                                    info!("Recording stopped, saved to {}", path);
                                    Some(path)
                                },
                                Ok(None) => {
                                    info!("No recording to stop");
                                    None
                                },
                                Err(e) => {
                                    error!("Failed to stop recording: {}", e);
                                    None
                                }
                            }
                        } else {
                            info!("No recorder found");
                            None
                        }
                    };
                    
                    // Update application state
                    if let Ok(mut state) = state_arc.lock() {
                        match state.stop() {
                            Ok(_) => {
                                // Process transcription if we have a recording path
                                if let Some(path) = recording_path.clone() {
                                    // スレッドを分離してトランスクリプション処理を行う
                                    let tx_clone = ui_state.tx_main.clone();
                                    let state_clone = state_arc.clone();
                                    
                                    // 処理中のインジケーターを更新するタイマー
                                    setup_processing_status_timer(&ui_state);
                                    
                                    // トランスクリプション処理用スレッド
                                    std::thread::spawn(move || {
                                        info!("Starting transcription in background thread");
                                        let result = if let Ok(mut state) = state_clone.lock() {
                                            state.transcribe(&path)
                                        } else {
                                            Err(anyhow::anyhow!("Could not lock state for transcription"))
                                        };
                                        
                                        // 処理完了後、結果をメインスレッドに送信
                                        match result {
                                            Ok(transcript) => {
                                                info!("Transcription complete, sending result to main thread");
                                                let _ = tx_clone.send(WindowMessage::UpdateTranscript(transcript));
                                            },
                                            Err(e) => {
                                                error!("Transcription error: {}", e);
                                                let error_text = format!("Error: {}", e);
                                                let _ = tx_clone.send(WindowMessage::UpdateTranscript(error_text));
                                            }
                                        }
                                        
                                        // 処理完了後、ステータスをIdleに戻す
                                        let _ = tx_clone.send(WindowMessage::UpdateStatus(AppStatus::Idle));
                                        
                                        // 処理中タイマーを停止 - glib_idle_add_localは使わず、タイマーIDを送信
                                        let _ = tx_clone.send(WindowMessage::StopProcessingTimer);
                                    });
                                    
                                    // メインスレッドはブロックせず即座に戻る
                                    return ControlFlow::Continue;
                                } else {
                                    // 録音ファイルがない場合はすぐにIdleに戻す
                                    state.status = AppStatus::Idle;
                                    update_ui_status(&ui_state, AppStatus::Idle);
                                }
                            },
                            Err(e) => {
                                error!("Failed to update state: {}", e);
                                // Ensure the UI is set back to Idle state even if there was an error
                                state.status = AppStatus::Idle;
                                update_ui_status(&ui_state, AppStatus::Idle);
                            }
                        }
                    } else {
                        // If we can't get state lock, still update the UI to allow re-recording
                        update_ui_status(&ui_state, AppStatus::Idle);
                    }
                },
                WindowMessage::ShowTranscript => {
                    // Nothing to do - transcript is already visible in the window
                },
                WindowMessage::UpdateStatus(status) => {
                    update_ui_status(&ui_state, status);
                    if let Ok(mut state) = state_arc.lock() {
                        state.status = status;
                    }
                },
                WindowMessage::UpdateTranscript(text) => {
                    if let Ok(mut state) = state_arc.lock() {
                        state.transcript = text.clone();
                    }
                    update_transcript_text(&ui_state.transcript_buffer, &text);
                },
                WindowMessage::StopProcessingTimer => {
                    // 処理中タイマーを停止
                    if let Ok(mut timer_id) = PROCESSING_STATUS_TIMER_ID.lock() {
                        if let Some(id) = timer_id.take() {
                            id.remove();
                        }
                    }
                },
            }
        },
        Err(mpsc::TryRecvError::Empty) => {
            // No message, continue
        },
        Err(mpsc::TryRecvError::Disconnected) => {
            error!("Message channel disconnected");
            // Exit the application
            gtk::main_quit();
            return ControlFlow::Break;
        }
    }
    
    ControlFlow::Continue
}

/// Add simplified keyboard shortcuts
fn setup_keyboard_shortcuts(window: &Window, config: &Config, tx: Sender<WindowMessage>) {
    // For recording - handle key press event
    let tx_clone = tx.clone();
    let key = config.shortcuts.toggle_recording.clone();
    window.connect_key_press_event(move |_, event| {
        if is_shortcut_key(event, &key) && !SHORTCUT_KEY_PRESSED.load(Ordering::SeqCst) {
            info!("Shortcut key pressed - starting recording");
            SHORTCUT_KEY_PRESSED.store(true, Ordering::SeqCst);
            let _ = tx_clone.send(WindowMessage::StartRecording);
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    
    // For recording - handle key release event
    let tx_clone = tx.clone();
    let key = config.shortcuts.toggle_recording.clone();
    window.connect_key_release_event(move |_, event| {
        if is_shortcut_key(event, &key) && SHORTCUT_KEY_PRESSED.load(Ordering::SeqCst) {
            info!("Shortcut key released - stopping recording and transcribing");
            SHORTCUT_KEY_PRESSED.store(false, Ordering::SeqCst);
            let _ = tx_clone.send(WindowMessage::StopRecording);
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    
    // For clearing transcript
    let tx_clone = tx.clone();
    let key = config.shortcuts.clear_transcript.clone();
    window.connect_key_press_event(move |_, event| {
        if is_shortcut_key(event, &key) {
            let _ = tx_clone.send(WindowMessage::UpdateTranscript(String::new()));
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    
    // For copying to clipboard
    let tx_clone = tx.clone();
    let key = config.shortcuts.copy_to_clipboard.clone();
    window.connect_key_press_event(move |_, event| {
        if is_shortcut_key(event, &key) {
            let _ = tx_clone.send(WindowMessage::ShowTranscript);
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    
    info!("Keyboard shortcuts configured");
}

/// Check if a key event matches a shortcut string like "Alt+Shift+R"
fn is_shortcut_key(event: &gdk::EventKey, shortcut: &str) -> bool {
    let parts: Vec<&str> = shortcut.split('+').collect();
    let key_part = parts.last().unwrap().to_lowercase();
    
    // Check if the key matches
    let key_matches = match key_part.as_str() {
        "r" => event.keyval() == gdk::keys::constants::r,
        "c" => event.keyval() == gdk::keys::constants::c,
        "x" => event.keyval() == gdk::keys::constants::x,
        "f1" => event.keyval() == gdk::keys::constants::F1,
        "f2" => event.keyval() == gdk::keys::constants::F2,
        "space" => event.keyval() == gdk::keys::constants::space,
        // ... add more key mappings as needed
        _ => {
            // Try to match a single character key directly
            if key_part.len() == 1 {
                let c = key_part.chars().next().unwrap();
                let keyval = event.keyval();
                
                let low_char = c.to_lowercase().next().unwrap();
                let key_code = match low_char {
                    'a' => gdk::keys::constants::a,
                    'b' => gdk::keys::constants::b,
                    'd' => gdk::keys::constants::d,
                    'e' => gdk::keys::constants::e,
                    'f' => gdk::keys::constants::f,
                    'g' => gdk::keys::constants::g,
                    'h' => gdk::keys::constants::h,
                    'i' => gdk::keys::constants::i,
                    'j' => gdk::keys::constants::j,
                    'k' => gdk::keys::constants::k,
                    'l' => gdk::keys::constants::l,
                    'm' => gdk::keys::constants::m,
                    'n' => gdk::keys::constants::n,
                    'o' => gdk::keys::constants::o,
                    'p' => gdk::keys::constants::p,
                    'q' => gdk::keys::constants::q,
                    'r' => gdk::keys::constants::r,
                    's' => gdk::keys::constants::s,
                    't' => gdk::keys::constants::t,
                    'u' => gdk::keys::constants::u,
                    'v' => gdk::keys::constants::v,
                    'w' => gdk::keys::constants::w,
                    'x' => gdk::keys::constants::x,
                    'y' => gdk::keys::constants::y,
                    'z' => gdk::keys::constants::z,
                    _ => keyval, // If not found, use the keyval from the event
                };
                
                keyval == key_code
            } else {
                false
            }
        }
    };
    
    // Check modifiers
    let shift_needed = parts.contains(&"Shift");
    let alt_needed = parts.contains(&"Alt");
    let ctrl_needed = parts.contains(&"Control") || parts.contains(&"Ctrl");
    
    let state = event.state();
    let shift_pressed = state.contains(gdk::ModifierType::SHIFT_MASK);
    let alt_pressed = state.contains(gdk::ModifierType::MOD1_MASK);
    let ctrl_pressed = state.contains(gdk::ModifierType::CONTROL_MASK);
    
    key_matches && 
        shift_pressed == shift_needed && 
        alt_pressed == alt_needed && 
        ctrl_pressed == ctrl_needed
}

/// Update the UI status (button and label)
fn update_ui_status(ui_state: &UiState, status: AppStatus) {
    // 既存のタイマーがあれば削除
    if let Ok(mut timer_id) = BUTTON_UPDATE_TIMER_ID.lock() {
        if let Some(id) = timer_id.take() {
            // source_removeではなく、SourceIdのメソッドを使用
            id.remove();
        }
    }

    match status {
        AppStatus::Idle => {
            ui_state.record_button.set_label("● 録音");
            ui_state.record_button.set_sensitive(true);
            // タイマーをリセット
            if let Ok(mut start_time) = RECORDING_START_TIME.lock() {
                *start_time = None;
            }
            ui_state.timer_label.set_text("00:00");
        },
        AppStatus::Recording => {
            // 録音ボタンラベルを簡素化 - 時間表示を削除
            ui_state.record_button.set_label("■ 停止");
            ui_state.record_button.set_sensitive(true);
            // タイマー開始時間を設定
            if let Ok(mut start_time) = RECORDING_START_TIME.lock() {
                *start_time = Some(Instant::now());
            }
            
            // タイマーラベルの更新だけを行い、録音ボタンのラベル更新は不要に
            let timer_label_clone = ui_state.timer_label.clone();
            let state_arc = ui_state.state.clone();
            let timer_id = glib::timeout_add_local(std::time::Duration::from_millis(1000), move || {
                if let Ok(state) = state_arc.lock() {
                    if state.status != AppStatus::Recording {
                        return ControlFlow::Break;
                    }
                    
                    if let Ok(start_time) = RECORDING_START_TIME.lock() {
                        if let Some(start) = *start_time {
                            let elapsed = start.elapsed();
                            let seconds = elapsed.as_secs();
                            let minutes = seconds / 60;
                            let remaining_seconds = seconds % 60;
                            
                            // 残り時間も計算（設定した最大時間から）
                            let max_duration = state.config.recording.max_duration_secs;
                            let remaining = if seconds < max_duration {
                                max_duration - seconds
                            } else {
                                0
                            };
                            let remaining_minutes = remaining / 60;
                            let remaining_seconds_mod = remaining % 60;
                            
                            // タイマーテキスト更新
                            timer_label_clone.set_text(&format!(
                                "{:02}:{:02} / {:02}:{:02}",
                                minutes, remaining_seconds,
                                remaining_minutes, remaining_seconds_mod
                            ));
                        }
                    }
                }
                ControlFlow::Continue
            });
            
            // タイマーIDを保存
            if let Ok(mut timer_id_guard) = BUTTON_UPDATE_TIMER_ID.lock() {
                *timer_id_guard = Some(timer_id);
            }
        },
        AppStatus::Transcribing => {
            ui_state.record_button.set_label("処理中...");
            ui_state.record_button.set_sensitive(false);
            // タイマーをリセット
            if let Ok(mut start_time) = RECORDING_START_TIME.lock() {
                *start_time = None;
            }
            ui_state.timer_label.set_text("処理中...");
        }
    }
}

/// Update the transcript text in the UI
fn update_transcript_text(buffer: &TextBuffer, text: &str) {
    // 改行を保持して表示
    buffer.set_text(text);
    
    // テキストビューにスクロールして表示を更新
    buffer.emit_by_name::<()>("changed", &[]);
}

/// Populate the device combo box with available audio devices
fn populate_audio_devices(combo: &ComboBoxText) {
    let host = cpal::default_host();
    
    // Get default device first
    if let Some(default_device) = host.default_input_device() {
        if let Ok(name) = default_device.name() {
            combo.append(Some("default"), &format!("{} (Default)", name));
            combo.set_active_id(Some("default"));
        }
    }
    
    // Add all other input devices
    if let Ok(devices) = host.input_devices() {
        for (idx, device) in devices.enumerate() {
            if let Ok(name) = device.name() {
                let id = format!("device_{}", idx);
                combo.append(Some(&id), &name);
            }
        }
    }
}

/// Start monitoring audio input levels in a separate thread
fn monitor_audio_input() {
    // We need to create a temporary input stream to monitor audio levels
    if let Ok(devices) = cpal::default_host().input_devices() {
        for device in devices {
            if let Ok(config) = device.default_input_config() {
                info!("Setting up audio monitoring");
                
                // Try to build a stream for monitoring
                let stream_result = match config.sample_format() {
                    cpal::SampleFormat::F32 => {
                        let audio_level = AUDIO_LEVEL.clone();
                        device.build_input_stream(
                            &config.into(),
                            move |data: &[f32], _: &_| {
                                if AUDIO_MONITORING.load(Ordering::SeqCst) {
                                    // Calculate RMS of the audio samples
                                    let sum: f32 = data.iter()
                                        .map(|&sample| sample * sample)
                                        .sum();
                                    let rms = (sum / data.len() as f32).sqrt();
                                    
                                    // Update shared audio level (scale RMS to 0.0-1.0 range)
                                    // Use non-linear scaling to make the meter more useful
                                    let level = (rms * 5.0).min(1.0) as f64;
                                    if let Ok(mut level_guard) = audio_level.lock() {
                                        *level_guard = level;
                                    }
                                }
                            },
                            |err| error!("Error in audio monitoring: {}", err),
                            None,
                        )
                    },
                    _ => {
                        error!("Unsupported sample format for audio monitoring");
                        Err(cpal::BuildStreamError::DeviceNotAvailable)
                    }
                };
                
                // Start the stream if successful
                if let Ok(stream) = stream_result {
                    if let Err(e) = stream.play() {
                        error!("Could not play stream for audio monitoring: {}", e);
                        continue;
                    }
                    
                    // より短い間隔でフラグをチェックして、すぐに反応できるようにする
                    while AUDIO_MONITORING.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    
                    // ストリームを明示的に停止して解放
                    drop(stream);
                    info!("Audio monitoring stopped and resources released");
                    
                    return; // Exit after setting up monitoring with the first working device
                }
            }
        }
    }
    
    error!("Failed to set up audio monitoring");
}

/// 辞書内容を表示用テキストビューに更新する
fn update_dictionary_view(buffer: &TextBuffer, config: &Config) {
    let dict_path = config.temp_dir.join("user_dictionary.json");
    
    // 辞書内容の文字列を構築
    let mut content = String::new();
    
    // UserDictionaryのプライベートフィールドにアクセスする代わりに
    // ファイルを直接読み込んでJSONをパースする
    if let Ok(file) = std::fs::File::open(&dict_path) {
        if let Ok(json) = serde_json::from_reader(std::io::BufReader::new(file)) {
            let dict: serde_json::Value = json;
            
            if let Some(words) = dict.get("words") {
                if let Some(words_obj) = words.as_object() {
                    if words_obj.is_empty() {
                        content = "登録されている単語はありません".to_string();
                    } else {
                        for (original, replacement) in words_obj {
                            if let Some(replacement_str) = replacement.as_str() {
                                content.push_str(&format!("「{}」→「{}」\n", original, replacement_str));
                            }
                        }
                    }
                }
            }
        }
    }
    
    if content.is_empty() {
        content = "辞書の読み込みに失敗しました".to_string();
    }
    
    buffer.set_text(&content);
}

/// トランスクリプション処理中のステータス表示を更新するタイマーをセットアップ
fn setup_processing_status_timer(ui_state: &UiState) {
    // 既存のタイマーがあれば削除
    if let Ok(mut timer_id) = PROCESSING_STATUS_TIMER_ID.lock() {
        if let Some(id) = timer_id.take() {
            id.remove();
        }
    }
    
    // ドット数をリセット
    if let Ok(mut dots) = PROCESSING_DOTS.lock() {
        *dots = 0;
    }
    
    // 処理中を示すアニメーションを表示
    let record_button_clone = ui_state.record_button.clone();
    let timer_label_clone = ui_state.timer_label.clone();
    
    let timer_id = glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
        if let Ok(mut dots) = PROCESSING_DOTS.lock() {
            *dots = (*dots + 1) % 4;
            let dots_str = ".".repeat(*dots);
            let padding = " ".repeat(3 - *dots);
            
            // ボタンとタイマーラベルのテキストを更新
            record_button_clone.set_label(&format!("処理中{}{}", dots_str, padding));
            timer_label_clone.set_text(&format!("処理中{}{}", dots_str, padding));
        }
        
        ControlFlow::Continue
    });
    
    // タイマーIDを保存
    if let Ok(mut timer_id_guard) = PROCESSING_STATUS_TIMER_ID.lock() {
        *timer_id_guard = Some(timer_id);
    }
} 