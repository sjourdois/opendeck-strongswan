//! The key image: a globe in the key's colour over its label, one look per
//! [`Shown`] state.
//!
//! The plugin draws the whole key rather than switching manifest states, since
//! neither the colour nor the label's height can be set any other way: OpenDeck
//! places a title only at the top, middle or bottom edge of the key.

use base64::Engine;

use crate::vpn::Shown;

/// The globe's colour when the settings give none, or give something that is
/// not a hex colour.
pub const DEFAULT_COLOR: &str = "#2ea44f";

/// Globe and slash colours for [`Shown::Unavailable`], whatever the key's colour.
const UNAVAILABLE_GREY: &str = "#8a8a8a";
const UNAVAILABLE_SLASH: &str = "#e5484d";

/// The label's size when it fits, and the smallest it shrinks to before it is
/// cut short instead.
const LABEL_SIZE: f32 = 22.0;
const LABEL_MIN_SIZE: f32 = 11.0;
/// Width the label may take, and a bold sans-serif glyph's average advance as a
/// fraction of the font size, to shrink a long label before it overflows.
const LABEL_WIDTH: f32 = 118.0;
const GLYPH_ADVANCE: f32 = 0.62;

/// `color` if it is a `#rgb` / `#rrggbb` hex colour — the only shape that is
/// safe to splice into the SVG — else [`DEFAULT_COLOR`].
pub fn color(color: Option<&str>) -> &str {
	match color {
		Some(c) if is_hex_color(c) => c,
		_ => DEFAULT_COLOR,
	}
}

fn is_hex_color(s: &str) -> bool {
	s.strip_prefix('#')
		.is_some_and(|h| matches!(h.len(), 3 | 6) && h.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The key image for a connection in state `shown`, as a data URI for
/// `setImage`. `color` must come from [`color`].
pub fn key(shown: Shown, color: &str, label: &str) -> String {
	let body = match shown {
		Shown::Up => globe("#ffffff", color, ""),
		Shown::Down => format!(r#"<g opacity="0.45">{}</g>"#, globe(color, "none", "")),
		Shown::Busy => globe(color, "none", r#" stroke-dasharray="9 6""#),
		Shown::Unavailable => format!(
			r#"<g opacity="0.45">{}</g><path d="M36 78 L92 10" stroke="{UNAVAILABLE_SLASH}" stroke-width="7" stroke-linecap="round"/>"#,
			globe(UNAVAILABLE_GREY, "none", "")
		),
	};
	let svg = format!(
		r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128">{body}{}</svg>"#,
		text(label)
	);
	let b64 = base64::engine::general_purpose::STANDARD.encode(svg.as_bytes());
	format!("data:image/svg+xml;base64,{b64}")
}

/// A globe centred in the top two thirds of the key: its outline in `stroke`,
/// filled with `fill`, meridian and parallels in `stroke` too. `outline` adds
/// attributes to the outer circle only.
fn globe(stroke: &str, fill: &str, outline: &str) -> String {
	format!(
		r#"<circle cx="64" cy="44" r="30" fill="{fill}" stroke="{stroke}" stroke-width="5"{outline}/><g fill="none" stroke="{stroke}" stroke-width="4"><ellipse cx="64" cy="44" rx="12" ry="30"/><path d="M34 44 H94 M39 29 H89 M39 59 H89"/></g>"#
	)
}

/// The label under the globe, shrunk to fit the key's width, and cut short
/// with an ellipsis once shrinking would make it unreadable.
fn text(label: &str) -> String {
	if label.is_empty() {
		return String::new();
	}
	let max_chars = (LABEL_WIDTH / (GLYPH_ADVANCE * LABEL_MIN_SIZE)) as usize;
	let mut chars: Vec<char> = label.chars().collect();
	if chars.len() > max_chars {
		chars.truncate(max_chars - 1);
		chars.push('…');
	}
	let fit = LABEL_WIDTH / (GLYPH_ADVANCE * chars.len() as f32);
	let size = fit.clamp(LABEL_MIN_SIZE, LABEL_SIZE).floor();
	let text = escape(&chars.into_iter().collect::<String>());
	format!(
		r##"<text x="64" y="104" font-family="sans-serif" font-size="{size}" font-weight="bold" fill="#ffffff" text-anchor="middle">{text}</text>"##
	)
}

fn escape(s: &str) -> String {
	s.replace('&', "&amp;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
		.replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
	use super::*;

	fn svg(uri: &str) -> String {
		let b64 = uri.strip_prefix("data:image/svg+xml;base64,").unwrap();
		String::from_utf8(
			base64::engine::general_purpose::STANDARD
				.decode(b64)
				.unwrap(),
		)
		.unwrap()
	}

	#[test]
	fn only_hex_colours_are_used() {
		assert_eq!(color(Some("#3b82f6")), "#3b82f6");
		assert_eq!(color(Some("#abc")), "#abc");
		assert_eq!(color(Some(r#"red" onload="x"#)), DEFAULT_COLOR);
		assert_eq!(color(Some("")), DEFAULT_COLOR);
		assert_eq!(color(None), DEFAULT_COLOR);
	}

	#[test]
	fn the_colour_tints_every_state_but_unavailable() {
		for shown in [Shown::Up, Shown::Down, Shown::Busy] {
			assert!(svg(&key(shown, "#3b82f6", "work1")).contains("#3b82f6"));
		}
		assert!(!svg(&key(Shown::Unavailable, "#3b82f6", "work1")).contains("#3b82f6"));
	}

	#[test]
	fn the_label_is_escaped() {
		let s = svg(&key(Shown::Up, DEFAULT_COLOR, "a<b&c"));
		assert!(s.contains(">a&lt;b&amp;c</text>"));
	}

	#[test]
	fn a_long_label_shrinks_then_is_cut_short() {
		let short = text("work1");
		assert!(short.contains(r#"font-size="22""#));
		let long = text("a-rather-long-connection");
		assert!(long.contains(r#"font-size="11""#));
		assert!(long.contains('…'));
		assert!(text("").is_empty());
	}
}
