// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Google's Trending Now RSS feed, read into values. Always compiled.
//!
//! The feed is small (about twenty kilobytes for ten items) and its shape
//! is fixed, so this is a hand parser rather than an XML crate: a tag
//! tokenizer that checks every element closes where it opened, decodes the
//! five named entities and numeric references, and unwraps CDATA. A feed
//! that is not well-formed is an error with a reason, never a short list -
//! a parser that stopped at the first bad tag and returned what it had so
//! far would draw a feed with three items in it as a feed with three items.

use opscope_core as tc;

/// One news story Google attaches to a trend.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct NewsItem {
    pub title: String,
    pub url: String,
    pub source: String,
}

/// One `<item>` of the feed.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Trend {
    /// The search term, as Google writes it.
    pub title: String,
    /// `<ht:approx_traffic>`, verbatim. A bucket like `200+`, not a count.
    pub traffic: String,
    /// `<pubDate>` as Unix seconds, when Google says the trend started.
    pub started: Option<i64>,
    /// `<pubDate>` as written, kept for when it would not parse.
    pub started_raw: String,
    pub news: Vec<NewsItem>,
}

/// What one fetch of the feed came to.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// The feed parsed. May be empty, which is its own reading.
    Items(Vec<Trend>),
    /// HTTP 400: Google does not know this region code.
    Rejected(String),
    /// Any other non-2xx status; the status and a squeezed body.
    Failed(String),
    /// A 2xx whose body would not parse, with the parser's reason.
    Unreadable(String),
}

/// One element of the document, with its text and children.
#[derive(Debug, Default)]
struct Node {
    name: String,
    text: String,
    children: Vec<Node>,
}

impl Node {
    fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    fn text_of(&self, name: &str) -> String {
        self.child(name).map(|c| clean(&c.text)).unwrap_or_default()
    }
}

