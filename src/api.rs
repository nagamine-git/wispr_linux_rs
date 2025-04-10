use anyhow::{Result, Context};
use log::{info, error};
use reqwest::blocking::multipart::{Form, Part};
use serde::{Serialize, Deserialize};
use std::path::Path;
use std::fs::File;
use std::io::Read;

use crate::config::Config;

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
        Self {
            config,
            client: reqwest::blocking::Client::new(),
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
            
        // Create form part with audio file
        let part = Part::bytes(buffer)
            .file_name(filename.to_string())
            .mime_str("audio/wav")?;
            
        // Create multipart form
        let form = Form::new()
            .part("file", part)
            .text("model", "whisper-1");
            
        // Send request to OpenAI API
        let response = self.client.post("https://api.openai.com/v1/audio/transcriptions")
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .multipart(form)
            .send()
            .context("Failed to send API request")?;
            
        // Check if request was successful
        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text()
                .unwrap_or_else(|_| "Failed to read error response".to_string());
                
            error!("API error {}: {}", status, error_text);
            return Err(anyhow::anyhow!("API error {}: {}", status, error_text));
        }
        
        // Parse response
        let transcription: TranscriptionResponse = response.json()
            .context("Failed to parse API response")?;
            
        info!("Transcription successful");
        
        // Return transcribed text
        Ok(transcription.text)
    }
    
    /// Implement mock transcription for testing without API key
    #[cfg(debug_assertions)]
    pub fn mock_transcribe(&self, _audio_path: &str) -> Result<String> {
        std::thread::sleep(std::time::Duration::from_secs(2));
        Ok("This is a mock transcription for testing purposes.".to_string())
    }
} 