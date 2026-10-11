use crate::theme::Resolved;
use pulldown_cmark::{html, CodeBlockKind, CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use syntect::highlighting::ThemeSet;
use syntect::html::{css_for_theme_with_class_style, ClassStyle, ClassedHTMLGenerator};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

const DARK_THEME: &str = "base16-ocean.dark";
const LIGHT_THEME: &str = "InspiredGitHub";
const TRANSPARENT_BACKGROUND: &str = "background-color: transparent";

fn syntaxes() -> &'static SyntaxSet {
	static SET: OnceLock<SyntaxSet> = OnceLock::new();
	SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn themes() -> &'static ThemeSet {
	static SET: OnceLock<ThemeSet> = OnceLock::new();
	SET.get_or_init(ThemeSet::load_defaults)
}

fn style_for(theme: Resolved) -> String {
	let name = match theme {
		Resolved::Dark => DARK_THEME,
		Resolved::Light => LIGHT_THEME,
	};
	let Some(theme) = themes().themes.get(name).or_else(|| themes().themes.values().next()) else {
		return String::new();
	};
	let css = css_for_theme_with_class_style(theme, ClassStyle::Spaced).unwrap_or_default();
	let mut body = css.split("*/").nth(1).unwrap_or(&css).to_string();
	let mut index = 0;
	while let Some(start) = body[index..].find("background-color:").map(|offset| index + offset) {
		let end = body[start..].find(';').map(|offset| start + offset).unwrap_or(body.len());
		body.replace_range(start..end, TRANSPARENT_BACKGROUND);
		index = start + TRANSPARENT_BACKGROUND.len();
	}
	body.replace('\t', " ")
}

pub fn styles(theme: Resolved) -> String {
	static CACHE: OnceLock<Mutex<HashMap<&'static str, String>>> = OnceLock::new();
	let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
	let key = match theme {
		Resolved::Dark => "dark",
		Resolved::Light => "light",
	};
	let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
	cache.entry(key).or_insert_with(|| style_for(theme)).clone()
}

fn highlight(code: &str, language: &str) -> Option<String> {
	let set = syntaxes();
	let syntax = set
		.find_syntax_by_token(language)
		.or_else(|| set.find_syntax_by_extension(language))
		.or_else(|| language.is_empty().then(|| set.find_syntax_plain_text()))?;
	let mut generator = ClassedHTMLGenerator::new_with_class_style(syntax, set, ClassStyle::Spaced);
	for line in LinesWithEndings::from(code) {
		generator.parse_html_for_line_which_includes_newline(line).ok()?;
	}
	Some(generator.finalize())
}

fn escape(text: &str) -> String {
	let mut out = String::with_capacity(text.len());
	for character in text.chars() {
		match character {
			'&' => out.push_str("&amp;"),
			'<' => out.push_str("&lt;"),
			'>' => out.push_str("&gt;"),
			'"' => out.push_str("&quot;"),
			'\'' => out.push_str("&#39;"),
			other => out.push(other),
		}
	}
	out
}

const ALLOWED_TAGS: &[&str] = &["br", "sub", "sup", "kbd", "mark", "small", "abbr", "wbr", "hr", "u", "s"];

fn sanitize(raw: &str) -> String {
	if !raw.contains('<') {
		return raw.to_string();
	}
	let lower = raw.to_lowercase();
	let mut out = String::with_capacity(raw.len());
	let mut index = 0;
	while let Some(offset) = raw[index..].find('<') {
		out.push_str(&raw[index..index + offset]);
		index += offset;
		let Some(end) = raw[index..].find('>') else {
			out.push_str(&escape(&raw[index..]));
			return out;
		};
		let tag = lower[index + 1..index + end].trim_start_matches('/').trim_end_matches('/');
		let name = tag.split_whitespace().next().unwrap_or("");
		if ALLOWED_TAGS.contains(&name) {
			out.push_str(&raw[index..index + end + 1]);
		}
		index += end + 1;
	}
	out.push_str(&raw[index..]);
	out
}

fn heading_id(text: &str, counts: &mut HashMap<String, u32>) -> String {
	let mut id: String = text
		.chars()
		.filter(|character| character.is_alphanumeric() || *character == ' ' || *character == '-' || *character == '_')
		.collect::<String>()
		.trim()
		.replace(' ', "-")
		.to_lowercase();
	if id.is_empty() {
		id = "section".to_string();
	}
	let count = counts.entry(id.clone()).or_insert(0);
	*count += 1;
	if *count > 1 {
		id = format!("{id}-{}", *count - 1);
	}
	id
}

fn level_of(level: HeadingLevel) -> u32 {
	match level {
		HeadingLevel::H1 => 1,
		HeadingLevel::H2 => 2,
		HeadingLevel::H3 => 3,
		HeadingLevel::H4 => 4,
		HeadingLevel::H5 => 5,
		HeadingLevel::H6 => 6,
	}
}

