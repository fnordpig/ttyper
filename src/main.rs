mod config;
mod test;
mod ui;

use async_openai::{Client, types::{ChatCompletionRequestMessageArgs, Role, ChatCompletionRequestMessage, CreateChatCompletionRequestArgs}};
use config::Config;
use test::{results::Results, Test};
use anyhow::Result;

use crossterm::{
    self, cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute, terminal,
};
use rand::{seq::SliceRandom, thread_rng};
use rust_embed::RustEmbed;
use std::{
    ffi::OsString,
    fs,
    io::{self, BufRead, stdout, Write},
    num,
    path::PathBuf,
    str,
};
use structopt::StructOpt;
use ratatui::{backend::{CrosstermBackend, Backend}, terminal::Terminal, text::{Line, Span}, widgets::{Paragraph, Block, Borders}, layout::Alignment};

#[derive(RustEmbed)]
#[folder = "resources/runtime"]
struct Resources;

#[derive(Debug, StructOpt)]
#[structopt(name = "ttyper", about = "Terminal-based typing test.")]
struct Opt {
    #[structopt(parse(from_os_str))]
    contents: Option<PathBuf>,

    #[structopt(short, long)]
    debug: bool,

    /// Specify word count
    #[structopt(short, long, default_value = "50")]
    words: num::NonZeroUsize,

    /// Use config file
    #[structopt(short, long)]
    config: Option<PathBuf>,

    /// Specify test language in file
    #[structopt(long, parse(from_os_str))]
    language_file: Option<PathBuf>,

    /// Specify test language
    #[structopt(short, long)]
    language: Option<String>,

    /// List installed languages
    #[structopt(long)]
    list_languages: bool,
}

impl Opt {
    async fn gen_contents(&self) -> Option<Vec<String>> {
        match &self.contents {
            Some(path) => {
                let lines: Vec<String> = if path.as_os_str() == "-" {
                    std::io::stdin()
                        .lock()
                        .lines()
                        .filter_map(Result::ok)
                        .collect()
                } else {
                    let file = fs::File::open(path).expect("Error reading language file.");
                    io::BufReader::new(file)
                        .lines()
                        .filter_map(Result::ok)
                        .collect()
                };

                Some(lines.iter().map(String::from).collect())
            }
            None => {
                let lang_name = self
                    .language
                    .clone()
                    .unwrap_or_else(|| self.config().default_language);

                let bytes: Vec<u8> = self
                    .language_file
                    .as_ref()
                    .map(fs::read)
                    .and_then(Result::ok)
                    .or_else(|| fs::read(self.language_dir().join(&lang_name)).ok())
                    .or_else(|| {
                        Resources::get(&format!("language/{}", &lang_name))
                            .map(|f| f.data.into_owned())
                    })?;

                let mut rng = thread_rng();

                let mut language: Vec<&str> = str::from_utf8(&bytes)
                    .expect("Language file had non-utf8 encoding.")
                    .lines()
                    .collect();
                language.shuffle(&mut rng);

                let mut contents: Vec<_> = language
                    .into_iter()
                    .cycle()
                    .take(self.words.get())
                    .map(ToOwned::to_owned)
                    .collect();
                contents.shuffle(&mut rng);
                Some(contents)
            }
        }
    }


    /// Configuration
    fn config(&self) -> Config {
        fs::read(
            self.config
                .clone()
                .unwrap_or_else(|| self.config_dir().join("config.toml")),
        )
        .map(|bytes| toml::from_str(str::from_utf8(&bytes).unwrap_or_default()).expect("Configuration was ill-formed."))
        .unwrap_or_default()
    }

    /// Installed languages under config directory
    fn languages(&self) -> io::Result<Vec<OsString>> {
        Ok(self
            .language_dir()
            .read_dir()?
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect())
    }

    /// Config directory
    fn config_dir(&self) -> PathBuf {
        dirs::config_dir()
            .expect("Failed to find config directory.")
            .join("ttyper")
    }

