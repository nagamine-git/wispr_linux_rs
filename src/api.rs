use anyhow::{Result, Context};
use log::{info, error, warn};
use reqwest::blocking::multipart::{Form, Part};
use serde::{Serialize, Deserialize};
use std::path::Path;
use std::fs::File;
use std::io::Read;
use std::time::Duration;

use crate::config::Config;
use crate::text_processor::TranscriptionProcessor;

/// OpenAI API client
pub struct TranscriptionAPI {
    config: Config,
    client: reqwest::blocking::Client,
}

/// Response from the transcription API
#[derive(Debug, Serialize, Deserialize)]
pub struct TranscriptionResponse {
    pub text: String,
}

impl TranscriptionAPI {
    /// Create a new API client
    pub fn new(config: Config) -> Self {
        // タイムアウト設定を長めに取ったクライアント設定
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120)) // 2分のタイムアウト
            .connect_timeout(Duration::from_secs(30)) // 接続タイムアウト30秒
            .build()
            .unwrap_or_else(|_| {
                warn!("Failed to build custom client, using default");
                reqwest::blocking::Client::new()
            });
            
        Self {
            config,
            client,
        }
    }
    
    /// Transcribe an audio file
    pub fn transcribe(&self, audio_path: &str) -> Result<String> {
        info!("Transcribing audio file: {}", audio_path);
        
        // Check if API key is set
        if self.config.api_key.is_empty() {
            return Err(anyhow::anyhow!("API key not configured"));
        }
        
        // Read the audio file
        let path = Path::new(audio_path);
        let mut file = File::open(path)
            .context("Failed to open audio file")?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)
            .context("Failed to read audio file")?;
            
        // Determine filename for the API
        let filename = path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("audio.wav");
            
        // APIリクエストをリトライループで囲む
        let max_retries = 3;
        let mut retry_count = 0;
        let mut last_error = None;
        
        while retry_count < max_retries {
            // Create form part with audio file
            let part = match Part::bytes(buffer.clone())
                .file_name(filename.to_string())
                .mime_str("audio/wav") {
                Ok(p) => p,
                Err(e) => {
                    error!("Failed to create multipart form: {}", e);
                    return Err(anyhow::anyhow!("Failed to create multipart form: {}", e));
                }
            };
                
            // Create multipart form
            let form = Form::new()
                .part("file", part)
                .text("model", "gpt-4o-mini-transcribe");
                
            info!("Sending API request (attempt {}/{})", retry_count + 1, max_retries);
            
            // Send request to OpenAI API
            let response_result = self.client.post("https://api.openai.com/v1/audio/transcriptions")
                .header("Authorization", format!("Bearer {}", self.config.api_key))
                .multipart(form)
                .send();
                
            match response_result {
                Ok(response) => {
                    // Check if request was successful
                    if response.status().is_success() {
                        // Parse response
                        match response.json::<TranscriptionResponse>() {
                            Ok(transcription) => {
                                info!("Transcription successful");
                                return Ok(transcription.text);
                            },
                            Err(e) => {
                                error!("Failed to parse API response: {}", e);
                                last_error = Some(anyhow::anyhow!("Failed to parse API response: {}", e));
                            }
                        }
                    } else {
                        let status = response.status();
                        let error_text = response.text()
                            .unwrap_or_else(|_| "Failed to read error response".to_string());
                            
                        error!("API error {}: {}", status, error_text);
                        
                        // 5xxエラーや一時的なエラーのみリトライ
                        if status.is_server_error() || 
                           error_text.contains("rate limit") || 
                           error_text.contains("timeout") {
                            warn!("Retryable error detected, will retry");
                            last_error = Some(anyhow::anyhow!("API error {}: {}", status, error_text));
                        } else {
                            // それ以外のエラーはすぐに失敗
                            return Err(anyhow::anyhow!("API error {}: {}", status, error_text));
                        }
                    }
                },
                Err(e) => {
                    error!("Failed to send API request: {}", e);
                    last_error = Some(anyhow::anyhow!("Failed to send API request: {}", e));
                    
                    // タイムアウトやネットワークエラーはリトライ
                    if e.is_timeout() || e.is_connect() {
                        warn!("Network error detected, will retry");
                    } else {
                        // その他のエラーはすぐに失敗
                        return Err(anyhow::anyhow!("Failed to send API request: {}", e));
                    }
                }
            }
            
            // リトライの前に待機（指数バックオフ）
            let wait_time = std::cmp::min(2u64.pow(retry_count as u32), 30);
            warn!("Retrying in {} seconds...", wait_time);
            std::thread::sleep(Duration::from_secs(wait_time));
            
            retry_count += 1;
        }
        
        // 全てのリトライが失敗
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("API request failed after {} retries", max_retries)))
    }
    
    /// Transcribe an audio file with text processing
    pub fn transcribe_with_processing(&self, audio_path: &str) -> Result<String> {
        // 通常の文字起こし実行
        let raw_text = self.transcribe(audio_path)?;
        
        // テキスト処理を適用
        let mut processor = TranscriptionProcessor::new(self.config.clone());
        let processed_text = processor.process_transcription(&raw_text)?;
        
        Ok(processed_text)
    }
    
    /// Implement mock transcription for testing without API key
    #[cfg(debug_assertions)]
    pub fn mock_transcribe(&self, _audio_path: &str) -> Result<String> {
        std::thread::sleep(std::time::Duration::from_secs(2));
        Ok("This is a mock transcription for testing purposes.".to_string())
    }
    
    #[cfg(debug_assertions)]
    pub fn mock_transcribe_with_processing(&self, _audio_path: &str) -> Result<String> {
        let raw_text = "えーと、今日はですね、あのー音声認識の精度についてまぁ話をしたいとおもいます。えっと、最近の技術では、えー、かなり高い精度で認識ができるようになってきてますよね。";
        
        let mut processor = TranscriptionProcessor::new(self.config.clone());
        let processed_text = processor.process_transcription(raw_text)?;
        
        Ok(processed_text)
    }
} 