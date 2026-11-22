//! Wikimedia Commons licence filter (pure).
//!
//! An image is used only when Commons itself hosts it (`imagerepository` is `local` on the
//! Commons API; files that exist only on a Wikipedia, which is where non-free and fair-use files
//! live, come back `missing`) and its licence is one of: public domain, CC0, CC BY or CC BY-SA
//! (any version or port). Everything else is refused: NonCommercial and NoDerivatives variants,
//! GFDL-only, GPL, "fair use", anything marked `NonFree`, and anything unclear. The display
//! file must be served from `upload.wikimedia.org/wikipedia/commons/` (never a Wikipedia's own
//! upload folder) and must not be an SVG (SVGs are used through Commons' PNG rendering).

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;

/// The only host images are loaded from.
pub const IMAGE_HOST: &str = "upload.wikimedia.org";
/// The Commons upload folder on [`IMAGE_HOST`]; other folders (such as `/wikipedia/en/`, where
/// English Wikipedia keeps its non-free files) are never used.
pub const COMMONS_PATH: &str = "/wikipedia/commons/";
const MAX_URL_LEN: usize = 1024;
const MAX_TEXT_CHARS: usize = 200;

/// Raster formats the cover cache and the web view accept for a display file.
const DISPLAY_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];
/// Source formats we take from Commons (SVG only through its PNG rendering).
const SOURCE_MIMES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/svg+xml"];

/// One free image from Commons with what its credit needs.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CommonsImage {
    /// File name without the `File:` prefix, as Commons names it.
    pub file: String,
    /// The display file (a rendering at most ~500 px wide), see [`display_url`].
    pub src: String,
    /// Size of `src` in pixels.
    pub width: u32,
    pub height: u32,
    /// Size of the original file (equal to the above when no rendering was needed).
    pub original_width: u32,
    pub original_height: u32,
    /// Short licence name as Commons shows it ("CC BY-SA 4.0", "Public domain").
    pub licence: String,
    /// The licence deed (https on creativecommons.org only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub licence_url: Option<String>,
    /// Author, plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Commons "Credit" (source), plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit: Option<String>,
    /// The file's description page on Commons.
    pub source_page: String,
    #[serde(default)]
    pub attribution_required: bool,
    /// Commons restrictions such as "trademarked", shown as a note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restrictions: Option<String>,
}

/// Why a file was not used (for logs and tests).
pub type Reject = &'static str;

static LICENCE_CODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:pd|cc0|cc-zero|cc-by-[0-9](?:\.[0-9])?(?:-[a-z0-9]+)*|cc-by-sa-[0-9](?:\.[0-9])?(?:-[a-z0-9]+)*)$")
        .unwrap()
});
static LICENCE_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:public domain|cc0(?: 1\.0)?|cc by(?:-sa)?(?: [0-9](?:\.[0-9])?)?(?: [a-z]{2,})?)$",
    )
    .unwrap()
});
static NOT_FREE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\bnc\b|-nc\b|\bnd\b|-nd\b|fair[ _-]?use|non-?free|noncommercial|no ?deriv)")
        .unwrap()
});
static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").unwrap());

/// Decides on a licence from Commons' `License` code, `LicenseShortName` and `NonFree` values.
/// Returns the name to show.
pub fn licence_verdict(code: &str, short_name: &str, non_free: &str) -> Result<String, Reject> {
    let non_free = non_free.trim().to_ascii_lowercase();
    if !non_free.is_empty() && non_free != "false" && non_free != "0" {
        return Err("marked non-free");
    }
    let code = code.trim().to_ascii_lowercase();
    let name = short_name.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = name.to_lowercase();
    if NOT_FREE.is_match(&code) || NOT_FREE.is_match(&lower) {
        return Err("NonCommercial, NoDerivatives or non-free licence");
    }
    let accepted = if code.is_empty() {
        LICENCE_NAME.is_match(&lower)
    } else {
        LICENCE_CODE.is_match(&code)
    };
    if !accepted {
        return Err("licence not on the allow list");
    }
    if !name.is_empty() {
        return Ok(plain_text(&name).unwrap_or(name));
    }
    Ok(match code.as_str() {
        "pd" => "Public domain".into(),
        "cc0" | "cc-zero" => "CC0".into(),
        other => other.to_ascii_uppercase().replace('-', " "),
    })
}

