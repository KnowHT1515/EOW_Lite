use std::{collections::BTreeMap, io::{self, Write}, ops::Range, time::Duration};

use crossterm::{cursor::{Hide, MoveTo, Show}, queue, style::{Color, Print, SetBackgroundColor, SetForegroundColor}, terminal::{Clear, ClearType}};
use tokio::time::Instant;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::protocol::{ContentType, ContextEvent};

const WHITE: Color = Color::Rgb { r: 255, g: 255, b: 255 };
const YELLOW: Color = Color::Rgb { r: 255, g: 255, b: 0 };
const OUTBOUND_MISSING: &str = "*Outbound Missing";
const MISSING_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq)]
enum Role {
    User,
    Instance,
    Notice,
}

#[derive(Default)]
struct RenderQueue {
    chunks: BTreeMap<usize, Option<String>>,
    next_chunk: usize,
    missing_at: Option<Instant>,
}

impl RenderQueue {

    fn push(&mut self, chunk_id: usize, content_type: ContentType, content: &str, output: &mut String) {
        if chunk_id < self.next_chunk {
            return;
        }
        self.chunks.entry(chunk_id).or_insert_with(|| matches!(content_type, ContentType::Text).then(|| plain(content)));
        while let Some(chunk) = self.chunks.remove(&self.next_chunk) {
            if let Some(text) = chunk {
                output.push_str(&text);
            }
            self.next_chunk += 1;
        }
    }

    fn update_deadline(&mut self, chunk_count: Option<usize>, now: Instant) {
        let missing: bool = match chunk_count {
            Some(count) => self.next_chunk < count,
            None => !self.chunks.is_empty(),
        };
        if missing {
            self.missing_at.get_or_insert(now + MISSING_WAIT);
        }
        else {
            self.missing_at = None;
        }
    }

}

struct Message {
    context_id: String,
    role: Role,
    content: String,
    visible: usize,
    render_queue: Option<RenderQueue>,
    chunk_count: Option<usize>,
    missing: bool,
}

impl Message {

    fn finish_rendering(&mut self) -> bool {
        if self.visible == self.content.len()
            && self.render_queue.as_ref().is_some_and(|queue: &RenderQueue| self.chunk_count == Some(queue.next_chunk)) {
            self.render_queue = None;
            return true;
        }
        false
    }

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
    missing: bool,
}

#[derive(Clone, Default, PartialEq)]
struct Line {
    prefix: String,
    yellow_prefix: bool,
    text: String,
    yellow_text: bool,
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
        self.messages.push(Message {
            context_id,
            role,
            visible: content.len(),
            content,
            render_queue: None,
            chunk_count: None,
            missing: false,
        });
    }

    fn output(&mut self, context_id: &str) -> &mut Message {
        let index: usize = match self.messages.iter().position(|message: &Message| message.context_id == context_id && message.role == Role::Instance) {
            Some(index) => index,
            None => {
                self.messages.push(Message {
                    context_id: context_id.into(),
                    role: Role::Instance,
                    content: String::new(),
                    visible: 0,
                    render_queue: Some(RenderQueue::default()),
                    chunk_count: None,
                    missing: false,
                });
                self.messages.len() - 1
            },
        };
        &mut self.messages[index]
    }

    pub fn event(&mut self, event: ContextEvent, now: Instant) -> io::Result<()> {
        self.expire_missing(now);
        let message: &mut Message = match event {
            ContextEvent::Inbound { context_id, content, .. } => {
                let inbound: serde_json::Value = serde_json::from_str(&content).map_err(io::Error::other)?;
                if let Some(content) = inbound.get("content").and_then(serde_json::Value::as_str) {
                    self.input(context_id, content.to_owned());
                }
                return Ok(());
            },
            ContextEvent::Outbound { context_id, chunk_id, content_type, content, .. } => {
                let message: &mut Message = self.output(&context_id);
                if let Some(queue) = &mut message.render_queue
                    && message.chunk_count.is_none_or(|count: usize| chunk_id < count) {
                    queue.push(chunk_id, content_type, &content, &mut message.content);
                }
                message
            },
            ContextEvent::OutboundEnd { context_id, chunk_count } => {
                if !self.messages.iter().any(|message: &Message| message.context_id == context_id) {
                    return Ok(());
                }
                let message: &mut Message = self.output(&context_id);
                message.chunk_count = Some(chunk_count);
                message
            },
        };
        if let Some(queue) = &mut message.render_queue {
            queue.update_deadline(message.chunk_count, now);
        }
        if message.finish_rendering() {
            self.previous.clear();
        }
        Ok(())
    }

    pub fn missing_deadline(&self) -> Option<Instant> {
        self.messages.iter().filter_map(|message: &Message| message.render_queue.as_ref()?.missing_at).min()
    }

    pub fn expire_missing(&mut self, now: Instant) {
        for message in &mut self.messages {
            if message.render_queue.as_ref().and_then(|queue: &RenderQueue| queue.missing_at).is_some_and(|deadline: Instant| deadline <= now) {
                message.content.truncate(message.visible);
                message.render_queue = None;
                message.missing = true;
            }
        }
    }

    pub fn typing(&self) -> bool {
        self.messages.iter().any(|message: &Message| message.visible < message.content.len())
    }

    pub fn advance(&mut self) {
        if let Some(message) = self.messages.iter_mut().find(|message: &&mut Message| message.visible < message.content.len())
            && let Some(character) = message.content[message.visible..].graphemes(true).next() {
            message.visible += character.len();
            if message.finish_rendering() {
                self.previous.clear();
            }
        }
    }

    pub fn complete(&self, context_id: &str) -> bool {
        self.messages.iter().any(|message: &Message| message.context_id == context_id && message.role == Role::Instance && message.chunk_count.is_some() && message.render_queue.is_none())
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
            let content: &str = &message.content[..message.visible];
            if !content.is_empty() {
                let mut ranges: Vec<Range<usize>> = wrap(content, available);
                if message.missing && content.ends_with('\n') {
                    ranges.pop();
                }
                for (number, range) in ranges.into_iter().enumerate() {
                    rows.push(Row { anchor: Anchor { message: index, offset: range.start }, prefix: number == 0, role: message.role, text: content[range].to_owned(), missing: false });
                }
            }
            if message.missing {
                for range in wrap(OUTBOUND_MISSING, available) {
                    rows.push(Row { anchor: Anchor { message: index, offset: message.visible + range.start }, prefix: false, role: message.role, text: OUTBOUND_MISSING[range].to_owned(), missing: true });
                }
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
            lines[padding + index] = Line { prefix: prefix.into(), yellow_prefix: row.role == Role::Instance && row.prefix, text: row.text.clone(), yellow_text: row.missing };
        }
        lines[height as usize - 2].text = "-".repeat(width.saturating_sub(2) as usize);
        let (visible, cursor_column): (&str, usize) = input_window(input, cursor, width.saturating_sub(4) as usize);
        lines[height as usize - 1] = Line { prefix: "> ".into(), text: visible.into(), ..Line::default() };
        queue!(output, Hide, SetBackgroundColor(Color::Black))?;
        for (index, line) in lines.iter().enumerate() {
            if self.previous.get(index) != Some(line) {
                queue!(output, MoveTo(0, index as u16), Clear(ClearType::CurrentLine), MoveTo(1, index as u16), SetForegroundColor(if line.yellow_prefix { YELLOW } else { WHITE }), Print(&line.prefix), SetForegroundColor(if line.yellow_text { YELLOW } else { WHITE }), Print(&line.text))?;
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
