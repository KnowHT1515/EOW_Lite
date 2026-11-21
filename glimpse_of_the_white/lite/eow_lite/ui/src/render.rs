use std::{io::{self, Write}, ops::Range};

use crossterm::{cursor::{Hide, MoveTo, Show}, queue, style::{Color, Print, SetBackgroundColor, SetForegroundColor}, terminal::{Clear, ClearType}};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::protocol::{ContentType, ContextEvent};

const WHITE: Color = Color::Rgb { r: 255, g: 255, b: 255 };
const YELLOW: Color = Color::Rgb { r: 255, g: 255, b: 0 };

#[derive(Clone, Copy, PartialEq)]
enum Role {
    User,
    Instance,
    Notice,
}

struct Message {
    context_id: String,
    role: Role,
    content: String,
    visible: usize,
    next_chunk: usize,
    ended: bool,
}

#[derive(Clone, Copy)]
struct Anchor {
    message: usize,
    offset: usize,
}

struct Row {
    anchor: Anchor,
    prefix: bool,
    role: Role,
    text: String,
}

#[derive(Clone, Default, PartialEq)]
struct Line {
    prefix: String,
    yellow: bool,
    text: String,
}

pub struct View {
    messages: Vec<Message>,
    anchor: Option<Anchor>,
    previous: Vec<Line>,
    dimensions: (u16, u16),
}

impl View {

    pub fn new() -> Self {
        Self { messages: Vec::new(), anchor: None, previous: Vec::new(), dimensions: (0, 0) }
    }

    pub fn input(&mut self, context_id: String, content: String) {
        if self.messages.iter().any(|message: &Message| message.context_id == context_id && message.role == Role::User) {
            return;
        }
        self.push(context_id, Role::User, plain(&content));
    }

    pub fn notice(&mut self, content: String) {
        self.push(String::new(), Role::Notice, plain(&content));
    }

    fn push(&mut self, context_id: String, role: Role, content: String) {
        self.messages.push(Message { context_id, role, visible: content.len(), content, next_chunk: 0, ended: true });
    }

    fn output(&mut self, context_id: &str) -> &mut Message {
        let index: usize = match self.messages.iter().position(|message: &Message| message.context_id == context_id && message.role == Role::Instance) {
            Some(index) => index,
            None => {
                self.messages.push(Message { context_id: context_id.into(), role: Role::Instance, content: String::new(), visible: 0, next_chunk: 0, ended: false });
                self.messages.len() - 1
            },
        };
        &mut self.messages[index]
    }

    pub fn event(&mut self, event: ContextEvent) -> io::Result<()> {
        match event {
            ContextEvent::Inbound { context_id, content, .. } => {
                let inbound: serde_json::Value = serde_json::from_str(&content).map_err(io::Error::other)?;
                if let Some(content) = inbound.get("content").and_then(serde_json::Value::as_str) {
                    self.input(context_id, content.to_owned());
                }
            },
            ContextEvent::Outbound { context_id, chunk_id, content_type, content, .. } => {
                let message: &mut Message = self.output(&context_id);
                if chunk_id >= message.next_chunk {
                    message.next_chunk = chunk_id + 1;
                    if matches!(content_type, ContentType::Text) {
                        message.content.push_str(&plain(&content));
                    }
                }
            },
            ContextEvent::OutboundEnd { context_id } => {
                if let Some(message) = self.messages.iter_mut().find(|message: &&mut Message| message.context_id == context_id && message.role == Role::Instance) {
                    message.ended = true;
                }
            },
        }
        Ok(())
    }

    pub fn typing(&self) -> bool {
        self.messages.iter().any(|message: &Message| message.visible < message.content.len())
    }

    pub fn advance(&mut self) {
        if let Some(message) = self.messages.iter_mut().find(|message: &&mut Message| message.visible < message.content.len())
            && let Some(character) = message.content[message.visible..].graphemes(true).next() {
            message.visible += character.len();
        }
    }

    pub fn complete(&self, context_id: &str) -> bool {
        self.messages.iter().any(|message: &Message| message.context_id == context_id && message.role == Role::Instance && message.ended && message.visible == message.content.len())
    }

    pub fn has_pending_text(&self, context_id: &str) -> bool {
        self.messages.iter().any(|message: &Message| message.context_id == context_id && message.role == Role::Instance && message.visible < message.content.len())
    }

    pub fn scroll(&mut self, amount: isize, dimensions: (u16, u16)) {
        let rows: Vec<Row> = self.rows(dimensions.0);
        let height: usize = dimensions.1.saturating_sub(2) as usize;
        let maximum: usize = rows.len().saturating_sub(height);
        let current: usize = self.top(&rows, maximum);
        let top: usize = current.saturating_add_signed(amount).min(maximum);
        self.anchor = if top == maximum { None } else { rows.get(top).map(|row: &Row| row.anchor) };
    }