/// HTML from Commons metadata as one line of plain text: tags removed, common entities decoded,
/// control and bidi characters dropped, at most 200 characters.
pub fn plain_text(html: &str) -> Option<String> {
    let text = TAGS.replace_all(html, " ");
    let text = text
        .replace("&nbsp;", " ")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#039;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    let text: String = crate::igdb::clean_name(&text)?
        .chars()
        .take(MAX_TEXT_CHARS)
        .collect();
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// `https://upload.wikimedia.org/wikipedia/commons/...` without credentials, port, query or
/// fragment (Commons adds `utm_` tracking parameters, which are dropped), with a raster
/// extension. `None` for anything else.
pub fn display_url(raw: &str) -> Option<String> {
    if raw.len() > MAX_URL_LEN {
        return None;
    }
    let mut url = url::Url::parse(raw.trim()).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    valid_image_url(url.as_str()).then(|| url.to_string())
}

/// Whether `raw` is exactly a Commons display file we may load and store: https on
/// [`IMAGE_HOST`], default port, no credentials, no query or fragment, under
/// [`COMMONS_PATH`], ending in a raster extension.
pub fn valid_image_url(raw: &str) -> bool {
    if raw.len() > MAX_URL_LEN {
        return false;
    }
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    let path = url.path();
    let ext = path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    url.scheme() == "https"
        && url.host_str() == Some(IMAGE_HOST)
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && path.starts_with(COMMONS_PATH)
        && !path.contains("..")
        && !path.contains('\\')
        && DISPLAY_EXTENSIONS.contains(&ext.as_str())
}

/// `https://commons.wikimedia.org/wiki/File:...` only.
fn description_page(raw: &str, file: &str) -> String {
    let ok = url::Url::parse(raw).ok().filter(|u| {
        u.scheme() == "https"
            && u.host_str() == Some("commons.wikimedia.org")
            && u.port().is_none()
            && u.username().is_empty()
            && u.query().is_none()
            && u.path().starts_with("/wiki/File:")
    });
    match ok {
        Some(url) => url.to_string(),
        None => {
            let mut url = url::Url::parse("https://commons.wikimedia.org/wiki/").unwrap();
            url.path_segments_mut()
                .expect("has a path")
                .pop()
                .push(&format!("File:{}", file.replace(' ', "_")));
            url.to_string()
        }
    }
}

/// The licence deed link: https on creativecommons.org, no credentials or port.
fn licence_link(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let raw = if let Some(rest) = raw.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        raw.replacen("http://", "https://", 1)
    };
    let url = url::Url::parse(&raw).ok()?;
    (url.scheme() == "https"
        && url.host_str() == Some("creativecommons.org")
        && url.port().is_none()
        && url.username().is_empty()
        && raw.len() <= 200)
        .then(|| url.to_string())
}

