mod config;
mod test;
mod ui;

use async_openai::{Client, types::{ChatCompletionRequestMessageArgs, Role, ChatCompletionRequestMessage, CreateChatCompletionRequestArgs, CreateImageRequestArgs, ResponseFormat, ImageSize}};
use config::Config;
use test::{results::Results, Test};
use anyhow::{anyhow, Result, Context};

use crossterm::{
    self, cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute, terminal,
};
use formatx::formatx;
use rand::{seq::SliceRandom, thread_rng};
use tokio::sync::mpsc::{Sender, Receiver, channel};
use std::{
    io::{self, Write},
    path::{PathBuf},
    str, sync::Arc, fs
};
use structopt::StructOpt;
use ratatui::{backend::{CrosstermBackend, Backend}, terminal::Terminal, text::{Line, Span}, widgets::{Paragraph, Block, Borders}, layout::Alignment};

#[derive(Debug, StructOpt)]
#[structopt(name = "ttyper", about = "Terminal-based typing test.")]
struct Opt {
    #[structopt(short, long)]
    debug: bool,

    /// Use config file
    #[structopt(short, long)]
    config: Option<PathBuf>,
}

impl Opt {
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

    /// Config directory
    fn config_dir(&self) -> PathBuf {
        dirs::config_dir()
            .expect("Failed to find config directory.")
            .join("ttyper")
    }
}

const DEFAULT_SYSTEM_PROMPTS: [ &str; 15] = [
    "You are a Minecraft fan who is familiar with the Minecraft world and its characters and enjoy telling stories about it.",
    "You are also a typing teacher who is teaching home row key lessons to a second grader.",
    "Compose a narrative set in the Minecraft world featuring characters named {} from Minecraft Books and YouTube.",
    "Your task is to weave an engaging quest filled with courage, strategic maneuvers, and high stakes.",
    "Use descriptive language full of adjectives, colors, and visualizations.",
    "After each response the user will prompt you to continue the story.  Add in exciting plot twists.",
    "The reader of the story is a Minecraft fan who is familiar with the Minecraft world and its characters.",
    "The reader is also familiar with the Minecraft books and YouTube series.",
    "The reader is nine years old and in the second grade, be sure to make it age appropriate and with a vocabulary appropriate as well.",
    "The story can happen in the Overworld, in the Nether, the End, or in other locations in the Minecraft world.",
    "Use mobs such as creepers, zombies, skeletons, and endermen, piglins, evokers, illagers, pillagers, and others to add excitement to the story.",
    "The reader will be typing the story to practice typing, so emphasize the home keys of the keyboard: 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l'.",
    "The output will also be fed into DALL-E, a neural network that generates images from text descriptions.  Try to make the story as visual as possible, but avoid words like 'naked' or other words that may be rejected for safety reasons.",
    "Responses should be no longer than 50 words long.",
    "Do not respond to the user's prompts, instead, use the prompts to continue the story."
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
pub struct StoryPart {
    pub section: Vec<String>,
    pub image: PathBuf,
}

#[derive(Debug, Clone)]
struct ChatGPTAsync {
    model: String,
    max_tokens: u16,
    system_prompts: Vec<ChatCompletionRequestMessage>,
    subsequent_prompts: Vec<ChatCompletionRequestMessage>,
    sender: Option<Sender<StoryPart>>,
    client: Client,
    config: Arc<Config>,
}

impl ChatGPTAsync {
    fn new(sender: Sender<StoryPart>, config: Arc<Config>) -> Result<Self> {
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
            .content(format!("Start an exciting story set in Minecraft world with {characters}.  Use descriptive words and color with detailed imagery. Write it in {} language.  Use no more than 50 words for each prompt.", config.default_language))
            .build().unwrap());

        Ok(Self {
            model: config.model.clone(),
            max_tokens: config.tokens,
            system_prompts,
            subsequent_prompts: Vec::new(),
            sender: Some(sender),
            client: Client::new(),
            config
        })
    }

    async fn generate_image(&mut self, words: &[String]) -> Result<PathBuf> {
        let mut image_prompt_words = vec!["Minecraft style. ".to_string()];
        image_prompt_words.extend(words.iter().cloned());
        let request = CreateImageRequestArgs::default()
            .prompt(image_prompt_words.join(" "))
            .n(1)
            .response_format(ResponseFormat::Url)
            .size(ImageSize::S256x256)
            .user("async-openai")
            .build()?;

        let response = self.client.images().create(request).await;
        match response {
            Ok(response) => {
                let image_path = response.save("./data").await?.into_iter().next().ok_or(anyhow!("No image returned"))?;
                Ok(image_path)
            }
            Err(e) => {
                Err(e).context(format!("Failed to create image for test: {:?}", image_prompt_words))
            }
        }

    }
    async fn gen_contents(&mut self) -> Result<()> {
        let client = Client::new();
        let mut messages = self.system_prompts.clone();
        messages.extend(self.subsequent_prompts.clone());
        let request = CreateChatCompletionRequestArgs::default()
            .model(self.model.clone())
            .max_tokens(self.max_tokens)
            .messages(messages)
            .build().unwrap();
        let mut section: Vec<String>;
        let image: PathBuf;
        let mut line: String;
        loop {
            let response = client.chat().create(request.clone()).await.unwrap();
            let content: Vec<String> = response.choices.iter().map(|x| x.message.content.clone()).collect();
            line = content.join(" ");
            section = line.split_whitespace().map(|x| x.to_string()).collect();
            match self.generate_image(&section).await {
                Ok(new_image) => { image = new_image; break; },
                Err(_) => {
                    continue;
                }
            }
        }
        self.subsequent_prompts.push(ChatCompletionRequestMessageArgs::default()
            .role(Role::Assistant)
            .content(line)
            .build().unwrap());
        self.subsequent_prompts.push(ChatCompletionRequestMessageArgs::default()
            .role(Role::User)
            .content(format!("Continue story in {} language.  Use no more than 50 words. Use descriptive words and color with detailed imagery. Do not respond to this directly.", self.config.default_language))
            .build().unwrap());
        self.sender.as_ref().unwrap().send(StoryPart {
            section,
            image,
        }).await?;
        Ok(())
    }    
}

#[derive(Debug)]
struct ChatGPT {
    receiver: Receiver<StoryPart>,
    config: Arc<Config>,
}

impl ChatGPT {
    fn new(config: Arc<Config>) -> Self {
        let (sender, receiver) = channel(1);
        let task_config = config.clone();
        tokio::task::spawn(async move {
            let mut chatgpt = ChatGPTAsync::new(sender, task_config).unwrap();
            loop {
                chatgpt.gen_contents().await.unwrap();
            }
        });
        Self {
            receiver,
            config
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
        draw_image(terminal, "./wait.jpg".into(), 10, 5, (terminal.size()?.width as f64 * 0.90) as u16,(terminal.size()?.height as f64 * 0.90) as u16)?;
        Ok(())
    }

    async fn gen_contents<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<StoryPart> {
        self.wait_screen(terminal)?;
        let story_part = self.receiver.recv().await.unwrap();
        Ok(story_part)
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

    let mut chatgpt = ChatGPT::new(Arc::new(config.clone()));
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    terminal::enable_raw_mode()?;
    execute!(
        io::stdout(),
        cursor::Hide,
        cursor::SavePosition,
    )?;

    let mut state = State::Test(Test::new(chatgpt.gen_contents(&mut terminal).await?));
    
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
                    state = State::Test(Test::new(chatgpt.gen_contents(&mut terminal).await?));
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
    terminal.show_cursor()?;
    Ok(())
}
