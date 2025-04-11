use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::path::Path;
use anyhow::{Result, Context};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use log::{info, error, warn};
use regex;

use crate::config::Config;

#[derive(Serialize, Deserialize, Default)]
pub struct UserDictionary {
    words: HashMap<String, String>,
    frequent_terms: HashMap<String, u32>,
}

impl UserDictionary {
    pub fn new() -> Self {
        Self {
            words: HashMap::new(),
            frequent_terms: HashMap::new(),
        }
    }

    pub fn load(path: &Path) -> Self {
        if path.exists() {
            match File::open(path) {
                Ok(file) => {
                    let reader = BufReader::new(file);
                    match serde_json::from_reader(reader) {
                        Ok(dict) => return dict,
                        Err(e) => {
                            error!("辞書ファイルの読み込みに失敗しました: {}", e);
                            Self::new()
                        }
                    }
                },
                Err(e) => {
                    error!("辞書ファイルを開けませんでした: {}", e);
                    Self::new()
                }
            }
        } else {
            Self::new()
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            if !dir.exists() {
                fs::create_dir_all(dir).context("辞書ディレクトリの作成に失敗")?;
            }
        }
        
        let file = File::create(path).context("辞書ファイルの作成に失敗")?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, self).context("辞書の保存に失敗")?;
        Ok(())
    }

    pub fn add_word(&mut self, original: String, replacement: String) {
        self.words.insert(original, replacement);
    }

    pub fn update_frequency(&mut self, term: String) {
        let count = self.frequent_terms.entry(term).or_insert(0);
        *count += 1;
    }

    pub fn apply_dictionary(&self, text: &str) -> String {
        if self.words.is_empty() {
            return text.to_string();
        }
        
        info!("辞書を適用します: {} 件の登録単語", self.words.len());
        let mut result = text.to_string();
        
        // 単語を適切に分離して処理
        for (original, replacement) in &self.words {
            // 単語の前後に空白や句読点があるかチェックして、単語単位での置換を行う
            let pattern = format!(r"(^|\s|、|。|「|」){}($|\s|、|。|「|」)", regex::escape(original));
            if let Ok(regex) = regex::Regex::new(&pattern) {
                result = regex.replace_all(&result, format!("$1{}$2", replacement)).to_string();
                continue;
            }
            
            // 正規表現エラーの場合は単純な文字列置換を行う
            result = result.replace(original, replacement);
        }
        
        info!("辞書適用後: {}", result);
        result
    }
}

pub struct TextFormatter {
    client: Client,
}

impl TextFormatter {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }
}

pub struct TranscriptionProcessor {
    dictionary: UserDictionary,
    formatter: TextFormatter,
    dictionary_path: std::path::PathBuf,
    config: Config,
}

impl TranscriptionProcessor {
    pub fn new(config: Config) -> Self {
        let dictionary_path = config.temp_dir.join("user_dictionary.json");
        let dictionary = UserDictionary::load(&dictionary_path);
        let formatter = TextFormatter::new();
        
        Self {
            dictionary,
            formatter,
            dictionary_path,
            config,
        }
    }
    
    pub fn process_transcription(&mut self, raw_text: &str) -> Result<String> {
        if raw_text.trim().is_empty() {
            return Ok(String::new());
        }
        
        info!("文字起こしテキストの処理を開始: \"{}\"", raw_text);
        
        // GPTでテキスト整形（辞書情報をプロンプトに埋め込む）
        let formatted = self.format_with_dictionary_embedded(raw_text)?;
        
        // 単語の頻度学習
        self.learn_from_text(raw_text);
        
        // 辞書保存
        if let Err(e) = self.dictionary.save(&self.dictionary_path) {
            warn!("辞書の保存に失敗: {}", e);
        }
        
        Ok(formatted)
    }
    
    pub fn add_custom_word(&mut self, original: String, replacement: String) -> Result<()> {
        info!("カスタム単語を追加: \"{}\" -> \"{}\"", original, replacement);
        self.dictionary.add_word(original, replacement);
        self.dictionary.save(&self.dictionary_path).context("辞書の保存に失敗")?;
        Ok(())
    }
    
    fn learn_from_text(&mut self, text: &str) {
        // 簡易的な単語頻度学習
        for word in text.split_whitespace() {
            if word.len() > 1 {
                self.dictionary.update_frequency(word.to_string());
            }
        }
    }

    // 辞書情報をプロンプトに埋め込んだGPT処理
    fn format_with_dictionary_embedded(&self, input_text: &str) -> Result<String> {
        if input_text.trim().is_empty() {
            return Ok(String::new());
        }

        // 辞書の内容をプロンプトに埋め込む
        let mut dictionary_instructions = String::new();
        
        if !self.dictionary.words.is_empty() {
            dictionary_instructions.push_str("When the following words or expressions appear, make sure to modify them exactly as specified:\n");
            
            for (original, replacement) in &self.dictionary.words {
                dictionary_instructions.push_str(&format!("- Replace \"{}\" with \"{}\"\n", original, replacement));
            }
            
            dictionary_instructions.push_str("\nEnsure to apply these word replacements exactly as specified while maintaining the word usage context.\n\n");
        }
        
        let prompt = format!(
            "Enhance this transcribed text while preserving the original language:\n\
            - Keep the text in its original language - do not translate\n\
            - Remove excessive filler words (like えー, あの) only if they are overly frequent\n\
            - Preserve casual speech patterns and tone\n\
            - Keep the original writing style and expressions\n\
            - Add line breaks and paragraph separations only where necessary\n\
            - Add bullet points or lists where contextually appropriate\n\
            {}\
            Input text: {}", 
            dictionary_instructions, input_text
        );

        info!("GPTによるテキスト整形とワード置換を開始（辞書単語数: {}）", self.dictionary.words.len());
        let response = self.formatter.client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&json!({
                "model": "gpt-4o-mini",
                "messages": [
                    {"role": "system", "content": "You are a transcription proofreader. Maintain the original language of the input text. Never translate. Output the corrected text directly without any meta-commentary."},
                    {"role": "user", "content": prompt}
                ],
                "temperature": 0.5,
                "max_tokens": 1000
            }))
            .send()
            .context("APIリクエスト失敗")?;

        let response_json: Value = response.json().context("JSONパース失敗")?;
        
        if let Some(error) = response_json.get("error") {
            let error_message = error.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown error");
            error!("GPT API エラー: {}", error_message);
            return Err(anyhow::anyhow!("API エラー: {}", error_message));
        }
        
        let formatted_text = response_json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string();
            
        info!("テキスト整形完了");
        Ok(formatted_text)
    }
} 