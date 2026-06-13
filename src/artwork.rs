//! Album art helpers: build the iTunes query URL, parse the JSON response,
//! upscale the returned thumbnail URL, and perform the best-effort async fetch.
//! Pure helpers are unit-tested. `fetch_artwork` uses `ureq` (blocking) via
//! `tokio::task::spawn_blocking` and never propagates errors — any failure
//! returns `None` so the SPA falls back to the gradient placeholder.

use serde_json::Value;

// ── URL helpers ───────────────────────────────────────────────────────────────

/// Percent-encode a string for use as a URL query parameter value.
/// Encodes everything that is not an unreserved character (A–Z a–z 0–9 - _ . ~).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            other => {
                out.push('%');
                out.push(
                    char::from_digit((other >> 4) as u32, 16)
                        .unwrap()
                        .to_ascii_uppercase(),
                );
                out.push(
                    char::from_digit((other & 0xf) as u32, 16)
                        .unwrap()
                        .to_ascii_uppercase(),
                );
            }
        }
    }
    out
}

/// Build the iTunes Search API query URL for an artist + album (or title).
///
/// Term used: `"<artist> <album>"` when album is non-empty, else
/// `"<artist> <title>"`. Returns `None` if there is no usable text at all
/// (artist AND both album+title are empty or whitespace-only).
pub fn itunes_query_url(artist: &str, album: &str, title: &str) -> Option<String> {
    let artist = artist.trim();
    let album = album.trim();
    let title = title.trim();

    // Determine the search term.  Build it from non-empty parts joined by a
    // space so a missing artist never produces a leading/trailing space.
    let term = if !album.is_empty() {
        // Prefer album context; prepend artist only when present.
        if artist.is_empty() {
            album.to_string()
        } else {
            format!("{artist} {album}")
        }
    } else if !title.is_empty() {
        if artist.is_empty() {
            title.to_string()
        } else {
            format!("{artist} {title}")
        }
    } else if !artist.is_empty() {
        artist.to_string()
    } else {
        // Nothing usable.
        return None;
    };

    let encoded = percent_encode(&term);
    Some(format!(
        "https://itunes.apple.com/search?term={encoded}&entity=album&limit=1"
    ))
}

// ── JSON parsing ──────────────────────────────────────────────────────────────

/// Parse an iTunes Search API JSON response and return the artwork URL from
/// `results[0].artworkUrl100` (or `artworkUrl60` as a fallback).
/// Returns `None` on any error: malformed JSON, empty results, missing field.
pub fn parse_itunes_artwork(json: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json).ok()?;
    let result = v.get("results")?.as_array()?.first()?;
    // Prefer artworkUrl100, fall back to artworkUrl60.
    let url = result
        .get("artworkUrl100")
        .or_else(|| result.get("artworkUrl60"))?
        .as_str()?;
    if url.is_empty() {
        return None;
    }
    Some(url.to_string())
}

// ── URL upscaling ─────────────────────────────────────────────────────────────

/// Replace the thumbnail size in an iTunes artwork URL with `600x600`.
/// iTunes serves the same image at larger dimensions just by changing the size
/// token in the path. If no known size token is present the URL is returned
/// unchanged (best-effort).
pub fn upscale_artwork_url(url: &str) -> String {
    // Replace the most common thumbnail sizes. Order matters: try the larger
    // sizes first so we don't double-substitute.
    for token in &["100x100bb", "100x100", "60x60bb", "60x60"] {
        if url.contains(token) {
            return url.replacen(token, "600x600bb", 1);
        }
    }
    url.to_string()
}

// ── Async HTTP fetch ──────────────────────────────────────────────────────────

