//! Use cases run on a worker; UI callbacks submit commands and consume events.
use nya_domain::{Document, Result, TextBackend, ToolInstallation, ToolRegistry};
use std::path::PathBuf;

pub enum Command {
    RefreshTools,
    OpenSource(PathBuf),
}

pub enum Event {
    ToolsDiscovered(Vec<ToolInstallation>),
    SourceOpened(Document),
    Failed(String),
}

pub struct Application<T, E> {
    tools: T,
    text: E,
}

impl<T: ToolRegistry, E: TextBackend> Application<T, E> {
    pub fn new(tools: T, text: E) -> Self {
        Self { tools, text }
    }

    pub fn execute(&mut self, command: Command) -> Event {
        let result: Result<Event> = match command {
            Command::RefreshTools => self.tools.discover().map(Event::ToolsDiscovered),
            Command::OpenSource(path) => self.text.load(&path).map(Event::SourceOpened),
        };
        result.unwrap_or_else(|error| Event::Failed(error.to_string()))
    }
}