    fn top(&self, rows: &[Row], maximum: usize) -> usize {
        match self.anchor {
            Some(anchor) => rows.iter().rposition(|row: &Row| row.anchor.message < anchor.message || (row.anchor.message == anchor.message && row.anchor.offset <= anchor.offset)).unwrap_or(0).min(maximum),
            None => maximum,
        }
    }

    fn rows(&self, width: u16) -> Vec<Row> {
        let mut rows: Vec<Row> = Vec::new();
        let available: usize = width.saturating_sub(4).max(1) as usize;
        for (index, message) in self.messages.iter().enumerate() {
            if message.content.is_empty() || message.visible == 0 {
                continue;
            }
            let content: &str = &message.content[..message.visible];
            for (number, range) in wrap(content, available).into_iter().enumerate() {
                rows.push(Row { anchor: Anchor { message: index, offset: range.start }, prefix: number == 0, role: message.role, text: content[range].to_owned() });
            }
        }
        rows
    }

    pub fn draw(&mut self, output: &mut impl Write, input: &str, cursor: usize, dimensions: (u16, u16)) -> io::Result<()> {
        let (width, height): (u16, u16) = dimensions;
        if width < 5 || height < 3 {
            return Ok(());
        }
        if self.dimensions != dimensions {
            queue!(output, SetBackgroundColor(Color::Black), Clear(ClearType::All))?;
            self.previous.clear();
            self.dimensions = dimensions;
        }
        let rows: Vec<Row> = self.rows(width);
        let history_height: usize = height.saturating_sub(2) as usize;
        let maximum: usize = rows.len().saturating_sub(history_height);
        let top: usize = self.top(&rows, maximum);
        let count: usize = history_height.min(rows.len().saturating_sub(top));
        let padding: usize = history_height - count;
        let mut lines: Vec<Line> = vec![Line::default(); height as usize];
        for (index, row) in rows.iter().skip(top).take(count).enumerate() {
            let prefix: &str = match (row.role, row.prefix) {
                (Role::User, true) => "> ",
                (Role::Instance, true) => "< ",
                _ => "  ",
            };
            lines[padding + index] = Line { prefix: prefix.into(), yellow: row.role == Role::Instance && row.prefix, text: row.text.clone() };
        }
        lines[height as usize - 2].text = "-".repeat(width.saturating_sub(2) as usize);
        let (visible, cursor_column): (&str, usize) = input_window(input, cursor, width.saturating_sub(4) as usize);
        lines[height as usize - 1] = Line { prefix: "> ".into(), yellow: false, text: visible.into() };
        queue!(output, Hide, SetBackgroundColor(Color::Black))?;
        for (index, line) in lines.iter().enumerate() {
            if self.previous.get(index) != Some(line) {
                queue!(output, MoveTo(0, index as u16), Clear(ClearType::CurrentLine), MoveTo(1, index as u16), SetForegroundColor(if line.yellow { YELLOW } else { WHITE }), Print(&line.prefix), SetForegroundColor(WHITE), Print(&line.text))?;
            }
        }
        queue!(output, SetForegroundColor(WHITE), MoveTo((3 + cursor_column) as u16, height - 1), Show)?;
        output.flush()?;
        self.previous = lines;
        Ok(())
    }

}

fn plain(content: &str) -> String {
    content.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ").chars().filter(|character: &char| *character == '\n' || !character.is_control()).collect()
}

fn wrap(content: &str, width: usize) -> Vec<Range<usize>> {
    let mut rows: Vec<Range<usize>> = Vec::new();
    let mut start: usize = 0;
    let mut columns: usize = 0;
    for (offset, character) in content.grapheme_indices(true) {
        if character == "\n" {
            rows.push(start..offset);
            start = offset + character.len();
            columns = 0;
        }
        else {
            let length: usize = character.width();
            if columns + length > width && offset > start {
                rows.push(start..offset);
                start = offset;
                columns = 0;
            }
            columns += length;
        }
    }
    rows.push(start..content.len());
    rows
}

fn input_window(input: &str, cursor: usize, width: usize) -> (&str, usize) {
    let mut start: usize = cursor;
    let mut columns: usize = 0;
    for (offset, character) in input[..cursor].grapheme_indices(true).rev() {
        let length: usize = character.width();
        if columns + length >= width {
            break;
        }
        start = offset;
        columns += length;
    }
    let mut end: usize = cursor;
    let mut remaining: usize = width.saturating_sub(columns);
    for character in input[cursor..].graphemes(true) {
        let length: usize = character.width();
        if length > remaining {
            break;
        }
        end += character.len();
        remaining -= length;
    }
    (&input[start..end], columns)
}