    /// Language directory under config directory
    fn language_dir(&self) -> PathBuf {
        self.config_dir().join("language")
    }
}

const DEFAULT_CHATGPT_MODEL: &str = "gpt-3.5-turbo";
const DEFAULT_MAX_TOKENS: u16 = 3000u16;
const DEFAULT_SYSTEM_PROMPTS: [ &str; 14] = [
    "You are an English language typing tutor that comes up with stories to type to train students of varying levels of skill.",
    "Given a prompt describing the students skill level provide a new sentence to type which will give a good exercise of typing skills utilizing the focus prompted.",
    "Focus on providing sentences that exercise the keyboard layout on a QWERTY keyboard. Do not explain anything and do not respond with anything more than the sentence to type.",
    "Do not provide information other than the sentence to type, and the sentence must be a part of the story.",
    "The syntax of the prompt will be 'Level: <skill level> Focus: <letters to focus on> Theme: <theme>'.",
    "The levels of skill are: beginner, intermediate, advanced, and expert.",
    "The letters to focus on are keys on the keyboard to use in generating sentences.",
    "You may use letters that are not in the focus, but most of the letters should be in the focus.",
    "The theme is the theme of the sentence and the sentence you generate must be on that theme.",
    "Be sure every sentence is a natural language English sentence that emphasizes the keys requested, but it may contain keys not requested to make the sentence more sensible.",
    "Make sure to write long sentences, at least 10 words lone but no more than 20 words long.",
    "An example initial: 'Level: beginner Focus: abcdefghijklmnopqrstuvwxyz Theme: Minecraft story with Creepers and Zombies and Steve'.  You respond: 'Steve was walking through the forest when he saw a creeper.  He ran away from the creeper.  The creeper exploded.  Steve was killed.'",
    "I will prompt you to continue the story with the sentence 'Continue the story. Do not respond to this directly'.  You will then continue the story with exciting plot twists.",
    "As you continue to story remember to use the focus keys and to use the theme.",
];

#[derive(Debug, Clone, Default)]
struct ChatGPT {
    model: String,
    max_tokens: u16,
    system_prompts: Vec<ChatCompletionRequestMessage>,
    subsequent_prompts: Vec<ChatCompletionRequestMessage>,
}

impl ChatGPT {
    fn default () -> Self {
        let mut system_prompts = DEFAULT_SYSTEM_PROMPTS.iter()
        .map(|x| ChatCompletionRequestMessageArgs::default()
            .role(Role::System)
            .content(x.to_string())
            .build().unwrap())
        .collect::<Vec<_>>();
        system_prompts.push(ChatCompletionRequestMessageArgs::default()
            .role(Role::User)
            .content("Level: beginner Focus: asdfghjkleiourt Theme: Story set in Minecraft world with Steve and Herobrine".to_string())
            .build().unwrap());

        Self {
            model: DEFAULT_CHATGPT_MODEL.to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            system_prompts,
            subsequent_prompts: Vec::new(),
        }
    }

    fn wait_screen<B: Backend>(&self, terminal: &mut Terminal<B>) -> Result<()> {
        terminal.clear()?;
        terminal.draw(|f| {
            let text = vec![
                Line::from(Span::raw("Loading...")),
            ];
            let paragraph = Paragraph::new(text)
                .block(Block::default().borders(Borders::ALL))
                .alignment(Alignment::Center);
            f.render_widget(paragraph, f.size());
        })?;
        let options = rascii_art::RenderOptions::default()
            .colored(true)
            .charset(rascii_art::charsets::BLOCK)
            .height((terminal.size()?.height as f64 * 0.90) as u32)
            .width((terminal.size()?.width as f64 * 0.90) as u32);
        
        let mut image = vec![];
        rascii_art::render_to("wait.jpg", &mut image, options).unwrap();
        let image_string = String::from_utf8_lossy(&image);
        let image_lines = image_string.lines();
        let x = 10;
        let y = 5;
        terminal.set_cursor(x, y)?;
        for (offset, line) in image_lines.enumerate() {
            terminal.set_cursor(x, y + offset as u16)?;
            write!(std::io::stdout(), "{}", line)?;
        }    
        Ok(())
    }

