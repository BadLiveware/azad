//! A line of output with terminal escapes as segments of text with classes: the 16 basic colors and bold of SGR escapes
//! become classes, and other escapes are left out. The page escapes the text, as it does all text.

use crate::model::{Line, Segment};

const ESCAPE: char = '\u{1b}';
const CODE_START: char = '[';
const STYLE_END: char = 'm';
const FINAL_BYTES: std::ops::RangeInclusive<char> = '@'..='~';
const RESET: u32 = 0;
const BOLD: u32 = 1;
const NORMAL: u32 = 22;
const DEFAULT_COLOR: u32 = 39;
const EXTENDED_COLOR: u32 = 38;
const EXTENDED_PALETTE: u32 = 5;
const EXTENDED_RGB: u32 = 2;
const COLORS: std::ops::RangeInclusive<u32> = 30..=37;
const BRIGHT_COLORS: std::ops::RangeInclusive<u32> = 90..=97;
const BRIGHT_OFFSET: u32 = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Style {
    color: Option<u32>,
    bold: bool,
}

impl Style {
    fn apply(&mut self, codes: &[u32]) {
        let mut codes = codes.iter().copied();
        while let Some(code) = codes.next() {
            match code {
                RESET => *self = Style::default(),
                BOLD => self.bold = true,
                NORMAL => self.bold = false,
                DEFAULT_COLOR => self.color = None,
                EXTENDED_COLOR => {
                    let skipped = match codes.next() {
                        Some(EXTENDED_PALETTE) => 1,
                        Some(EXTENDED_RGB) => 3,
                        _ => 0,
                    };
                    codes.by_ref().take(skipped).for_each(drop);
                }
                code if COLORS.contains(&code) => self.color = Some(code - COLORS.start()),
                code if BRIGHT_COLORS.contains(&code) => {
                    self.color = Some(code - BRIGHT_COLORS.start() + BRIGHT_OFFSET)
                }
                _ => {}
            }
        }
    }

    fn class(self) -> String {
        let color = self.color.map(|color| format!("c{color}"));
        let bold = self.bold.then(|| "b".to_owned());
        color.into_iter().chain(bold).collect::<Vec<_>>().join(" ")
    }
}

pub fn segments(line: &str) -> Line {
    let mut segments = Vec::new();
    let mut style = Style::default();
    let mut text = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != ESCAPE {
            text.push(c);
            continue;
        }
        if chars.peek() != Some(&CODE_START) {
            chars.next();
            continue;
        }
        chars.next();
        let mut parameters = String::new();
        let mut end = None;
        for next in chars.by_ref() {
            if FINAL_BYTES.contains(&next) {
                end = Some(next);
                break;
            }
            parameters.push(next);
        }
        if end != Some(STYLE_END) {
            continue;
        }
        if !text.is_empty() {
            segments.push(Segment { text: std::mem::take(&mut text), class: style.class() });
        }
        let codes: Vec<u32> = parameters.split(';').map(|code| code.parse().unwrap_or(RESET)).collect();
        style.apply(&codes);
    }
    if !text.is_empty() {
        segments.push(Segment { text, class: style.class() });
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(line: &str) -> Vec<(String, String)> {
        segments(line).into_iter().map(|segment| (segment.text, segment.class)).collect()
    }

    #[test]
    fn colors_and_bold_become_classes_and_other_escapes_are_left_out() {
        let pair = |text: &str, class: &str| (text.to_owned(), class.to_owned());
        assert_eq!(shown("\u{1b}[1;31merror\u{1b}[0m: no"), [pair("error", "c1 b"), pair(": no", "")]);
        assert_eq!(shown("\u{1b}[38;5;208mx\u{1b}[2Ky"), [pair("xy", "")]);
        assert_eq!(shown("\u{1b}[92mok"), [pair("ok", "c10")]);
        assert_eq!(shown("<b> & \"x\""), [pair("<b> & \"x\"", "")]);
    }
}