fn safe_url(url: &str) -> Option<String> {
	let trimmed = url.trim();
	let lowered = trimmed.to_lowercase();
	if lowered.starts_with("http://")
		|| lowered.starts_with("https://")
		|| lowered.starts_with("mailto:")
		|| lowered.starts_with("file://")
		|| lowered.starts_with("data:image/")
		|| lowered.starts_with("ash://")
	{
		return Some(trimmed.to_string());
	}
	if trimmed.starts_with('#') || trimmed.starts_with('/') || trimmed.starts_with("ui/") || trimmed.starts_with("icons/") {
		return Some(trimmed.to_string());
	}
	None
}

fn boxed(text: String) -> CowStr<'static> {
	CowStr::Boxed(text.into_boxed_str())
}

pub fn render(markdown: &str) -> String {
	let mut options = Options::empty();
	for option in [
		Options::ENABLE_TABLES,
		Options::ENABLE_FOOTNOTES,
		Options::ENABLE_STRIKETHROUGH,
		Options::ENABLE_TASKLISTS,
		Options::ENABLE_SMART_PUNCTUATION,
		Options::ENABLE_GFM,
		Options::ENABLE_MATH,
		Options::ENABLE_SUPERSCRIPT,
		Options::ENABLE_SUBSCRIPT,
	] {
		options.insert(option);
	}
	let mut counts: HashMap<String, u32> = HashMap::new();
	let mut heading: Option<String> = None;
	let mut fence: Option<(String, String)> = None;
	let mut events: Vec<Event> = Vec::new();
	for event in Parser::new_ext(markdown, options) {
		match event {
			Event::Start(Tag::CodeBlock(kind)) => {
				let language = match kind {
					CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("").to_string(),
					CodeBlockKind::Indented => String::new(),
				};
				fence = Some((language, String::new()));
			}
			Event::Text(text) | Event::Code(text) if fence.is_some() => {
				if let Some(fence) = fence.as_mut() {
					fence.1.push_str(&text);
				}
			}
			Event::End(TagEnd::CodeBlock) => {
				if let Some((language, body)) = fence.take() {
					let trimmed = body.strip_suffix('\n').unwrap_or(&body);
					let label = if language.is_empty() { "text".to_string() } else { language.clone() };
					let html = highlight(trimmed, &language).unwrap_or_else(|| escape(trimmed));
					events.push(Event::Html(boxed(format!(
						"<div class=\"code-block\" data-language=\"{label}\"><div class=\"code-head\"><span class=\"code-lang\">{label}</span><button type=\"button\" class=\"code-copy\" data-code-copy=\"copy\">Copy</button></div><pre class=\"code\"><code class=\"language-{label}\">{html}</code></pre></div>",
						label = escape(&label)
					))));
				}
			}
			Event::Start(Tag::Heading { level, id, .. }) => {
				heading = Some(id.map(|id| id.into_string()).unwrap_or_default());
				events.push(Event::Html(boxed(format!("<h{}>", level_of(level)))));
			}
			Event::End(TagEnd::Heading(level)) => {
				let anchor = heading.take().map(|text| heading_id(&text, &mut counts)).unwrap_or_else(|| "section".to_string());
				events.push(Event::Html(boxed(format!(
					"</h{}><a class=\"heading-anchor\" href=\"#{anchor}\" aria-label=\"Link to this section\"></a>",
					level_of(level)
				))));
			}
			Event::Text(text) => {
				if let Some(heading) = heading.as_mut() {
					heading.push_str(&text);
				}
				events.push(Event::Text(text));
			}
			Event::Code(text) => {
				if let Some(heading) = heading.as_mut() {
					heading.push_str(&text);
				}
				events.push(Event::Code(text));
			}
			Event::Start(Tag::Link { dest_url, title, .. }) => {
				let target = safe_url(&dest_url).unwrap_or_else(|| "#".to_string());
				let external = target.starts_with("http://") || target.starts_with("https://") || target.starts_with("mailto:");
				events.push(Event::Html(boxed(format!(
					"<a href=\"{}\"{}{}>",
					escape(&target),
					if title.is_empty() { String::new() } else { format!(" title=\"{}\"", escape(&title)) },
					if external { " target=\"_blank\" rel=\"noopener noreferrer\"" } else { "" }
				))));
			}
			Event::End(TagEnd::Link) => events.push(Event::Html(CowStr::Borrowed("</a>"))),
			Event::Start(Tag::Image { dest_url, title, .. }) => {
				let target = safe_url(&dest_url).unwrap_or_default();
				events.push(Event::Html(boxed(format!(
					"<img src=\"{}\" alt=\"",
					escape(&target)
				))));
				if !title.is_empty() {
					events.push(Event::Text(CowStr::Boxed(title.clone().into_string().into_boxed_str())));
				}
			}
			Event::End(TagEnd::Image) => events.push(Event::Html(CowStr::Borrowed("\" loading=\"lazy\">"))),
			Event::Html(raw) | Event::InlineHtml(raw) => events.push(Event::Html(boxed(sanitize(&raw)))),
			Event::InlineMath(text) => events.push(Event::Html(boxed(format!(
				"<span class=\"math math-inline\">{}</span>",
				escape(&text)
			)))),
			Event::DisplayMath(text) => events.push(Event::Html(boxed(format!(
				"<span class=\"math math-display\">{}</span>",
				escape(&text)
			)))),
			other => events.push(other),
		}
	}
	let mut output = String::new();
	html::push_html(&mut output, events.into_iter());
	output
}