/// Text from the feed, as one line with no terminal escapes in it.
fn clean(s: &str) -> String {
    tc::source_text(&s.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The line a byte offset is on, for an error a person can find.
fn line_of(text: &str, at: usize) -> usize {
    text[..at.min(text.len())].matches('\n').count() + 1
}

/// Decode `&amp;` and friends. An unknown entity is kept as written: the
/// feed has never used one, and dropping it would lose text silently.
fn decode_entities(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        // Bytes, not a `&str` slice: byte 12 can fall inside a multibyte
        // character after the entity, and `;` is ASCII, so its index is
        // always a character boundary.
        let Some(end) = after.as_bytes()[..after.len().min(12)]
            .iter()
            .position(|&b| b == b';')
        else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let name = &after[1..end];
        let decoded = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if name.starts_with("#x") || name.starts_with("#X") => {
                u32::from_str_radix(&name[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if name.starts_with('#') => name[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The document as a tree, or why it is not well-formed.
fn parse_document(text: &str) -> Result<Node, String> {
    let mut stack: Vec<Node> = vec![Node::default()];
    let mut i = 0usize;
    let bytes = text.as_bytes();
    while i < text.len() {
        if bytes[i] != b'<' {
            let end = text[i..].find('<').map_or(text.len(), |n| i + n);
            let chunk = &text[i..end];
            if let Some(top) = stack.last_mut() {
                top.text.push_str(&decode_entities(chunk));
            }
            i = end;
            continue;
        }
        let rest = &text[i..];
        if rest.starts_with("<?") {
            let end = rest.find("?>").ok_or_else(|| {
                format!("line {}: a <? declaration never closes", line_of(text, i))
            })?;
            i += end + 2;
        } else if rest.starts_with("<!--") {
            let end = rest
                .find("-->")
                .ok_or_else(|| format!("line {}: a comment never closes", line_of(text, i)))?;
            i += end + 3;
        } else if rest.starts_with("<![CDATA[") {
            let end = rest.find("]]>").ok_or_else(|| {
                format!("line {}: a CDATA section never closes", line_of(text, i))
            })?;
            if let Some(top) = stack.last_mut() {
                top.text.push_str(&rest[9..end]);
            }
            i += end + 3;
        } else if rest.starts_with("<!") {
            let end = rest.find('>').ok_or_else(|| {
                format!("line {}: a <! declaration never closes", line_of(text, i))
            })?;
            i += end + 1;
        } else if let Some(close) = rest.strip_prefix("</") {
            let end = close
                .find('>')
                .ok_or_else(|| format!("line {}: a closing tag never ends", line_of(text, i)))?;
            let name = close[..end].trim();
            if stack.len() < 2 {
                return Err(format!(
                    "line {}: </{}> closes nothing that was open",
                    line_of(text, i),
                    name
                ));
            }
            let node = stack.pop().expect("checked above");
            if node.name != name {
                return Err(format!(
                    "line {}: expected </{}>, found </{}>",
                    line_of(text, i),
                    node.name,
                    name
                ));
            }
            stack
                .last_mut()
                .expect("the root is never popped")
                .children
                .push(node);
            i += 2 + end + 1;
        } else {
            // A start tag. Attribute values are quoted and may hold `>`.
            let mut j = 1usize;
            let mut quote: Option<u8> = None;
            let tag_bytes = rest.as_bytes();
            while j < tag_bytes.len() {
                let b = tag_bytes[j];
                match quote {
                    Some(q) if b == q => quote = None,
                    Some(_) => {}
                    None if b == b'"' || b == b'\'' => quote = Some(b),
                    None if b == b'>' => break,
                    None if b == b'<' => {
                        return Err(format!(
                            "line {}: a tag opens inside another tag",
                            line_of(text, i + j)
                        ));
                    }
                    None => {}
                }
                j += 1;
            }
            if j >= tag_bytes.len() {
                return Err(format!("line {}: a tag never ends", line_of(text, i)));
            }
            let inner = &rest[1..j];
            let self_closing = inner.ends_with('/');
            let name: String = inner
                .trim_end_matches('/')
                .split(|c: char| c.is_whitespace())
                .next()
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                return Err(format!("line {}: a tag with no name", line_of(text, i)));
            }
            let node = Node {
                name,
                ..Node::default()
            };
            if self_closing {
                stack
                    .last_mut()
                    .expect("the root is never popped")
                    .children
                    .push(node);
            } else {
                stack.push(node);
            }
            i += j + 1;
        }
    }
    if stack.len() > 1 {
        let open = stack.last().map(|n| n.name.clone()).unwrap_or_default();
        return Err(format!("the document ends with <{}> still open", open));
    }
    let doc = stack.pop().unwrap_or_default();
    // One root and nothing beside it. Two documents run together would
    // otherwise read as the first alone, and the trends in the second
    // would be recorded as having left the feed.
    if doc.children.len() > 1 {
        return Err(format!(
            "{} root elements where XML allows one (<{}> then <{}>)",
            doc.children.len(),
            doc.children[0].name,
            doc.children[1].name
        ));
    }
    if !doc.text.trim().is_empty() {
        return Err("text outside the root element".into());
    }
    Ok(doc)
}

/// `<pubDate>` as Unix seconds. RFC 2822, as RSS specifies.
pub fn parse_pub_date(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc2822(text.trim())
        .ok()
        .map(|t| t.timestamp())
}

/// The trends in a Trending Now feed, in the order Google lists them.
///
/// Never re-ranked: the order is Google's and is part of what it says.
/// A missing optional field is an empty string or `None`, not an error;
/// an item with no title is skipped, because there is nothing to show
/// for it and nothing to count it as.
pub fn parse_trending_rss(text: &str) -> Result<Vec<Trend>, String> {
    let body = text.trim_start_matches('\u{feff}');
    if body.trim().is_empty() {
        return Err("the response was empty".into());
    }
    let doc = parse_document(body)?;
    let rss = doc.child("rss").ok_or_else(|| match doc.children.first() {
        Some(other) => format!("expected an <rss> document, found <{}>", other.name),
        None => "no XML element in the response".to_string(),
    })?;
    let channel = rss
        .child("channel")
        .ok_or_else(|| "the <rss> document has no <channel>".to_string())?;
    let mut out = Vec::new();
    for item in channel.all("item") {
        let title = item.text_of("title");
        if title.is_empty() {
            continue;
        }
        let started_raw = item.text_of("pubDate");
        let news = item
            .all("ht:news_item")
            .map(|n| NewsItem {
                title: n.text_of("ht:news_item_title"),
                url: n.text_of("ht:news_item_url"),
                source: n.text_of("ht:news_item_source"),
            })
            .filter(|n| !(n.title.is_empty() && n.url.is_empty() && n.source.is_empty()))
            .collect();
        out.push(Trend {
            title,
            traffic: item.text_of("ht:approx_traffic"),
            started: parse_pub_date(&started_raw),
            started_raw,
            news,
        });
    }
    Ok(out)
}

/// One HTTP answer from the feed URL, read into what the pane says.
///
/// `body` is what came back, `status` its HTTP status, `geo` the region
/// that was asked for. 400 is Google's answer to a code it does not know;
/// every other refusal is a failed fetch that keeps the last rows.
pub fn parse_answer(body: &str, status: u16, geo: &str) -> Answer {
    match status {
        200..=299 => match parse_trending_rss(body) {
            Ok(items) => Answer::Items(items),
            Err(why) => Answer::Unreadable(why),
        },
        400 => Answer::Rejected(format!("Google rejects geo \"{}\"", geo)),
        0 => Answer::Failed("no HTTP status in the response".into()),
        _ => Answer::Failed(format!("HTTP {}", status)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like the real feed: the same namespaces, the same element
    /// names, the self-closing `<description/>`, entities in titles, and
    /// one CDATA title. Every term and source is made up.
    const FEED: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<rss xmlns:atom="http://www.w3.org/2005/Atom" xmlns:ht="https://trends.google.com/trending/rss" version="2.0">
	<channel>
		<title>Daily Search Trends</title>
		<description>Recent searches</description>
		<link>https://trends.google.com/trending/rss?geo=US</link>
		<atom:link href="https://trends.google.com/trending/rss?geo=US" rel="self" type="application/rss+xml"/>
		<item>
			<title>zorblax festival</title>
			<ht:approx_traffic>5000+</ht:approx_traffic>
			<description/>
			<link>https://trends.google.com/trending/rss?geo=US</link>
			<pubDate>Wed, 21 Oct 2026 21:00:00 -0700</pubDate>
			<ht:picture>https://example.com/p.jpg</ht:picture>
			<ht:picture_source>Example News</ht:picture_source>
			<ht:news_item>
				<ht:news_item_title>Zorblax &amp; friends draw a crowd &#8212; again</ht:news_item_title>
				<ht:news_item_snippet/>
				<ht:news_item_url>https://example.com/a?x=1&amp;y=2</ht:news_item_url>
				<ht:news_item_picture>https://example.com/a.jpg</ht:news_item_picture>
				<ht:news_item_source>Example News</ht:news_item_source>
			</ht:news_item>
			<ht:news_item>
				<ht:news_item_title><![CDATA[Quux <b>live</b> & loud]]></ht:news_item_title>
				<ht:news_item_url>https://example.org/b</ht:news_item_url>
				<ht:news_item_source>Sample Gazette</ht:news_item_source>
			</ht:news_item>
		</item>
		<item>
			<title><![CDATA[flumbo "q" & co]]></title>
			<ht:approx_traffic>200+</ht:approx_traffic>
			<pubDate>Thu, 22 Oct 2026 03:30:00 +0000</pubDate>
		</item>
	</channel>
</rss>
"#;

    #[test]
    fn a_normal_feed_reads_in_googles_order() {
        let items = parse_trending_rss(FEED).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "zorblax festival");
        assert_eq!(items[0].traffic, "5000+");
        assert_eq!(items[0].started, Some(1_792_641_600));
        assert_eq!(items[0].news.len(), 2);
        assert_eq!(
            items[0].news[0].title,
            "Zorblax & friends draw a crowd \u{2014} again"
        );
        assert_eq!(items[0].news[0].url, "https://example.com/a?x=1&y=2");
        assert_eq!(items[0].news[0].source, "Example News");
        // CDATA is raw text: markup inside it is not markup, and `&` is `&`.
        assert_eq!(items[0].news[1].title, "Quux <b>live</b> & loud");
        assert_eq!(items[1].title, "flumbo \"q\" & co");
    }

    #[test]
    fn the_bucket_is_kept_verbatim() {
        let feed = FEED.replace("5000+", "2M+");
        assert_eq!(parse_trending_rss(&feed).unwrap()[0].traffic, "2M+");
    }

    #[test]
    fn missing_optional_fields_are_empty_not_errors() {
        let items = parse_trending_rss(FEED).unwrap();
        assert!(items[1].news.is_empty());
        let bare = r#"<rss><channel><item><title>lonely term</title></item></channel></rss>"#;
        let items = parse_trending_rss(bare).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].traffic, "");
        assert_eq!(items[0].started, None);
        assert_eq!(items[0].started_raw, "");
    }

    #[test]
    fn an_unparseable_date_is_none_and_kept_as_written() {
        let feed = FEED.replace("Wed, 21 Oct 2026 21:00:00 -0700", "sometime yesterday");
        let items = parse_trending_rss(&feed).unwrap();
        assert_eq!(items[0].started, None);
        assert_eq!(items[0].started_raw, "sometime yesterday");
    }

    #[test]
    fn an_item_with_no_title_is_skipped() {
        let feed = r#"<rss><channel><item><title></title></item><item><title>kept</title></item></channel></rss>"#;
        let items = parse_trending_rss(feed).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "kept");
    }

    #[test]
    fn an_empty_feed_is_an_empty_list_not_an_error() {
        let feed = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Daily Search Trends</title></channel></rss>"#;
        assert_eq!(parse_trending_rss(feed).unwrap(), Vec::new());
    }

    #[test]
    fn malformed_xml_says_why() {
        let cut = &FEED[..FEED.find("</channel>").unwrap()];
        let err = parse_trending_rss(cut).unwrap_err();
        assert!(err.contains("still open"), "{err}");

        let crossed = FEED.replace("</ht:approx_traffic>", "</title>");
        let err = parse_trending_rss(&crossed).unwrap_err();
        assert!(err.contains("expected </ht:approx_traffic>"), "{err}");
        assert!(err.starts_with("line "), "{err}");

        let err = parse_trending_rss("").unwrap_err();
        assert!(err.contains("empty"), "{err}");

        let err = parse_trending_rss("<html><body>sorry</body></html>").unwrap_err();
        assert!(err.contains("found <html>"), "{err}");

        let err = parse_trending_rss("<rss><item>").unwrap_err();
        assert!(err.contains("still open"), "{err}");

        let err = parse_trending_rss("<rss></channel>").unwrap_err();
        assert!(err.contains("expected </rss>"), "{err}");

        let err = parse_trending_rss("<rss><channel></channel></rss></rss>").unwrap_err();
        assert!(err.contains("closes nothing"), "{err}");
    }

    #[test]
    fn terminal_escapes_in_the_feed_do_not_reach_the_screen() {
        let feed = FEED.replace("zorblax festival", "zorblax \u{1b}[31mfestival");
        let items = parse_trending_rss(&feed).unwrap();
        assert_eq!(items[0].title, "zorblax festival");
    }

    #[test]
    fn entities_decode_and_unknown_ones_are_kept() {
        assert_eq!(
            decode_entities("a &lt;b&gt; &quot;c&quot; &apos;d&apos;"),
            "a <b> \"c\" 'd'"
        );
        assert_eq!(decode_entities("&#65;&#x42;"), "AB");
        assert_eq!(
            decode_entities("fish &chips; & more"),
            "fish &chips; & more"
        );
        // Byte 12 lands inside a multibyte character; this panicked.
        assert_eq!(decode_entities("&amp;日本語日本語"), "&日本語日本語");
        assert_eq!(decode_entities("&amp;éééé"), "&éééé");
        assert_eq!(decode_entities("&éééééééé;"), "&éééééééé;");
    }

    #[test]
    fn a_document_with_two_roots_or_stray_text_is_refused() {
        let one = "<rss><channel></channel></rss>";
        assert!(parse_document(one).is_ok());
        let two = format!("{one}{one}");
        let err = parse_document(&two).unwrap_err();
        assert!(err.contains("2 root elements"), "{err}");
        let stray = format!("{one}trailing");
        assert_eq!(
            parse_document(&stray).unwrap_err(),
            "text outside the root element"
        );
        assert!(parse_trending_rss(&two).is_err());
    }

    #[test]
    fn a_400_is_a_rejected_geo() {
        let page = "<html><title>Error 400 (Bad Request)!!1</title></html>";
        assert_eq!(
            parse_answer(page, 400, "ZZ"),
            Answer::Rejected("Google rejects geo \"ZZ\"".into())
        );
    }

    #[test]
    fn other_refusals_are_failures_with_their_status() {
        assert_eq!(
            parse_answer("", 429, "US"),
            Answer::Failed("HTTP 429".into())
        );
        assert_eq!(
            parse_answer("oops", 503, "US"),
            Answer::Failed("HTTP 503".into())
        );
        assert!(matches!(parse_answer("", 0, "US"), Answer::Failed(_)));
    }

    #[test]
    fn a_200_that_will_not_parse_is_unreadable_with_the_reason() {
        match parse_answer("<rss><channel>", 200, "US") {
            Answer::Unreadable(why) => assert!(why.contains("still open"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            parse_answer(FEED, 200, "US"),
            Answer::Items(parse_trending_rss(FEED).unwrap())
        );
    }
}
