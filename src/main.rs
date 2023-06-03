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
use formatx::formatx;
use rand::{seq::SliceRandom, thread_rng};
use rust_embed::RustEmbed;
use std::{
    ffi::OsString,
    fs,
    io::{self, BufRead, Write},
    num,
    path::{PathBuf},
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
const DEFAULT_MAX_TOKENS: u16 = 75u16;
const DEFAULT_SYSTEM_PROMPTS: [ &str; 3] = [
    "Compose a narrative set in the Minecraft world featuring characters named {} from Minecraft Books and YouTube. Your task is to weave an engaging quest filled with courage, strategic maneuvers, and high stakes. However, there is a unique constraint: you're only allowed to use the following characters to construct sentences: 'a', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'o', 's', 'u', 'y', 't'. This means you must completely avoid using 'n', 'r', 'b', 'm', 'w', 'v', 'c', 'p', and any other characters not listed in the allowed set, including words like 'and', 'but', 'they', 'with', 'from', 'can', 'upon', 'moon', 'against', 'fabulous', 'aghast' and any others not allowed. Be especially vigilant about this as the purpose of the story is for a typing tutorial program, so incorporating words with letters that aren't allowed would not be beneficial for the students. The narrative should flow naturally, despite these unique constraints.  Use descriptive language full of adjectives, colors, and visualizations.",
    "After each response the user will prompt you to continue the story.  Add in exciting plot twists.  Do not use any other letters than 'asdfghjkleiou'.  Do not respond directly to the users prompt.",
    "Responses should be no longer than 50 words long."
];
const MINECRAFT_CHARACTERS: [&str; 16] = [
    "Jedu", 
    "Eli", 
    "Dash",
    "Herobrine",
    "Steve",
    "Alex",
    "Notch",
    "Jeb",
    "Mikey and JJ",
    "Dave the Villager",
    "Sir Hogarth",
    "Gromp",
    "Clyde",
    "Arch-Illager",
    "Villager",
    "Baby Zeke",
];

#[derive(Debug, Clone, Default)]
struct ChatGPT {
    model: String,
    max_tokens: u16,
    system_prompts: Vec<ChatCompletionRequestMessage>,
    subsequent_prompts: Vec<ChatCompletionRequestMessage>,
}

impl ChatGPT {
    fn default () -> Result<Self> {
        let characters = MINECRAFT_CHARACTERS.choose_multiple(&mut thread_rng(), 3).fold(String::new(), |acc, x| acc + x + ", ");

        let mut system_prompts = DEFAULT_SYSTEM_PROMPTS.iter()
        .map(|x| {
            let filled = formatx!(x.to_string(), &characters)?;
            Ok(ChatCompletionRequestMessageArgs::default()
                .role(Role::System) 
                .content(filled)
                .build().unwrap())
        })
        .collect::<Result<Vec<_>>>()?;
        system_prompts.push(ChatCompletionRequestMessageArgs::default()
            .role(Role::User)
            .content(format!("Start a story set in Minecraft world with {characters} using only the letters 'asdfghjkleiout''."))
            .build().unwrap());

        Ok(Self {
            model: DEFAULT_CHATGPT_MODEL.to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            system_prompts,
            subsequent_prompts: Vec::new(),
        })
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
        draw_image(terminal, "./wait.jpg".into(), 10, 5, (terminal.size()?.width as f64 * 0.90) as u16,(terminal.size()?.height as f64 * 0.90) as u16)?;
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
            .role(Role::User)
            .content("Continue story.  Use only the letters 'asdfghjkleiou'.  Do not use the letters 'tpwqrzxcvbnm'. Do not respond to this directly.")
            .build().unwrap());
        Ok(Some(words))
    }    
}

fn draw_image<B: Backend>(terminal: &mut Terminal<B>, image_path: PathBuf, x: u16, y: u16, w: u16, h: u16) -> Result<()> {
    let options = rascii_art::RenderOptions::default()
        .colored(true)
        .charset(rascii_art::charsets::BLOCK)
        .height(h as u32)
        .width(w as u32);
    
    let mut image = vec![];
    rascii_art::render_to(image_path, &mut image, options).unwrap();
    let image_string = String::from_utf8_lossy(&image);
    let image_lines = image_string.lines();
    terminal.set_cursor(x, y)?;
    for (offset, line) in image_lines.enumerate() {
        terminal.set_cursor(x, y + offset as u16)?;
        write!(std::io::stdout(), "{}", line)?;
    }    
    Ok(())
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
    ) -> Result<()> {
        match self {
            State::Test(test) => {
                terminal.draw(|f| {
                    f.render_widget(config.theme.apply_to(test), f.size());
                })?;
                draw_image(terminal, test.image_path.clone(), 10, 10, (terminal.size()?.width as f64 * 0.75) as u16,(terminal.size()?.height as f64 * 0.75) as u16)?;
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
    let mut chatgpt = ChatGPT::default()?;
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

    let mut state = State::Test(Test::new(words).await?);
    
    terminal.clear()?;
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
                    terminal.clear()?;
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
                        )).await?);
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