/// Fetch album art for the given track metadata using the iTunes Search API.
///
/// Best-effort: every failure path returns `None`.
/// - If `SOUNDSYNC_ARTWORK=off`, skips the lookup immediately.
/// - If there is no usable metadata, skips the lookup.
/// - Any network error, timeout, or non-match returns `None`.
/// - On success, returns the upscaled artwork URL (`600x600bb`).
///
/// The HTTP call is run in `tokio::task::spawn_blocking` (ureq is a blocking
/// client) so it does not stall the async runtime.
pub async fn fetch_artwork(artist: String, album: String, title: String) -> Option<String> {
    // Env opt-out for offline/hardened mode.
    if std::env::var("SOUNDSYNC_ARTWORK").as_deref() == Ok("off") {
        return None;
    }

    // Build the query URL — returns None if no usable metadata.
    let url = itunes_query_url(&artist, &album, &title)?;

    let result = tokio::task::spawn_blocking(move || -> Option<String> {
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(4))
            .build();
        let body = match agent.get(&url).call() {
            Ok(response) => match response.into_string() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("soundsync: artwork: body read error: {e}");
                    return None;
                }
            },
            Err(e) => {
                eprintln!("soundsync: artwork: HTTP error: {e}");
                return None;
            }
        };
        parse_itunes_artwork(&body).map(|u| upscale_artwork_url(&u))
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(e) => {
            eprintln!("soundsync: artwork: spawn_blocking error: {e}");
            None
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── itunes_query_url ──────────────────────────────────────────────────────

    #[test]
    fn query_url_artist_and_album() {
        let url = itunes_query_url("Pink Floyd", "The Wall", "Another Brick").unwrap();
        // term = "Pink Floyd The Wall"
        assert!(url.starts_with("https://itunes.apple.com/search?term="));
        assert!(url.contains("Pink+Floyd+The+Wall"));
        assert!(url.contains("entity=album"));
        assert!(url.contains("limit=1"));
    }

    #[test]
    fn query_url_falls_back_to_title_when_no_album() {
        let url = itunes_query_url("Radiohead", "", "Creep").unwrap();
        // term = "Radiohead Creep"
        assert!(url.contains("Radiohead+Creep"));
    }

    #[test]
    fn query_url_uses_only_title_when_artist_also_empty() {
        let url = itunes_query_url("", "", "Bohemian Rhapsody").unwrap();
        assert!(url.contains("Bohemian+Rhapsody"));
    }

    #[test]
    fn query_url_returns_none_when_all_empty() {
        assert_eq!(itunes_query_url("", "", ""), None);
        assert_eq!(itunes_query_url("   ", "  ", " "), None);
    }

    #[test]
    fn query_url_percent_encodes_special_chars() {
        let url = itunes_query_url("AC/DC", "Back in Black", "").unwrap();
        // '/' → %2F
        assert!(url.contains("AC%2FDC"));
    }

    #[test]
    fn query_url_encodes_ampersand_and_equals() {
        let url = itunes_query_url("Jay-Z & Kanye", "Watch the Throne", "").unwrap();
        assert!(url.contains("%26")); // '&' → %26
    }

    #[test]
    fn query_url_prefers_album_over_title() {
        let url = itunes_query_url("Artist", "Album Name", "Track Title").unwrap();
        assert!(url.contains("Album+Name"));
        assert!(!url.contains("Track+Title"));
    }

    #[test]
    fn query_url_no_leading_space_when_artist_empty_album_present() {
        // Regression: format!("{} {}", "", "Abbey Road") used to produce
        // " Abbey Road" with a leading space → "+Abbey+Road" in the URL.
        let url = itunes_query_url("", "Abbey Road", "Come Together").unwrap();
        // term should be exactly "Abbey Road" — no leading '+' or space.
        assert!(url.contains("Abbey+Road"));
        assert!(!url.contains("+Abbey+Road")); // no leading space encoded as '+'
        assert!(!url.contains("%20Abbey")); // no leading space encoded as '%20'
                                            // Should not contain the track title (album preferred when present).
        assert!(!url.contains("Come+Together"));
    }

    #[test]
    fn query_url_artist_only_no_album_no_title() {
        // Artist-only fallback: only artist, both album and title empty.
        let url = itunes_query_url("Massive Attack", "", "").unwrap();
        assert!(url.contains("Massive+Attack"));
        // Should not have a trailing '+' or extra separator.
        let decoded_term = url
            .split("term=")
            .nth(1)
            .and_then(|s| s.split('&').next())
            .unwrap_or("");
        assert_eq!(decoded_term, "Massive+Attack");
    }

    // ── parse_itunes_artwork ──────────────────────────────────────────────────

    #[test]
    fn parse_returns_artwork_url() {
        let json = r#"{
            "resultCount": 1,
            "results": [{
                "collectionName": "The Wall",
                "artworkUrl100": "https://is1-ssl.mzstatic.com/image/thumb/Music/v4/abc/100x100bb.jpg",
                "artworkUrl60": "https://is1-ssl.mzstatic.com/image/thumb/Music/v4/abc/60x60bb.jpg"
            }]
        }"#;
        let url = parse_itunes_artwork(json).unwrap();
        assert_eq!(
            url,
            "https://is1-ssl.mzstatic.com/image/thumb/Music/v4/abc/100x100bb.jpg"
        );
    }

    #[test]
    fn parse_falls_back_to_artwork_url_60() {
        let json = r#"{
            "resultCount": 1,
            "results": [{
                "collectionName": "Some Album",
                "artworkUrl60": "https://example.com/60x60bb.jpg"
            }]
        }"#;
        let url = parse_itunes_artwork(json).unwrap();
        assert_eq!(url, "https://example.com/60x60bb.jpg");
    }

    #[test]
    fn parse_returns_none_for_empty_results() {
        let json = r#"{"resultCount": 0, "results": []}"#;
        assert_eq!(parse_itunes_artwork(json), None);
    }

    #[test]
    fn parse_returns_none_for_malformed_json() {
        assert_eq!(parse_itunes_artwork("not json at all"), None);
        assert_eq!(parse_itunes_artwork("{"), None);
        assert_eq!(parse_itunes_artwork("null"), None);
    }

    #[test]
    fn parse_returns_none_when_artwork_field_missing() {
        let json = r#"{"resultCount": 1, "results": [{"collectionName": "X"}]}"#;
        assert_eq!(parse_itunes_artwork(json), None);
    }

    // ── upscale_artwork_url ───────────────────────────────────────────────────

    #[test]
    fn upscale_replaces_100x100bb() {
        let input = "https://is1-ssl.mzstatic.com/image/thumb/Music/v4/abc/100x100bb.jpg";
        let output = upscale_artwork_url(input);
        assert_eq!(
            output,
            "https://is1-ssl.mzstatic.com/image/thumb/Music/v4/abc/600x600bb.jpg"
        );
    }

    #[test]
    fn upscale_replaces_100x100_without_bb() {
        let input = "https://example.com/art/100x100.jpg";
        let output = upscale_artwork_url(input);
        assert_eq!(output, "https://example.com/art/600x600bb.jpg");
    }

    #[test]
    fn upscale_replaces_60x60bb() {
        let input = "https://is1-ssl.mzstatic.com/image/thumb/Music/abc/60x60bb.jpg";
        let output = upscale_artwork_url(input);
        assert_eq!(
            output,
            "https://is1-ssl.mzstatic.com/image/thumb/Music/abc/600x600bb.jpg"
        );
    }

    #[test]
    fn upscale_leaves_url_unchanged_when_no_match() {
        let input = "https://example.com/art/original.jpg";
        let output = upscale_artwork_url(input);
        assert_eq!(output, input);
    }

    #[test]
    fn upscale_does_not_double_substitute() {
        // 100x100bb is matched first; the result should have exactly one 600x600bb
        let input = "https://example.com/100x100bb/100x100bb.jpg";
        let output = upscale_artwork_url(input);
        // replacen(..., 1) touches only the first occurrence
        assert!(output.starts_with("https://example.com/600x600bb/"));
        assert!(output.ends_with("/100x100bb.jpg"));
    }

    // ── percent_encode ────────────────────────────────────────────────────────

    #[test]
    fn percent_encode_unreserved_chars_pass_through() {
        assert_eq!(percent_encode("abc-_.~ABC"), "abc-_.~ABC");
    }

    #[test]
    fn percent_encode_space_becomes_plus() {
        assert_eq!(percent_encode("hello world"), "hello+world");
    }

    #[test]
    fn percent_encode_slash_becomes_percent_2f() {
        assert_eq!(percent_encode("a/b"), "a%2Fb");
    }
}
