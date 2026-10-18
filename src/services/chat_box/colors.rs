//! Name colors: the Twitch default colors, and the lightening of the colors
//! that are too dark to read.

use twitch_irc::message::RGBColor;

/// The colors that Twitch gives to the users that did not select one.
const DEFAULT_COLORS: &[&str] = &[
    "#FF0000", "#0000FF", "#00FF00", "#B22222", "#FF7F50", "#9ACD32", "#FF4500", "#2E8B57",
    "#DAA520", "#D2691E", "#5F9EA0", "#1E90FF", "#FF69B4", "#8A2BE2", "#00FF7F",
];

/// The relative luminance of a name color must be at least this, so that
/// it is readable with the black outline on a dark stream (a contrast of
/// 3:1 with black).
const MIN_LUMINANCE: f64 = 0.1;

pub fn parse_hex(hex: &str) -> Option<RGBColor> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    Some(RGBColor {
        r: channel(0)?,
        g: channel(2)?,
        b: channel(4)?,
    })
}

fn luminance([r, g, b]: [f64; 3]) -> f64 {
    let linear = |c: f64| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

fn to_hsl([r, g, b]: [f64; 3]) -> [f64; 3] {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d == 0.0 {
        return [0.0, 0.0, l];
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    [h * 60.0, s, l]
}

fn from_hsl([h, s, l]: [f64; 3]) -> [f64; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

/// The color with the same hue and saturation, and the least HSL
/// lightness that gives [`MIN_LUMINANCE`].
pub fn readable(color: RGBColor) -> RGBColor {
    let rgb = [color.r, color.g, color.b].map(|c| c as f64 / 255.0);
    if luminance(rgb) >= MIN_LUMINANCE {
        return color;
    }
    let [h, s, mut low] = to_hsl(rgb);
    let mut high = 1.0;
    for _ in 0..20 {
        let mid = (low + high) / 2.0;
        if luminance(from_hsl([h, s, mid])) >= MIN_LUMINANCE {
            high = mid;
        } else {
            low = mid;
        }
    }
    let [r, g, b] = from_hsl([h, s, high]).map(|c| (c * 255.0).round().clamp(0.0, 255.0) as u8);
    RGBColor { r, g, b }
}

/// A Twitch default color, chosen by the login.
pub fn default_color(login: &str) -> RGBColor {
    let hash = login.bytes().fold(0usize, |hash, b| {
        hash.wrapping_mul(31).wrapping_add(b as usize)
    });
    parse_hex(DEFAULT_COLORS[hash % DEFAULT_COLORS.len()]).expect("valid default colors")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(hex: &str) -> RGBColor {
        parse_hex(hex).unwrap()
    }

    #[test]
    fn default_color_is_stable() {
        assert_eq!(default_color("x"), default_color("x"));
        assert!(DEFAULT_COLORS.contains(&&*default_color("x").to_string()));
    }

    #[test]
    fn dark_colors_are_lightened() {
        // readable colors do not change
        for color in ["#FF0000", "#8A2BE2", "#2E8B57", "#FAFAFA"] {
            assert_eq!(readable(rgb(color)).to_string(), color);
        }
        for color in ["#0A0000", "#000000", "#0000FF", "#1A1A40"] {
            let light = readable(rgb(color));
            let channels = [light.r, light.g, light.b].map(|c| c as f64 / 255.0);
            let l = luminance(channels);
            assert!(
                (MIN_LUMINANCE - 0.005..MIN_LUMINANCE + 0.02).contains(&l),
                "{color} \u{2192} {light}"
            );
        }
        // the hue stays
        let red = readable(rgb("#0A0000"));
        assert!(red.r > 0 && red.g == 0 && red.b == 0, "{red}");
        // grays stay gray
        let gray = readable(rgb("#000000"));
        assert!(gray.r == gray.g && gray.g == gray.b, "{gray}");
    }

    #[test]
    fn hex() {
        assert_eq!(
            parse_hex("#0A0B0C"),
            Some(RGBColor {
                r: 10,
                g: 11,
                b: 12
            })
        );
        assert_eq!(parse_hex("0A0B0C"), None);
        assert_eq!(parse_hex("#0A0B0"), None);
        assert_eq!(parse_hex("#0A0B0Z"), None);
    }
}
