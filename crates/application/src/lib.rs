//! Use cases run on a worker; UI callbacks submit commands and consume events.
use nya_domain::{
    firmware::{FirmwareBackend, FirmwareImage},
    Document, Result, TextBackend, ToolInstallation, ToolRegistry,
};
use std::path::PathBuf;

pub enum Command {
    RefreshTools,
    OpenSource(PathBuf),
    OpenFirmware(PathBuf),
}

pub enum Event {
    ToolsDiscovered(Vec<ToolInstallation>),
    SourceOpened(Document),
    FirmwareOpened(Box<FirmwareImage>),
    Failed(String),
}

pub struct Application<T, E, F> {
    tools: T,
    text: E,
    firmware: F,
}

impl<T: ToolRegistry, E: TextBackend, F: FirmwareBackend> Application<T, E, F> {
    pub fn new(tools: T, text: E, firmware: F) -> Self {
        Self {
            tools,
            text,
            firmware,
        }
    }

    pub fn execute(&mut self, command: Command) -> Event {
        let result: Result<Event> = match command {
            Command::RefreshTools => self.tools.discover().map(Event::ToolsDiscovered),
            Command::OpenSource(path) => self.text.load(&path).map(Event::SourceOpened),
            Command::OpenFirmware(path) => self
                .firmware
                .load(&path)
                .map(|image| Event::FirmwareOpened(Box::new(image))),
        };
        result.unwrap_or_else(|error| Event::Failed(error.to_string()))
    }
}
