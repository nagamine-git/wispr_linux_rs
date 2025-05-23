use anyhow::{Result, Context};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SizedSample;
use log::{info, error, warn};
use std::fs::File;
use std::io::BufWriter;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering, AtomicU64};
use std::time::{Duration, Instant};
use std::marker::PhantomData;

use crate::config::Config;

/// Audio recorder that handles microphone capture
pub struct AudioRecorder {
    config: Config,
    recording: Arc<AtomicBool>,
    output_file: Option<String>,
    start_time: Option<Instant>,
    stream: Option<StreamWrapper>,
    last_active: Arc<AtomicU64>, // 録音アクティビティの最終時刻
    _marker: PhantomData<*const ()>, // Add a PhantomData to opt out of Send/Sync
}

// Implement Send/Sync for the wrapper since we're ensuring it's used safely
// This is safe because we control all thread interactions with AudioRecorder
unsafe impl Send for AudioRecorder {}
unsafe impl Sync for AudioRecorder {}

// Stream wrapper to encapsulate the cpal stream
struct StreamWrapper {
    _stream: cpal::Stream,
}

impl StreamWrapper {
    fn new(stream: cpal::Stream) -> Self {
        Self { _stream: stream }
    }
}

impl AudioRecorder {
    /// Create a new audio recorder
    pub fn new(config: Config) -> Self {
        Self {
            config,
            recording: Arc::new(AtomicBool::new(false)),
            output_file: None,
            start_time: None,
            stream: None,
            last_active: Arc::new(AtomicU64::new(0)),
            _marker: PhantomData,
        }
    }
    
    /// Start recording with a specific device
    pub fn start_with_device(&mut self, device_name: Option<String>) -> Result<()> {
        if self.recording.load(Ordering::SeqCst) {
            return Ok(());
        }
        
        // Create output file path
        let output_file = format!("{}/recording_{}.wav", 
            self.config.temp_dir.display(),
            chrono::Local::now().format("%Y%m%d_%H%M%S"));
        
        // Set output file and recording flag
        self.output_file = Some(output_file.clone());
        self.recording.store(true, Ordering::SeqCst);
        self.start_time = Some(Instant::now());
        
        // 録音開始時の時刻を記録
        self.last_active.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            Ordering::SeqCst
        );
        
        info!("Starting audio recording to {}", output_file);
        
        // Ensure the output directory exists
        let dir = std::path::Path::new(&self.config.temp_dir);
        if !dir.exists() {
            info!("Creating output directory: {}", dir.display());
            std::fs::create_dir_all(dir)
                .context("Failed to create output directory")?;
        }
        
        // Get host and determine input device
        let host = cpal::default_host();
        let device = if let Some(name) = device_name {
            // Try to find the specified device
            let mut found_device = None;
            if let Ok(devices) = host.input_devices() {
                for device in devices {
                    if let Ok(device_name) = device.name() {
                        if device_name == name {
                            found_device = Some(device);
                            break;
                        }
                    }
                }
            }
            
            // Fall back to default if not found
            match found_device {
                Some(d) => {
                    info!("Using selected input device: {}", name);
                    d
                },
                None => {
                    warn!("Device {} not found, using default", name);
                    host.default_input_device()
                        .context("No input device found")?
                }
            }
        } else {
            // Use default device
            host.default_input_device()
                .context("No input device found")?
        };
            
        info!("Using input device: {}", device.name()?);
        
        // Get default config
        let default_config = device.default_input_config()
            .context("Failed to get default input config")?;
            
        // Debug info
        info!("Default config: {:?}", default_config);
        let sample_format = default_config.sample_format();
        info!("Sample format: {:?}", sample_format);
        info!("Channels: {}", default_config.channels());
        info!("Sample rate: {}", default_config.sample_rate().0);
        
        // Create stream config from default settings
        let mut config: cpal::StreamConfig = default_config.into();
        
        // 追加: 設定ファイルのサンプルレートを適用
        if self.config.recording.sample_rate > 0 {
            info!("Overriding sample rate with user setting: {} Hz", self.config.recording.sample_rate);
            config.sample_rate = cpal::SampleRate(self.config.recording.sample_rate);
        }
        
        // 汎用的で堅牢なバッファリング設定
        // システムとデバイスの特性を考慮して自動的に適切なバッファサイズを選択
        info!("Using system-selected optimal buffer size for maximum compatibility");
        config.buffer_size = cpal::BufferSize::Default;
        
