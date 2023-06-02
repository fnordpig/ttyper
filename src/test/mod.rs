pub mod results;

use async_openai::Client;
use async_openai::types::{CreateImageRequestArgs, ResponseFormat, ImageSize};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;
use anyhow::{Result, anyhow, Context};

pub struct TestEvent {
    pub time: Instant,
    pub key: KeyEvent,
    pub correct: Option<bool>,
}

impl fmt::Debug for TestEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestEvent")
            .field("time", &String::from("Instant { ... }"))
            .field("key", &self.key)
            .finish()
    }
}

#[derive(Debug)]
pub struct TestWord {
    pub text: String,
    pub progress: String,
    pub events: Vec<TestEvent>,
}

impl From<String> for TestWord {
    fn from(string: String) -> Self {
        TestWord {
            text: string,
            progress: String::new(),
            events: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct Test {
    pub words: Vec<TestWord>,
    pub current_word: usize,
    pub complete: bool,
    pub image_path: PathBuf,
}

impl Test {
    pub async fn new(words: Vec<String>) -> Result<Self> {
        let client = Client::new();
        let mut image_prompt_words = vec!["Render an image in Minecraft style. ".to_string()];
        image_prompt_words.extend(words.clone());
        let request = CreateImageRequestArgs::default()
            .prompt(image_prompt_words.join(" "))
            .n(1)
            .response_format(ResponseFormat::Url)
            .size(ImageSize::S256x256)
            .user("async-openai")
            .build()?;

        let response = client.images().create(request).await;
        match response {
            Ok(response) => {
                let image_path = response.save("./data").await?.into_iter().next().ok_or(anyhow!("No image returned"))?;
                Ok(Self {
                    words: words.into_iter().map(TestWord::from).collect(),
                    current_word: 0,
                    complete: false,
                    image_path,
                })        
            }
            Err(e) => {
                Err(e).context(format!("Failed to create image for test: {:?}", image_prompt_words))
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        let word = &mut self.words[self.current_word];
        match key.code {
            KeyCode::Char(' ') | KeyCode::Enter => {
                if word.text.chars().nth(word.progress.len()) == Some(' ') {
                    word.progress.push(' ');
                    word.events.push(TestEvent {
                        time: Instant::now(),
                        correct: Some(true),
                        key,
                    })
                } else if !word.progress.is_empty() || word.text.is_empty() {
                    word.events.push(TestEvent {
                        time: Instant::now(),
                        correct: Some(word.text == word.progress),
                        key,
                    });
                    self.next_word();
                }
            }
            KeyCode::Backspace => {
                if word.progress.is_empty() {
                    self.last_word();
                } else {
                    word.events.push(TestEvent {
                        time: Instant::now(),
                        correct: Some(!word.text.starts_with(&word.progress[..])),
                        key,
                    });
                    word.progress.pop();
                }
            }
            // CTRL-BackSpace
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.words[self.current_word].progress.is_empty() {
                    self.last_word();
                }

                let word = &mut self.words[self.current_word];

                word.events.push(TestEvent {
                    time: Instant::now(),
                    correct: None,
                    key,
                });
                word.progress.clear();
            }
            KeyCode::Char(c) => {
                word.progress.push(c);
                word.events.push(TestEvent {
                    time: Instant::now(),
                    correct: Some(word.text.starts_with(&word.progress[..])),
                    key,
                });
                if word.progress == word.text && self.current_word == self.words.len() - 1 {
                    self.complete = true;
                    self.current_word = 0;
                }
            }
            _ => {}
        };
    }

    fn last_word(&mut self) {
        if self.current_word != 0 {
            self.current_word -= 1;
        }
    }

    fn next_word(&mut self) {
        if self.current_word == self.words.len() - 1 {
            self.complete = true;
            self.current_word = 0;
        } else {
            self.current_word += 1;
        }
    }
}
