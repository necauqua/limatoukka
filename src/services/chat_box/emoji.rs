//! Emoji in the text, shown as Twemoji images.

use unicode_segmentation::UnicodeSegmentation;

/// The Twemoji version of the images
const TWEMOJI: &str = "https://cdn.jsdelivr.net/gh/jdecked/twemoji@16.0.1/assets/svg";

/// Text presentation: the bot writes `⚡︎` so that it is not an emoji
const TEXT_SELECTOR: char = '\u{FE0E}';
const EMOJI_SELECTOR: char = '\u{FE0F}';
const ZWJ: char = '\u{200D}';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Run<'a> {
    Text(&'a str),
    Emoji(&'a str),
}

/// Whether a grapheme cluster is an emoji. ASCII and `©`, `®`, `™` are
/// emoji only with the emoji selector, as in Twemoji.
fn is_emoji(grapheme: &str) -> bool {
    if grapheme.contains(TEXT_SELECTOR) {
        return false;
    }
    let mut chars = grapheme.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && (c.is_ascii() || matches!(c, '©' | '®' | '™'))
    {
        return false;
    }
    let bare = grapheme.replace(EMOJI_SELECTOR, "");
    emojis::get(grapheme).is_some()
        || emojis::get(&bare).is_some()
        || emojis::get(&format!("{bare}{EMOJI_SELECTOR}")).is_some()
}

/// The text split into text and emoji.
pub fn split(text: &str) -> Vec<Run<'_>> {
    let mut runs = Vec::new();
    let mut text_start = None;
    for (i, grapheme) in text.grapheme_indices(true) {
        if is_emoji(grapheme) {
            if let Some(start) = text_start.take() {
                runs.push(Run::Text(&text[start..i]));
            }
            runs.push(Run::Emoji(grapheme));
        } else if text_start.is_none() {
            text_start = Some(i);
        }
    }
    if let Some(start) = text_start {
        runs.push(Run::Text(&text[start..]));
    }
    runs
}

/// The Twemoji image: the code points in hex, without the emoji selector
/// when there is no zero-width joiner.
pub fn twemoji_url(emoji: &str) -> String {
    let keep_selector = emoji.contains(ZWJ);
    let name = emoji
        .chars()
        .filter(|c| keep_selector || *c != EMOJI_SELECTOR)
        .map(|c| format!("{:x}", c as u32))
        .collect::<Vec<_>>()
        .join("-");
    format!("{TWEMOJI}/{name}.svg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits() {
        assert_eq!(
            split("hi 👋🏻 there❤️!"),
            [
                Run::Text("hi "),
                Run::Emoji("👋🏻"),
                Run::Text(" there"),
                Run::Emoji("❤️"),
                Run::Text("!"),
            ]
        );
        assert_eq!(split("🇺🇦"), [Run::Emoji("🇺🇦")]);
        // text
        for text in [
            "704.098⚡\u{FE0E}",
            "a © b ™ 1 #",
            "→ stream start cutoff ←",
            "привіт 日本語",
        ] {
            assert_eq!(split(text), [Run::Text(text)], "{text}");
        }
        // a text-default emoji without the selector is still an emoji
        assert_eq!(split("⚡"), [Run::Emoji("⚡")]);
    }

    #[test]
    fn urls() {
        assert!(twemoji_url("👋").ends_with("/1f44b.svg"));
        assert!(twemoji_url("👋🏻").ends_with("/1f44b-1f3fb.svg"));
        assert!(twemoji_url("❤️").ends_with("/2764.svg"));
        assert!(twemoji_url("1️⃣").ends_with("/31-20e3.svg"));
        // with a joiner, the selector stays
        assert!(twemoji_url("🏳️‍🌈").ends_with("/1f3f3-fe0f-200d-1f308.svg"));
    }
}
