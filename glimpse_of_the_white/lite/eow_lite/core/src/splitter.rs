use std::{collections::{HashSet, VecDeque}, fmt, io, sync::Arc};

use serde::Deserialize;

use crate::config::invalid;
use crate::protocol::ContentType;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Placement {
    Previous,
    Discard,
    Next,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum BoundaryClass {
    Soft,
    Hard,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenGroup {
    tokens: Vec<String>,
    placement: Placement,
}

#[derive(Clone, Debug)]
struct SplitToken {
    chars: Vec<char>,
    placement: Placement,
    order: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplitterConfig {
    hard_split_tokens: Vec<TokenGroup>,
    soft_split_tokens: Vec<TokenGroup>,
    white_list: Vec<String>,
    tag_begin_tokens: String,
    tag_end_tokens: String,
    buffer_chars: usize,
    target_chars: usize,
    max_chars: usize,
    min_chars: usize,
    #[serde(skip)]
    hard_tokens: Vec<SplitToken>,
    #[serde(skip)]
    soft_tokens: Vec<SplitToken>,
    #[serde(skip)]
    whitelist: Vec<Vec<char>>,
    #[serde(skip)]
    tag_begin: Vec<char>,
    #[serde(skip)]
    tag_end: Vec<char>,
    #[serde(skip)]
    max_pattern_chars: usize,
    #[serde(skip)]
    max_whitelist_chars: usize,
}

impl SplitterConfig {

    pub fn compile(&mut self) -> io::Result<()> {
        if self.buffer_chars == 0
            || self.min_chars == 0
            || self.min_chars > self.target_chars
            || self.target_chars > self.max_chars
            || self.max_chars.checked_add(self.buffer_chars).is_none()
        {
            return Err(invalid("invalid SentenceSplitter character thresholds"));
        }
        if self.tag_begin_tokens.is_empty()
            || self.tag_end_tokens.is_empty()
            || self.tag_begin_tokens == self.tag_end_tokens
        {
            return Err(invalid("invalid SentenceSplitter tag delimiters"));
        }

        let mut seen: HashSet<String> = HashSet::new();
        let mut order: usize = 0;
        let mut compile_groups = |groups: &[TokenGroup]| -> io::Result<Vec<SplitToken>> {
            let mut compiled: Vec<SplitToken> = Vec::new();
            for group in groups {
                for token in &group.tokens {
                    if token.is_empty()
                        || !seen.insert(token.clone())
                        || token == &self.tag_begin_tokens
                        || token == &self.tag_end_tokens
                    {
                        return Err(invalid(
                            "empty, duplicate, or conflicting SentenceSplitter token",
                        ));
                    }
                    compiled.push(SplitToken {
                        chars: token.chars().collect(),
                        placement: group.placement,
                        order,
                    });
                    order += 1;
                }
            }
            Ok(compiled)
        };
        self.hard_tokens = compile_groups(&self.hard_split_tokens)?;
        self.soft_tokens = compile_groups(&self.soft_split_tokens)?;
        let mut whitelist_seen: HashSet<&str> = HashSet::new();
        self.whitelist = self.white_list
            .iter()
            .map(|value: &String| {
                if value.is_empty() {
                    Err(invalid(
                        "SentenceSplitter whitelist entries must not be empty",
                    ))
                }
                else if !whitelist_seen.insert(value.as_str()) {
                    Err(invalid(format!(
                        "duplicate SentenceSplitter.white_list entry {value:?}"
                    )))
                }
                else {
                    Ok(value.chars().collect::<Vec<_>>())
                }
            })
            .collect::<io::Result<_>>()?;
        self.tag_begin = self.tag_begin_tokens.chars().collect();
        self.tag_end = self.tag_end_tokens.chars().collect();
        self.max_pattern_chars = self.hard_tokens
            .iter()
            .chain(&self.soft_tokens)
            .map(|token: &SplitToken| token.chars.len())
            .chain([self.tag_begin.len(), self.tag_end.len(), 2])
            .max()
            .unwrap_or(2);
        self.max_whitelist_chars = self.whitelist.iter().map(Vec::len).max().unwrap_or(0);
        Ok(())
    }

}
#[derive(Clone, PartialEq, Eq)]
pub struct SplitCandidate {
    content_type: ContentType,
    content: String,
}

impl SplitCandidate {

    pub fn into_parts(self) -> (ContentType, String) {
        (self.content_type, self.content)
    }

}

impl fmt::Debug for SplitCandidate {

    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SplitCandidate")
            .field("content_type", &self.content_type)
            .field("char_count", &self.content.chars().count())
            .finish()
    }

}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Normal,
    Tag,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Boundary {
    class: BoundaryClass,
    token_start: usize,
    token_end: usize,
    placement: Placement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TokenMatch {
    token_index: usize,
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchResult {
    Match(TokenMatch),
    NeedMore,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HardRun {
    first: TokenMatch,
    final_match: TokenMatch,
    run_end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HardResult {
    Run(HardRun),
    NeedMore,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Progress,
    NeedMore,
    Done,
}

struct QueuedCandidate {
    release_at: usize,
    candidate: SplitCandidate,
}

pub struct SentenceSplitter {
    config: Arc<SplitterConfig>,
    buffer: Vec<char>,
    buffer_start: usize,
    lookbehind: Vec<char>,
    cursor: usize,
    pending: Option<Boundary>,
    queued_candidates: VecDeque<QueuedCandidate>,
    mode: Mode,
    ignored_tag_begin_at_zero: bool,
}

impl fmt::Debug for SentenceSplitter {

    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SentenceSplitter")
            .field("mode", &self.mode)
            .field("buffer_chars", &self.buffer.len())
            .field("lookbehind_chars", &self.lookbehind.len())
            .field("cursor", &self.cursor)
            .field("pending_class", &self.pending.map(|pending: Boundary| pending.class))
            .finish()
    }

}

impl SentenceSplitter {

    pub fn new(config: Arc<SplitterConfig>) -> Self {
        Self {
            config,
            buffer: Vec::new(),
            buffer_start: 0,
            lookbehind: Vec::new(),
            cursor: 0,
            pending: None,
            queued_candidates: VecDeque::new(),
            mode: Mode::Normal,
            ignored_tag_begin_at_zero: false,
        }
    }

    pub fn push_str(&mut self, fragment: &str) -> Vec<SplitCandidate> {
        self.buffer.extend(fragment.chars());
        self.process(false)
    }

    pub fn finish(mut self) -> Vec<SplitCandidate> {
        self.process(true)
    }

    pub fn split(config: Arc<SplitterConfig>, text: &str) -> Vec<SplitCandidate> {
        let mut splitter: Self = Self::new(config);
        let mut output: Vec<SplitCandidate> = splitter.push_str(text);
        output.extend(splitter.finish());
        output
    }

    fn process(&mut self, eof: bool) -> Vec<SplitCandidate> {
        let mut output: Vec<SplitCandidate> = Vec::new();
        let received: usize = self.received_chars();
        while self.queued_candidates
            .front()
            .is_some_and(|queued: &QueuedCandidate| queued.release_at <= received)
        {
            let queued: QueuedCandidate = self.queued_candidates
                .pop_front()
                .expect("queue is nonempty");
            output.push(queued.candidate);
        }
        loop {
            let step: Step = match self.mode {
                Mode::Normal => self.step_normal(eof, &mut output),
                Mode::Tag => self.step_tag(eof, &mut output),
            };
            match step {
                Step::Progress => {},
                Step::NeedMore | Step::Done => break,
            }
        }
        if eof {
            output.extend(
                self.queued_candidates
                    .drain(..)
                    .map(|queued: QueuedCandidate| queued.candidate),
            );
        }
        output
    }

    fn step_normal(&mut self, eof: bool, output: &mut Vec<SplitCandidate>) -> Step {
        if let Some(pending) = self.pending
            && self.cursor > pending.token_end.saturating_add(self.config.buffer_chars)
        {
            self.emit_boundary(pending, output);
            return Step::Progress;
        }

        if self.pending.is_none() && self.cursor >= self.config.max_chars {
            return self.apply_max_rule(eof, output);
        }

        if self.cursor >= self.buffer.len() {
            if !eof {
                return Step::NeedMore;
            }
            if let Some(pending) = self.pending {
                self.emit_boundary(pending, output);
                return Step::Progress;
            }
            if self.buffer.is_empty() {
                return Step::Done;
            }
            self.emit_remainder(output);
            return Step::Progress;
        }

        if !(self.ignored_tag_begin_at_zero && self.cursor == 0) {
            match self.match_literal(&self.config.tag_begin, self.cursor, eof) {
                MatchResult::Match(found) => {
                    self.enter_tag(found.start, output);
                    return Step::Progress;
                },
                MatchResult::NeedMore => return Step::NeedMore,
                MatchResult::None => {},
            }
        }

        match self.hard_run_at(self.cursor, eof) {
            HardResult::NeedMore => return Step::NeedMore,
            HardResult::Run(run) => {
                let forward_limit: usize = self.config.max_chars
                    .saturating_add(self.config.buffer_chars);
                if run.final_match.start >= forward_limit {
                    if let Some(pending) = self.pending {
                        self.emit_boundary(pending, output);
                    }
                    else {
                        self.cursor = self.config.max_chars;
                    }
                    return Step::Progress;
                }
                self.cursor = run.run_end;
                let token: &SplitToken = &self.config.hard_tokens[run.final_match.token_index];
                if !self.whitelist_suppresses(run.final_match.start, run.final_match.end)
                    && !self.hard_following_suppresses(run.run_end, eof)
                    && run.final_match.end >= self.config.min_chars
                {
                    self.schedule(Boundary {
                        class: BoundaryClass::Hard,
                        token_start: run.final_match.start,
                        token_end: run.final_match.end,
                        placement: token.placement,
                    });
                }
                return Step::Progress;
            },
            HardResult::None => {},
        }

        match self.best_token_at(&self.config.soft_tokens, self.cursor, eof) {
            MatchResult::NeedMore => Step::NeedMore,
            MatchResult::Match(found) => {
                self.cursor = found.end;
                if !self.whitelist_suppresses(found.start, found.end)
                    && found.end >= self.config.target_chars
                {
                    let token: &SplitToken = &self.config.soft_tokens[found.token_index];
                    self.schedule(Boundary {
                        class: BoundaryClass::Soft,
                        token_start: found.start,
                        token_end: found.end,
                        placement: token.placement,
                    });
                }
                Step::Progress
            },
            MatchResult::None => {
                self.cursor += 1;
                Step::Progress
            },
        }
    }

    fn step_tag(&mut self, eof: bool, output: &mut Vec<SplitCandidate>) -> Step {
        let search_limit: usize = self.config.max_chars
            .saturating_add(self.config.buffer_chars);
        if self.cursor >= search_limit {
            self.abandon_tag();
            return Step::Progress;
        }
        if self.cursor >= self.buffer.len() {
            if eof {
                self.abandon_tag();
                return Step::Progress;
            }
            return Step::NeedMore;
        }
        match self.match_literal(&self.config.tag_end, self.cursor, eof) {
            MatchResult::Match(found) => {
                self.close_tag(found.end, output);
                Step::Progress
            },
            MatchResult::NeedMore => Step::NeedMore,
            MatchResult::None => {
                self.cursor += 1;
                Step::Progress
            },
        }
    }

    fn best_token_at(&self, tokens: &[SplitToken], start: usize, eof: bool) -> MatchResult {
        let mut best: Option<TokenMatch> = None;
        let mut incomplete: bool = false;
        for (token_index, token) in tokens.iter().enumerate() {
            let available: usize = self.buffer.len().saturating_sub(start);
            let compared: usize = available.min(token.chars.len());
            if self.buffer[start..start + compared] != token.chars[..compared] {
                continue;
            }
            if available < token.chars.len() {
                incomplete = !eof;
                continue;
            }
            let candidate: TokenMatch = TokenMatch {
                token_index,
                start,
                end: start + token.chars.len(),
            };
            let replaces: bool = best.is_none_or(|current: TokenMatch| {
                let current: &SplitToken = &tokens[current.token_index];
                token.chars.len() > current.chars.len()
                    || (token.chars.len() == current.chars.len() && token.order < current.order)
            });
            if replaces {
                best = Some(candidate);
            }
        }
        if incomplete {
            MatchResult::NeedMore
        }
        else if let Some(found) = best {
            MatchResult::Match(found)
        }
        else {
            MatchResult::None
        }
    }

    fn match_literal(&self, literal: &[char], start: usize, eof: bool) -> MatchResult {
        let available: usize = self.buffer.len().saturating_sub(start);
        let compared: usize = available.min(literal.len());
        if self.buffer[start..start + compared] != literal[..compared] {
            return MatchResult::None;
        }
        if available < literal.len() {
            if eof {
                MatchResult::None
            }
            else {
                MatchResult::NeedMore
            }
        }
        else {
            MatchResult::Match(TokenMatch {
                token_index: 0,
                start,
                end: start + literal.len(),
            })
        }
    }

    fn hard_run_at(&self, start: usize, eof: bool) -> HardResult {
        let first: TokenMatch = match self.best_token_at(&self.config.hard_tokens, start, eof) {
            MatchResult::Match(found) => found,
            MatchResult::NeedMore => return HardResult::NeedMore,
            MatchResult::None => return HardResult::None,
        };
        let mut final_match: TokenMatch = first;
        let mut run_end: usize = first.end;
        loop {
            if run_end == self.buffer.len() {
                if eof {
                    break;
                }
                return HardResult::NeedMore;
            }
            match self.best_token_at(&self.config.hard_tokens, run_end, eof) {
                MatchResult::Match(found) => {
                    final_match = found;
                    run_end = found.end;
                },
                MatchResult::NeedMore => return HardResult::NeedMore,
                MatchResult::None => break,
            }
        }
        HardResult::Run(HardRun {
            first,
            final_match,
            run_end,
        })
    }

    fn whitelist_suppresses(&self, token_start: usize, token_end: usize) -> bool {
        let token_len: usize = token_end.saturating_sub(token_start);
        self.config.whitelist
            .iter()
            .any(|literal: &Vec<char>| literal.len() >= token_len && self.source_ends_with(token_end, literal))
    }

    fn source_ends_with(&self, buffer_end: usize, literal: &[char]) -> bool {
        let available: usize = self.lookbehind.len().saturating_add(buffer_end);
        if literal.len() > available {
            return false;
        }
        let source_start: usize = available - literal.len();
        literal.iter().enumerate().all(|(offset, expected): (usize, &char)| {
            let source_index: usize = source_start + offset;
            let actual: char = if source_index < self.lookbehind.len() {
                self.lookbehind[source_index]
            }
            else {
                self.buffer[source_index - self.lookbehind.len()]
            };
            actual == *expected
        })
    }

    fn hard_following_suppresses(&self, run_end: usize, eof: bool) -> bool {
        if self.buffer[run_end - 1].is_whitespace() {
            return false;
        }
        match self.buffer.get(run_end) {
            Some(character) => !character.is_whitespace(),
            None => !eof,
        }
    }

    fn schedule(&mut self, boundary: Boundary) {
        let replace: bool = self.pending.is_none_or(|current: Boundary| {
            boundary.class > current.class
                || (boundary.class == current.class && boundary.token_end >= current.token_end)
        });
        if replace {
            self.pending = Some(boundary);
        }
    }

    fn emit_boundary(&mut self, boundary: Boundary, output: &mut Vec<SplitCandidate>) {
        let (emit_end, consume_end, next_cursor): (usize, usize, usize) = match boundary.placement {
            Placement::Previous => (boundary.token_end, boundary.token_end, 0),
            Placement::Discard => (boundary.token_start, boundary.token_end, 0),
            Placement::Next => (
                boundary.token_start,
                boundary.token_start,
                boundary.token_end - boundary.token_start,
            ),
        };
        self.push_candidate(ContentType::Text, emit_end, None, output);
        self.consume_prefix(consume_end);
        self.cursor = next_cursor;
        self.pending = None;
        self.mode = Mode::Normal;
    }

    fn enter_tag(&mut self, begin_start: usize, output: &mut Vec<SplitCandidate>) {
        let begin_end: usize = begin_start + self.config.tag_begin.len();
        self.push_candidate(ContentType::Text, begin_start, Some(begin_end), output);
        self.consume_prefix(begin_start);
        self.mode = Mode::Tag;
        self.pending = None;
        self.cursor = self.config.tag_begin.len();
        self.ignored_tag_begin_at_zero = false;
    }

    fn abandon_tag(&mut self) {
        self.mode = Mode::Normal;
        self.pending = None;
        self.cursor = 0;
        self.ignored_tag_begin_at_zero = true;
    }

    fn close_tag(&mut self, end: usize, output: &mut Vec<SplitCandidate>) {
        self.push_candidate(ContentType::Tag, end, Some(end), output);
        self.consume_prefix(end);
        self.mode = Mode::Normal;
        self.pending = None;
        self.cursor = 0;
        self.ignored_tag_begin_at_zero = false;
    }

    fn emit_remainder(&mut self, output: &mut Vec<SplitCandidate>) {
        let end: usize = self.buffer.len();
        self.push_candidate(ContentType::Text, end, None, output);
        self.consume_prefix(end);
        self.cursor = 0;
        self.pending = None;
        self.ignored_tag_begin_at_zero = false;
    }

    fn push_candidate(&mut self, content_type: ContentType, content_end: usize, delimiter_end: Option<usize>, output: &mut Vec<SplitCandidate>) {
        let content: &[char] = &self.buffer[..content_end];
        if content.is_empty() || content.iter().all(|character: &char| character.is_whitespace()) {
            return;
        }
        let candidate: SplitCandidate = SplitCandidate {
            content_type,
            content: content.iter().collect(),
        };
        let release_offset: usize = delimiter_end.map_or(content_end, |end: usize| {
            end.saturating_add(self.config.buffer_chars)
        });
        let release_at: usize = self.buffer_start.saturating_add(release_offset);
        if self.queued_candidates.is_empty() && release_at <= self.received_chars() {
            output.push(candidate);
        }
        else {
            self.queued_candidates.push_back(QueuedCandidate {
                release_at,
                candidate,
            });
        }
    }

    fn received_chars(&self) -> usize {
        self.buffer_start.saturating_add(self.buffer.len())
    }

    fn consume_prefix(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        if self.config.max_whitelist_chars > 0 {
            self.lookbehind.extend_from_slice(&self.buffer[..count]);
            let keep: usize = self.config.max_whitelist_chars;
            if self.lookbehind.len() > keep {
                let remove: usize = self.lookbehind.len() - keep;
                self.lookbehind.drain(..remove);
            }
        }
        self.buffer.drain(..count);
        self.buffer_start = self.buffer_start.saturating_add(count);
        self.ignored_tag_begin_at_zero = false;
    }

    fn apply_max_rule(&mut self, eof: bool, output: &mut Vec<SplitCandidate>) -> Step {
        let touch: usize = self.config.max_chars;
        let forward_limit: usize = touch.saturating_add(self.config.buffer_chars);
        let required: usize = forward_limit
            .saturating_add(self.config.max_pattern_chars)
            .saturating_add(1);
        if !eof && self.buffer.len() < required {
            return Step::NeedMore;
        }
        let available_limit: usize = forward_limit.min(self.buffer.len());

        if let Some(start) = self.find_forward_tag(touch, available_limit) {
            self.enter_tag(start, output);
            return Step::Progress;
        }
        if let Some(boundary) = self.find_forward_hard(touch, available_limit, eof) {
            self.emit_boundary(boundary, output);
            return Step::Progress;
        }
        if let Some(boundary) = self.find_forward_soft(touch, available_limit, eof) {
            self.emit_boundary(boundary, output);
            return Step::Progress;
        }
        if let Some(end) = self.find_forward_line_break(touch, available_limit) {
            self.emit_plain_boundary(end, output);
            return Step::Progress;
        }
        if let Some(end) = self.find_forward_whitespace(touch, available_limit) {
            self.emit_plain_boundary(end, output);
            return Step::Progress;
        }
        if let Some(end) = self.find_backward_line_break(touch) {
            self.emit_plain_boundary(end, output);
            return Step::Progress;
        }
        if let Some(end) = self.find_backward_whitespace(touch) {
            self.emit_plain_boundary(end, output);
            return Step::Progress;
        }
        self.emit_plain_boundary(touch.min(self.buffer.len()), output);
        Step::Progress
    }

    fn find_forward_tag(&self, start: usize, limit: usize) -> Option<usize> {
        (start..limit).find(|position: &usize| {
            matches!(
                self.match_literal(&self.config.tag_begin, *position, true),
                MatchResult::Match(_)
            )
        })
    }

    fn find_forward_hard(&self, start: usize, limit: usize, eof: bool) -> Option<Boundary> {
        for position in start..limit {
            if let HardResult::Run(run) = self.hard_run_at(position, eof) {
                if run.final_match.start >= limit {
                    continue;
                }
                if self.whitelist_suppresses(run.final_match.start, run.final_match.end)
                    || self.hard_following_suppresses(run.run_end, eof)
                {
                    continue;
                }
                let token: &SplitToken = &self.config.hard_tokens[run.final_match.token_index];
                return Some(Boundary {
                    class: BoundaryClass::Hard,
                    token_start: run.final_match.start,
                    token_end: run.final_match.end,
                    placement: token.placement,
                });
            }
        }
        None
    }

    fn find_forward_soft(&self, start: usize, limit: usize, eof: bool) -> Option<Boundary> {
        for position in start..limit {
            if let MatchResult::Match(found) =
                self.best_token_at(&self.config.soft_tokens, position, eof)
            {
                if self.whitelist_suppresses(found.start, found.end) {
                    continue;
                }
                let token: &SplitToken = &self.config.soft_tokens[found.token_index];
                return Some(Boundary {
                    class: BoundaryClass::Soft,
                    token_start: found.start,
                    token_end: found.end,
                    placement: token.placement,
                });
            }
        }
        None
    }

    fn find_forward_line_break(&self, start: usize, limit: usize) -> Option<usize> {
        for position in start..limit {
            match self.buffer[position] {
                '\r' => {
                    return Some(
                        position + usize::from(self.buffer.get(position + 1) == Some(&'\n')) + 1,
                    );
                },
                '\n' => return Some(position + 1),
                _ => {},
            }
        }
        None
    }

    fn find_forward_whitespace(&self, start: usize, limit: usize) -> Option<usize> {
        (start..limit)
            .find(|position: &usize| {
                let character: char = self.buffer[*position];
                character != '\r' && character != '\n' && character.is_whitespace()
            })
            .map(|position: usize| position + 1)
    }

    fn find_backward_line_break(&self, touch: usize) -> Option<usize> {
        let start: usize = touch.saturating_sub(self.config.buffer_chars);
        let mut nearest: Option<usize> = None;
        for position in start..touch.min(self.buffer.len()) {
            match self.buffer[position] {
                '\r' => {
                    nearest = Some(
                        position + usize::from(self.buffer.get(position + 1) == Some(&'\n')) + 1,
                    );
                },
                '\n' if position == 0 || self.buffer[position - 1] != '\r' => {
                    nearest = Some(position + 1);
                },
                _ => {},
            }
        }
        nearest
    }

    fn find_backward_whitespace(&self, touch: usize) -> Option<usize> {
        let start: usize = touch.saturating_sub(self.config.buffer_chars);
        (start..touch.min(self.buffer.len()))
            .rev()
            .find(|position: &usize| {
                let character: char = self.buffer[*position];
                character != '\r' && character != '\n' && character.is_whitespace()
            })
            .map(|position: usize| position + 1)
    }

    fn emit_plain_boundary(&mut self, end: usize, output: &mut Vec<SplitCandidate>) {
        self.push_candidate(ContentType::Text, end, None, output);
        self.consume_prefix(end);
        self.cursor = 0;
        self.pending = None;
        self.mode = Mode::Normal;
    }

}