        // Open output file
        let spec = hound::WavSpec {
            channels: config.channels,
            sample_rate: config.sample_rate.0,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        
        info!("Creating WAV file with spec: {:?}", spec);
        
        let output_file_arc = Arc::new(Mutex::new(
            Some(hound::WavWriter::create(&output_file, spec)
                .context("Failed to create WAV file")?)
        ));
        
        // Clone Atomic bool for capture thread
        let recording = self.recording.clone();
        let last_active = self.last_active.clone();
        
        // Create and start the stream
        let err_fn = move |err| {
            error!("Audio error: {}", err);
        };
        
        // Set up the input stream based on the device's sample format
        let stream = match sample_format {
            cpal::SampleFormat::I16 => self.setup_stream::<i16>(&device, &config, err_fn, output_file_arc.clone(), recording.clone()),
            cpal::SampleFormat::F32 => self.setup_stream::<f32>(&device, &config, err_fn, output_file_arc.clone(), recording.clone()),
            cpal::SampleFormat::U16 => return Err(anyhow::anyhow!("Unsupported sample format: U16")),
            _ => return Err(anyhow::anyhow!("Unknown sample format")),
        }?;
        
        // Save stream and start it
        info!("Playing audio stream");
        stream.play().context("Failed to start audio stream")?;
        self.stream = Some(StreamWrapper::new(stream));
        
        // Spawn a thread to stop recording after max duration
        let max_duration = self.config.recording.max_duration_secs;
        let recording_clone = self.recording.clone();
        let last_active_clone = self.last_active.clone();
        let disable_silence_detection = self.config.recording.disable_silence_detection;
        
        std::thread::spawn(move || {
            // 一定間隔でチェックを行う（10秒ごと）
            let check_interval = Duration::from_secs(10);
            let mut elapsed = Duration::from_secs(0);
            
            while recording_clone.load(Ordering::SeqCst) && elapsed.as_secs() < max_duration {
                std::thread::sleep(check_interval);
                elapsed += check_interval;
                
                // 無音検出が有効な場合のみチェックする
                if !disable_silence_detection {
                    // 最終アクティビティが60秒以上前なら、異常と判断して録音停止
                    // 30秒から60秒に延長して誤検出を減らす
                    let current_time = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let last_active_time = last_active_clone.load(Ordering::SeqCst);
                    
                    // 録音開始から少なくとも20秒は無音検出をスキップする（準備時間）
                    if elapsed.as_secs() < 20 {
                        // 20秒未満の場合は最終アクティブ時間を更新して無音検出をスキップ
                        last_active_clone.store(current_time, Ordering::SeqCst);
                        continue;
                    }
                    
                    if current_time - last_active_time > 60 && last_active_time > 0 {
                        warn!("No audio activity detected for 60 seconds, stopping recording");
                        recording_clone.store(false, Ordering::SeqCst);
                        break;
                    }
                }
            }
            
            // 最大時間に達したら録音を停止
            if recording_clone.load(Ordering::SeqCst) && elapsed.as_secs() >= max_duration {
                warn!("Reached maximum recording duration of {} seconds", max_duration);
                recording_clone.store(false, Ordering::SeqCst);
            }
        });
        
        Ok(())
    }
    
    /// Start recording audio (using default device)
    pub fn start(&mut self) -> Result<()> {
        self.start_with_device(None)
    }
    
    /// Stop recording and return the path to the recording
    pub fn stop(&mut self) -> Result<Option<String>> {
        if !self.recording.load(Ordering::SeqCst) {
            return Ok(None);
        }
        
        info!("Stopping recording and finalizing audio file");
        
        // Set recording flag to false to stop recording
        self.recording.store(false, Ordering::SeqCst);
        
        // Drop the stream to stop recording
        if let Some(stream) = self.stream.take() {
            info!("Closing audio stream");
            drop(stream);
        }
        
        // Wait a bit to ensure all data is flushed
        std::thread::sleep(Duration::from_millis(500));
        
        // Calculate recording duration
        if let Some(start_time) = self.start_time {
            let duration = start_time.elapsed();
            info!("Recording stopped after {:?}", duration);
            self.start_time = None;
        }
        
        // Check if the output file exists and is valid
        if let Some(path) = &self.output_file {
            match std::fs::metadata(path) {
                Ok(metadata) => {
                    let file_size = metadata.len();
                    info!("Recorded file size: {} bytes", file_size);
                    if file_size < 100 {
                        warn!("Warning: Audio file is very small ({} bytes), may not contain valid audio data", file_size);
                    }
                },
                Err(e) => {
                    error!("Failed to get file metadata: {}", e);
                }
            }
        }
        
        // Return the output file path
        let output_file = self.output_file.take();
        Ok(output_file)
    }
    
