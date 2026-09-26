//! Minimal OpenGraph / Twitter card extraction from HTML.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageMeta {
    pub title: Option<String>,
    pub description: Option<String>,
    pub site: Option<String>,
    /// Absolute image URL, if any.
    pub image: Option<String>,
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let start = from + i;
        let before_ok = start == 0 || lower.as_bytes()[start - 1].is_ascii_whitespace();
        let rest = lower[start + name.len()..].trim_start();
        if before_ok && rest.starts_with('=') {
            let vstart = tag.len() - rest.len() + 1;
            let v = tag[vstart..].trim_start();
            let off = tag.len() - v.len();
            return match v.chars().next() {
                Some(q @ ('"' | '\'')) => tag[off + 1..].find(q).map(|e| &tag[off + 1..off + 1 + e]),
                Some(_) => Some(v.split(|c: char| c.is_whitespace() || c == '>').next().unwrap_or("")),
                None => None,
            };
        }
        from = start + name.len();
    }
    None
}

fn unescape(s: &str) -> String {
    let s = s
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn absolutize(base: &str, url: &str) -> Option<String> {
    let url = url.trim();
    if url.starts_with("https://") || url.starts_with("http://") {
        return Some(url.to_owned());
    }
    let scheme_end = base.find("://")? + 3;
    if let Some(rest) = url.strip_prefix("//") {
        return Some(format!("{}{rest}", &base[..scheme_end]));
    }
    let host_end = base[scheme_end..].find('/').map_or(base.len(), |i| scheme_end + i);
    if url.starts_with('/') {
        return Some(format!("{}{url}", &base[..host_end]));
    }
    let dir_end = base.rfind('/').filter(|&i| i >= host_end).unwrap_or(host_end);
    Some(format!("{}/{url}", &base[..dir_end]))
}

fn truncate(s: String, max: usize) -> String {
    if s.chars().count() <= max { s } else { s.chars().take(max - 1).chain(std::iter::once('…')).collect() }
}

/// Parses the document head. Returns `None` when there's nothing worth showing.
pub fn parse(html: &str, base: &str) -> Option<PageMeta> {
    let head_end = html.to_ascii_lowercase().find("</head>").unwrap_or(html.len());
    let head = &html[..head_end];
    let mut m = PageMeta::default();
    let mut title_tag = None;
    let mut rest = head;
    while let Some(i) = rest.find('<') {
        let after = &rest[i + 1..];
        let end = after.find('>').unwrap_or(after.len());
        let tag = &after[..end];
        let lower_start: String = tag.chars().take(6).collect::<String>().to_ascii_lowercase();
        if lower_start.starts_with("meta") {
            let key = attr(tag, "property").or_else(|| attr(tag, "name")).map(|k| k.to_ascii_lowercase());
            let content = attr(tag, "content").map(unescape);
            if let (Some(k), Some(c)) = (key, content.filter(|c| !c.is_empty())) {
                match k.as_str() {
                    "og:title" | "twitter:title" => m.title = m.title.or(Some(c)),
                    "og:description" | "twitter:description" | "description" => {
                        m.description = m.description.or(Some(c))
                    }
                    "og:site_name" => m.site = m.site.or(Some(c)),
                    "og:image" | "og:image:url" | "og:image:secure_url" | "twitter:image" | "twitter:image:src" => {
                        m.image = m.image.or_else(|| absolutize(base, &c))
                    }
                    _ => {}
                }
            }
        } else if lower_start.starts_with("title") && title_tag.is_none() {
            let content_start = i + 1 + end + 1;
            if let Some(close) = rest.get(content_start..).and_then(|r| r.to_ascii_lowercase().find("</title")) {
                title_tag = Some(unescape(&rest[content_start..content_start + close]));
            }
        }
        rest = &after[end.min(after.len())..];
    }
    m.title = m.title.or(title_tag).filter(|t| !t.is_empty()).map(|t| truncate(t, 140));
    m.description = m.description.map(|d| truncate(d, 300));
    if m.title.is_none() && m.image.is_none() {
        return None;
    }
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opengraph() {
        let html = r#"<!doctype html><html><head>
            <title>Fallback title</title>
            <meta property="og:title" content="Rust &amp; IRC">
            <meta content='A   description' name="description"/>
            <meta property="og:image" content="/img/card.png">
            <meta property="og:site_name" content="Example">
            </head><body><meta property="og:title" content="ignored"></body></html>"#;
        let m = parse(html, "https://example.com/blog/post").unwrap();
        assert_eq!(m.title.as_deref(), Some("Rust & IRC"));
        assert_eq!(m.description.as_deref(), Some("A description"));
        assert_eq!(m.image.as_deref(), Some("https://example.com/img/card.png"));
        assert_eq!(m.site.as_deref(), Some("Example"));
    }

    #[test]
    fn title_only_and_relative_urls() {
        let m = parse("<head><TITLE>Just a title</TITLE></head>", "https://a.b/c/d").unwrap();
        assert_eq!(m.title.as_deref(), Some("Just a title"));
        assert!(parse("<head></head>", "https://a.b/").is_none());
        assert_eq!(absolutize("https://a.b/c/d", "e.png").as_deref(), Some("https://a.b/c/e.png"));
        assert_eq!(absolutize("https://a.b", "//cdn.x/y.png").as_deref(), Some("https://cdn.x/y.png"));
    }
}