    async fn gen_contents<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<Option<Vec<String>>> {
        self.wait_screen(terminal)?;
        let client = Client::new();
        let mut messages = self.system_prompts.clone();
        messages.extend(self.subsequent_prompts.clone());
        let request = CreateChatCompletionRequestArgs::default()
            .model(self.model.clone())
            .max_tokens(self.max_tokens)
            .messages(messages)
            .build().unwrap();
        let response = client.chat().create(request).await.unwrap();
        let content: Vec<String> = response.choices.iter().map(|x| x.message.content.clone()).collect();
        let line = content.join(" ");
        let words = line.split_whitespace().map(|x| x.to_string()).collect();
        self.subsequent_prompts.push(ChatCompletionRequestMessageArgs::default()
            .role(Role::Assistant)
            .content(line)
            .build().unwrap());
        self.subsequent_prompts.push(ChatCompletionRequestMessageArgs::default()
            .role(Role::Assistant)
            .content("Continue the story.  Do not respond to this directly.")
            .build().unwrap());
        terminal.clear()?;
        Ok(Some(words))
    }    
}

enum State {
    Test(Test),
    Results(Results),
}

impl State {
    fn render_into<B: Backend>(
        &self,
        terminal: &mut Terminal<B>,
        config: &Config,
    ) -> crossterm::Result<()> {
        match self {
            State::Test(test) => {
                terminal.draw(|f| {
                    f.render_widget(config.theme.apply_to(test), f.size());
                })?;
            }
            State::Results(results) => {
                terminal.draw(|f| {
                    f.render_widget(config.theme.apply_to(results), f.size());
                })?;
            }
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let opt = Opt::from_args();
    if opt.debug {
        dbg!(&opt);
    }

    let config = opt.config();
    if opt.debug {
        dbg!(&config);
    }

    if opt.list_languages {
        opt.languages()
            .expect("Couldn't get installed languages under config directory. Make sure the config directory exists.")
            .iter()
            .for_each(|name| println!("{}", name.to_str().expect("Ill-formatted language name.")));
        return Ok(());
    }
    let mut chatgpt = ChatGPT::default();
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    terminal::enable_raw_mode()?;
    execute!(
        io::stdout(),
        cursor::Hide,
        cursor::SavePosition,
    )?;

    let words = chatgpt.gen_contents(&mut terminal).await?.expect(
        "Couldn't get test contents. Make sure the specified language actually exists.",
    );

    terminal.clear()?;
    let mut state = State::Test(Test::new(words));

    state.render_into(&mut terminal, &config)?;
    loop {
        let event = event::read()?;

        // handle exit controls
        match event {
            Event::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => break,
            Event::Key(KeyEvent {
                code: KeyCode::Esc,
                modifiers: KeyModifiers::NONE,
                ..
            }) => match state {
                State::Test(ref test) => {
                    state = State::Results(Results::from(test));
                }
                State::Results(_) => break,
            },
            _ => {}
        }

        match state {
            State::Test(ref mut test) => {
                if let Event::Key(key) = event {
                    test.handle_key(key);
                    if test.complete {
                        state = State::Results(Results::from(&*test));
                    }
                }
            }
            State::Results(_) => match event {
                Event::Key(KeyEvent {
                    code: KeyCode::Char('r'),
                    modifiers: KeyModifiers::NONE,
                    ..
                }) => {
                    state = State::Test(Test::new(chatgpt.gen_contents(&mut terminal).await?.expect(
                            "Couldn't get test contents. Make sure the specified language actually exists.",
                        )));
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    modifiers: KeyModifiers::NONE,
                    ..
                }) => break,
                _ => {}
            },
        }

        state.render_into(&mut terminal, &config)?;
    }

    terminal::disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        cursor::RestorePosition,
        cursor::Show,
        terminal::LeaveAlternateScreen,
    )?;
    terminal.show_cursor();
    Ok(())
}