    /// Setup audio stream with correct sample type
    fn setup_stream<T>(&self, 
                     device: &cpal::Device,
                     config: &cpal::StreamConfig,
                     err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
                     writer: Arc<Mutex<Option<hound::WavWriter<BufWriter<File>>>>>,
                     recording: Arc<AtomicBool>) -> Result<cpal::Stream>
    where
        T: cpal::Sample + hound::Sample + SizedSample,
    {
        info!("Setting up audio stream with type {}", std::any::type_name::<T>());
        
        // 最終アクティブ時間の参照をクローン
        let last_active = self.last_active.clone();
        // Capture the config value we need
        let disable_silence_detection = self.config.recording.disable_silence_detection;
        
        let stream = match std::any::type_name::<T>() {
            "f32" => {
                let channels = config.channels as usize;
                device.build_input_stream(
                    config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        if recording.load(Ordering::SeqCst) {
                            // 無音検出が有効な場合のみ音声アクティビティをチェック
                            if !disable_silence_detection {
                                // RMSベースの音声レベル検出に変更（より正確）
                                let rms: f32 = data.iter()
                                    .map(|&sample| sample * sample)
                                    .sum::<f32>() / data.len() as f32;
                                let rms = rms.sqrt();
                                
                                // しきい値を引き下げて、より小さな音声でもアクティビティとして検出
                                // 0.01より少ない0.003で検出（約70%減少）
                                if rms > 0.003 {
                                    last_active.store(
                                        std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_secs(),
                                        Ordering::SeqCst
                                    );
                                }
                            } else {
                                // 無音検出が無効の場合は常に最終アクティブ時間を更新
                                last_active.store(
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs(),
                                    Ordering::SeqCst
                                );
                            }
                            
                            // Write samples to WAV file
                            if let Ok(mut guard) = writer.lock() {
                                if let Some(writer) = guard.as_mut() {
                                    // Process data in chunks for each channel
                                    for chunk in data.chunks(channels) {
                                        for &sample in chunk {
                                            // Convert f32 [-1.0, 1.0] to i16 range with clipping protection
                                            let sample_clipped = if sample > 1.0 {
                                                1.0
                                            } else if sample < -1.0 {
                                                -1.0
                                            } else {
                                                sample
                                            };
                                            
                                            let sample_i16 = (sample_clipped * 32767.0) as i16;
                                            
                                            if let Err(e) = writer.write_sample(sample_i16) {
                                                error!("Error writing sample: {}", e);
                                                // エラーが発生しても継続を試みる
                                                continue;
                                            }
                                        }
                                    }
                                    
                                    // Attempt to flush the writer periodically
                                    if data.len() > 1000 {
                                        if let Err(e) = writer.flush() {
                                            error!("Error flushing writer: {}", e);
                                            // エラーが発生しても継続する
                                        }
                                    }
                                }
                            }
                        } else if let Ok(mut guard) = writer.lock() {
                            // Finish and close the file when recording stops
                            if let Some(writer) = guard.take() {
                                info!("Finalizing WAV file from stream");
                                if let Err(e) = writer.finalize() {
                                    error!("Error finalizing WAV file: {}", e);
                                }
                                info!("WAV file finalized successfully");
                            }
                        }
                    },
                    err_fn,
                    None
                )?
            },
            "i16" => {
                let channels = config.channels as usize;
                device.build_input_stream(
                    config,
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        if recording.load(Ordering::SeqCst) {
                            // 無音検出が有効な場合のみ音声アクティビティをチェック
                            if !disable_silence_detection {
                                // i16の場合のRMSベースの音声レベル検出
                                let rms: f32 = data.iter()
                                    .map(|&sample| {
                                        let normalized = sample as f32 / 32767.0;
                                        normalized * normalized
                                    })
                                    .sum::<f32>() / data.len() as f32;
                                let rms = rms.sqrt();
                                
                                // しきい値を設定
                                if rms > 0.003 {
                                    last_active.store(
                                        std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_secs(),
                                        Ordering::SeqCst
                                    );
                                }
                            } else {
                                // 無音検出が無効の場合は常に最終アクティブ時間を更新
                                last_active.store(
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs(),
                                    Ordering::SeqCst
                                );
                            }
                            
                            // Write samples to WAV file
                            if let Ok(mut guard) = writer.lock() {
                                if let Some(writer) = guard.as_mut() {
                                    // Process data in chunks for each channel
                                    for chunk in data.chunks(channels) {
                                        for &sample in chunk {
                                            if let Err(e) = writer.write_sample(sample) {
                                                error!("Error writing sample: {}", e);
                                            }
                                        }
                                    }
                                    
                                    // Attempt to flush the writer periodically
                                    if data.len() > 1000 {
                                        if let Err(e) = writer.flush() {
                                            error!("Error flushing writer: {}", e);
                                        }
                                    }
                                }
                            }
                        } else if let Ok(mut guard) = writer.lock() {
                            // Finish and close the file when recording stops
                            if let Some(writer) = guard.take() {
                                info!("Finalizing WAV file from stream");
                                if let Err(e) = writer.finalize() {
                                    error!("Error finalizing WAV file: {}", e);
                                }
                                info!("WAV file finalized successfully");
                            }
                        }
                    },
                    err_fn,
                    None
                )?
            },
            _ => return Err(anyhow::anyhow!("Unsupported sample format")),
        };
        
        Ok(stream)
    }
    
    /// Check if currently recording
    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::SeqCst)
    }
} 