fn meta<'a>(ext: &'a Value, key: &str) -> &'a str {
    ext.get(key)
        .and_then(|v| v.get("value"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn dimension(value: Option<&Value>) -> u32 {
    value
        .and_then(Value::as_u64)
        .filter(|v| *v <= 100_000)
        .unwrap_or(0) as u32
}

/// Normalizes a Commons file name the way MediaWiki does for comparing: no `File:` prefix,
/// underscores as spaces, single spaces, first letter upper-case.
pub fn file_key(name: &str) -> String {
    let name = name.trim();
    let name = name
        .strip_prefix("File:")
        .or_else(|| name.strip_prefix("file:"))
        .unwrap_or(name);
    let name = name.replace('_', " ");
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// One page of a Commons `prop=imageinfo` response (formatversion 2) → the image, or why it was
/// refused.
pub fn check_page(page: &Value) -> Result<CommonsImage, Reject> {
    if page
        .get("missing")
        .is_some_and(|m| m.as_bool() != Some(false))
    {
        return Err("not on Commons");
    }
    if page.get("imagerepository").and_then(Value::as_str) != Some("local") {
        return Err("not hosted by Commons");
    }
    let title = page
        .get("title")
        .and_then(Value::as_str)
        .ok_or("no title")?;
    let file = file_key(title);
    if file.is_empty() || file.len() > 255 {
        return Err("bad file name");
    }
    let info = page
        .get("imageinfo")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .ok_or("no image info")?;
    let mime = info.get("mime").and_then(Value::as_str).unwrap_or_default();
    if !SOURCE_MIMES.contains(&mime) {
        return Err("unsupported image type");
    }
    let ext = info.get("extmetadata").ok_or("no licence metadata")?;
    let licence = licence_verdict(
        meta(ext, "License"),
        meta(ext, "LicenseShortName"),
        meta(ext, "NonFree"),
    )?;
    let original_width = dimension(info.get("width"));
    let original_height = dimension(info.get("height"));
    // The rendering Commons made for the requested width; SVGs have no usable original.
    let thumb = info.get("thumburl").and_then(Value::as_str);
    let (raw, width, height) = match thumb {
        Some(t) => (
            t,
            dimension(info.get("thumbwidth")),
            dimension(info.get("thumbheight")),
        ),
        None if mime != "image/svg+xml" => (
            info.get("url").and_then(Value::as_str).unwrap_or_default(),
            original_width,
            original_height,
        ),
        None => return Err("no raster rendering"),
    };
    let src = display_url(raw).ok_or("image URL not on the Commons upload host")?;
    if width == 0 || height == 0 {
        return Err("unknown image size");
    }
    let flag = |key: &str| meta(ext, key).trim().eq_ignore_ascii_case("true");
    let restrictions = meta(ext, "Restrictions")
        .split('|')
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    Ok(CommonsImage {
        src,
        width,
        height,
        original_width: if original_width == 0 {
            width
        } else {
            original_width
        },
        original_height: if original_height == 0 {
            height
        } else {
            original_height
        },
        licence,
        licence_url: licence_link(meta(ext, "LicenseUrl")),
        artist: plain_text(meta(ext, "Artist")),
        credit: plain_text(meta(ext, "Credit")),
        source_page: description_page(
            info.get("descriptionurl")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            &file,
        ),
        attribution_required: flag("AttributionRequired"),
        restrictions: plain_text(&restrictions),
        file,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn licence_matrix() {
        let ok = |code: &str, name: &str| licence_verdict(code, name, "");
        // Accepted: public domain, CC0, CC BY, CC BY-SA (any version or port).
        assert_eq!(ok("pd", "Public domain").unwrap(), "Public domain");
        assert_eq!(ok("cc0", "CC0").unwrap(), "CC0");
        assert_eq!(ok("cc-by-3.0", "CC BY 3.0").unwrap(), "CC BY 3.0");
        assert_eq!(ok("cc-by-sa-4.0", "CC BY-SA 4.0").unwrap(), "CC BY-SA 4.0");
        assert_eq!(
            ok("cc-by-sa-3.0-de", "CC BY-SA 3.0 de").unwrap(),
            "CC BY-SA 3.0 de"
        );
        assert_eq!(ok("cc-by-2.0", "").unwrap(), "CC BY 2.0");
        assert_eq!(ok("", "CC BY-SA 4.0").unwrap(), "CC BY-SA 4.0");
        assert_eq!(ok("", "Public domain").unwrap(), "Public domain");
        // Refused: NonCommercial, NoDerivatives, GFDL-only, GPL, fair use, unclear, empty.
        for (code, name) in [
            ("cc-by-nc-4.0", "CC BY-NC 4.0"),
            ("cc-by-nc-sa-3.0", "CC BY-NC-SA 3.0"),
            ("cc-by-nd-4.0", "CC BY-ND 4.0"),
            ("", "CC BY-NC-ND 4.0"),
            ("gfdl", "GFDL"),
            ("", "GFDL"),
            ("", "GPL"),
            ("gpl", "GPL"),
            ("", "Fair use"),
            ("fair use", "Non-free media"),
            ("", "Copyrighted"),
            ("", ""),
            ("cc-by", "Attribution"),
            ("cc-by-sa-4.0", "CC BY-SA 4.0 (non-free logo)"),
        ] {
            assert!(ok(code, name).is_err(), "{code} / {name}");
        }
        // NonFree set refuses even a free-looking licence.
        assert!(licence_verdict("cc-by-sa-4.0", "CC BY-SA 4.0", "true").is_err());
        assert!(licence_verdict("pd", "Public domain", "1").is_err());
        assert!(licence_verdict("pd", "Public domain", "false").is_ok());
    }

    #[test]
    fn display_urls_are_commons_uploads_only() {
        let good = "https://upload.wikimedia.org/wikipedia/commons/thumb/d/d6/Half-Life_2_text_logo.svg/500px-Half-Life_2_text_logo.svg.png";
        assert_eq!(display_url(good).as_deref(), Some(good));
        // Tracking parameters are dropped.
        assert_eq!(
            display_url("https://upload.wikimedia.org/wikipedia/commons/a/a1/X.png?utm_source=commons.wikimedia.org&utm_content=original").as_deref(),
            Some("https://upload.wikimedia.org/wikipedia/commons/a/a1/X.png")
        );
        for bad in [
            // English Wikipedia's own uploads (where non-free files live).
            "https://upload.wikimedia.org/wikipedia/en/a/a1/Cover.jpg",
            "http://upload.wikimedia.org/wikipedia/commons/a/a1/X.png",
            "https://upload.wikimedia.org:8443/wikipedia/commons/a/a1/X.png",
            "https://user@upload.wikimedia.org/wikipedia/commons/a/a1/X.png",
            "https://upload.wikimedia.org.evil.example/wikipedia/commons/a/a1/X.png",
            "https://evil.example/wikipedia/commons/a/a1/X.png",
            "https://upload.wikimedia.org/wikipedia/commons/a/a1/X.svg",
            "https://upload.wikimedia.org/wikipedia/commons/a/a1/X.gif",
            "https://upload.wikimedia.org/wikipedia/commons/../en/a/X.png",
            "upload.wikimedia.org/wikipedia/commons/a/a1/X.png",
        ] {
            assert_eq!(display_url(bad), None, "{bad}");
        }
        assert!(!valid_image_url(
            "https://upload.wikimedia.org/wikipedia/commons/a/a1/X.png?x=1"
        ));
        assert!(!valid_image_url(
            "https://upload.wikimedia.org/wikipedia/commons/a/a1/X.png#f"
        ));
    }

    fn page(extra: Value) -> Value {
        let mut base = json!({
            "title": "File:Half-Life 2 text logo.svg",
            "imagerepository": "local",
            "imageinfo": [{
                "width": 699, "height": 52, "mime": "image/svg+xml",
                "thumburl": "https://upload.wikimedia.org/wikipedia/commons/thumb/d/d6/Half-Life_2_text_logo.svg/500px-Half-Life_2_text_logo.svg.png",
                "thumbwidth": 500, "thumbheight": 37,
                "url": "https://upload.wikimedia.org/wikipedia/commons/d/d6/Half-Life_2_text_logo.svg",
                "descriptionurl": "https://commons.wikimedia.org/wiki/File:Half-Life_2_text_logo.svg",
                "extmetadata": {
                    "License": {"value": "pd"},
                    "LicenseShortName": {"value": "Public domain"},
                    "Artist": {"value": "<a href=\"https://en.wikipedia.org/wiki/Valve\">Valve</a> &amp; co"},
                    "AttributionRequired": {"value": "false"},
                    "Restrictions": {"value": "trademarked|design"}
                }
            }]
        });
        if let (Some(target), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
            for (k, v) in extra {
                target.insert(k.clone(), v.clone());
            }
        }
        base
    }

    #[test]
    fn commons_pages_are_checked() {
        let image = check_page(&page(json!({}))).unwrap();
        assert_eq!(image.file, "Half-Life 2 text logo.svg");
        assert!(image.src.ends_with("500px-Half-Life_2_text_logo.svg.png"));
        assert_eq!((image.width, image.height), (500, 37));
        assert_eq!(image.artist.as_deref(), Some("Valve & co"));
        assert_eq!(image.licence, "Public domain");
        assert_eq!(image.restrictions.as_deref(), Some("trademarked, design"));
        assert_eq!(
            image.source_page,
            "https://commons.wikimedia.org/wiki/File:Half-Life_2_text_logo.svg"
        );
        assert!(!image.attribution_required);
        // Not on Commons (a Wikipedia-only, usually non-free file), or another repository.
        assert!(check_page(
            &json!({"title": "File:Doom cover art.jpg", "missing": true, "imagerepository": ""})
        )
        .is_err());
        assert!(check_page(&page(json!({"imagerepository": "shared"}))).is_err());
        assert!(check_page(&page(json!({"imagerepository": ""}))).is_err());
        // A rendering outside the Commons folder is refused.
        let mut wrong = page(json!({}));
        wrong["imageinfo"][0]["thumburl"] =
            json!("https://upload.wikimedia.org/wikipedia/en/thumb/x.png");
        assert!(check_page(&wrong).is_err());
        // SVG without a rendering, GIF, NonFree, NC.
        let mut svg = page(json!({}));
        svg["imageinfo"][0]
            .as_object_mut()
            .unwrap()
            .remove("thumburl");
        assert!(check_page(&svg).is_err());
        let mut gif = page(json!({}));
        gif["imageinfo"][0]["mime"] = json!("image/gif");
        assert!(check_page(&gif).is_err());
        let mut non_free = page(json!({}));
        non_free["imageinfo"][0]["extmetadata"]["NonFree"] = json!({"value": "true"});
        assert!(check_page(&non_free).is_err());
        let mut nc = page(json!({}));
        nc["imageinfo"][0]["extmetadata"]["License"] = json!({"value": "cc-by-nc-4.0"});
        nc["imageinfo"][0]["extmetadata"]["LicenseShortName"] = json!({"value": "CC BY-NC 4.0"});
        assert!(check_page(&nc).is_err());
        let mut gpl = page(json!({}));
        gpl["imageinfo"][0]["extmetadata"]["License"] = json!({"value": ""});
        gpl["imageinfo"][0]["extmetadata"]["LicenseShortName"] = json!({"value": "GPL"});
        assert!(check_page(&gpl).is_err());
        // A raster original is used as is when Commons gives no rendering.
        let mut png = page(json!({"title": "File:Black-mesa-logo.png"}));
        let info = png["imageinfo"][0].as_object_mut().unwrap();
        info.remove("thumburl");
        info.insert("mime".into(), json!("image/png"));
        info.insert(
            "url".into(),
            json!("https://upload.wikimedia.org/wikipedia/commons/a/ab/Black-mesa-logo.png?utm_source=x"),
        );
        let image = check_page(&png).unwrap();
        assert_eq!(
            image.src,
            "https://upload.wikimedia.org/wikipedia/commons/a/ab/Black-mesa-logo.png"
        );
        assert_eq!((image.width, image.height), (699, 52));
    }

    #[test]
    fn credits_are_plain_text_and_links_are_checked() {
        assert_eq!(
            plain_text("<bdi><a href=\"x\">Nintendo</a></bdi>\u{202e} &lt;3").as_deref(),
            Some("Nintendo <3")
        );
        assert_eq!(plain_text("<p> </p>"), None);
        assert_eq!(plain_text(&"a".repeat(500)).unwrap().chars().count(), 200);
        assert_eq!(
            licence_link("https://creativecommons.org/licenses/by-sa/4.0").as_deref(),
            Some("https://creativecommons.org/licenses/by-sa/4.0")
        );
        assert_eq!(
            licence_link("//creativecommons.org/licenses/by/3.0/").as_deref(),
            Some("https://creativecommons.org/licenses/by/3.0/")
        );
        assert_eq!(licence_link("https://evil.example/licenses/by/3.0/"), None);
        assert_eq!(
            file_key("File:half-Life_2  logo.svg"),
            "Half-Life 2 logo.svg"
        );
        // A missing or odd description page is rebuilt from the file name.
        assert_eq!(
            description_page("https://evil.example/wiki/File:X.png", "My logo.png"),
            "https://commons.wikimedia.org/wiki/File:My_logo.png"
        );
    }
}
