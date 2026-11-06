//! Matchup thumbnail: a 16:9 PNG with the away logo, "at", and the home logo.
//!
//! [`render_matchup`] draws the image. [`ArtStore`] resolves the logos through
//! the [`EspnClient`] cache and keeps the rendered PNG files on disk under a
//! size limit.

// Pixel math converts between small image sizes and floats. All values are below 2^23.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use chrono::NaiveDate;
use image::imageops::{self, FilterType};
use image::{ImageFormat, Rgba, RgbaImage};

use crate::client::{EspnClient, EspnError};
use crate::league::League;

/// Image width in pixels.
pub const WIDTH: u32 = 1280;
/// Image height in pixels. The image is 16:9.
pub const HEIGHT: u32 = 720;
/// The default size limit of the disk cache: 64 MiB.
pub const DEFAULT_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// `DejaVu Sans Bold`. The license is in `assets/DEJAVU-LICENSE.txt`.
const FONT_BYTES: &[u8] = include_bytes!("../assets/DejaVuSans-Bold.ttf");
const BACKGROUND: Rgba<u8> = Rgba([38, 42, 50, 255]);
const TEXT: Rgba<u8> = Rgba([220, 224, 230, 255]);
const LOGO_BOX: u32 = 460;

/// Errors from the art code.
#[derive(Debug, thiserror::Error)]
pub enum ArtError {
    #[error("the event key is not valid")]
    BadKey,
    #[error("the event or its logos are not known")]
    NotFound,
    #[error("the logo URL is not an ESPN URL")]
    BadLogoHost,
    #[error(transparent)]
    Espn(#[from] EspnError),
    #[error("image error: {0}")]
    Image(String),
    #[error("disk error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<image::ImageError> for ArtError {
    fn from(error: image::ImageError) -> Self {
        Self::Image(error.to_string())
    }
}

/// Builds the event key `<LEAGUE>-<YYYYMMDD>-<event id>`.
///
/// Returns `None` when the event id has a character other than a digit or a letter.
#[must_use]
pub fn event_key(league: &str, local_date: NaiveDate, event_id: &str) -> Option<String> {
    let valid = |text: &str| !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric());
    if !valid(league) || !valid(event_id) {
        return None;
    }
    Some(format!(
        "{league}-{}-{event_id}",
        local_date.format("%Y%m%d")
    ))
}

/// Parses an event key into the league, the local date, and the event id.
#[must_use]
pub fn parse_event_key(key: &str) -> Option<(League, NaiveDate, String)> {
    let mut parts = key.splitn(3, '-');
    let league = League::from_key(parts.next()?)?;
    let date = NaiveDate::parse_from_str(parts.next()?, "%Y%m%d").ok()?;
    let id = parts.next()?;
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some((league, date, id.to_owned()))
}

fn fit_logo(bytes: &[u8]) -> Result<RgbaImage, ArtError> {
    let logo = image::load_from_memory(bytes)?.to_rgba8();
    let (width, height) = logo.dimensions();
    if width == 0 || height == 0 {
        return Err(ArtError::Image("empty logo".to_owned()));
    }
    let scale = f64::from(LOGO_BOX) / f64::from(width.max(height));
    let new_width = (f64::from(width) * scale).round().max(1.0) as u32;
    let new_height = (f64::from(height) * scale).round().max(1.0) as u32;
    Ok(imageops::resize(
        &logo,
        new_width,
        new_height,
        FilterType::Lanczos3,
    ))
}

fn draw_text_centered(canvas: &mut RgbaImage, text: &str, center_x: f32, center_y: f32) {
    let Ok(font) = FontRef::try_from_slice(FONT_BYTES) else {
        return;
    };
    let scale = PxScale::from(96.0);
    let scaled = font.as_scaled(scale);
    let width: f32 = text
        .chars()
        .map(|c| scaled.h_advance(font.glyph_id(c)))
        .sum();
    let mut x = center_x - width / 2.0;
    let baseline = center_y + f32::midpoint(scaled.ascent(), scaled.descent());
    for character in text.chars() {
        let id = font.glyph_id(character);
        let glyph = id.with_scale_and_position(scale, ab_glyph::point(x, baseline));
        x += scaled.h_advance(id);
        if let Some(outline) = font.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, coverage| {
                let px = bounds.min.x as i64 + i64::from(gx);
                let py = bounds.min.y as i64 + i64::from(gy);
                if px < 0 || py < 0 || px >= i64::from(WIDTH) || py >= i64::from(HEIGHT) {
                    return;
                }
                let pixel = canvas.get_pixel_mut(px as u32, py as u32);
                for channel in 0..3 {
                    let base = f32::from(pixel[channel]);
                    let top = f32::from(TEXT[channel]);
                    pixel[channel] = (base + (top - base) * coverage).round() as u8;
                }
            });
        }
    }
}

/// Renders the matchup image and returns the PNG bytes.
///
/// # Errors
///
/// Returns an error when a logo cannot be decoded or the PNG cannot be written.
pub fn render_matchup(away_logo: &[u8], home_logo: &[u8]) -> Result<Vec<u8>, ArtError> {
    let mut canvas = RgbaImage::from_pixel(WIDTH, HEIGHT, BACKGROUND);
    let slots = [
        (away_logo, 40_i64),
        (home_logo, i64::from(WIDTH - 40 - LOGO_BOX)),
    ];
    for (bytes, left) in slots {
        let logo = fit_logo(bytes)?;
        let x = left + (i64::from(LOGO_BOX) - i64::from(logo.width())) / 2;
        let y = (i64::from(HEIGHT) - i64::from(logo.height())) / 2;
        imageops::overlay(&mut canvas, &logo, x, y);
    }
    draw_text_centered(&mut canvas, "at", WIDTH as f32 / 2.0, HEIGHT as f32 / 2.0);
    let mut out = Cursor::new(Vec::new());
    canvas.write_to(&mut out, ImageFormat::Png)?;
    Ok(out.into_inner())
}

fn allowed_logo_host(url: &str) -> bool {
    url::host_of(url).is_some_and(|host| host == "espncdn.com" || host.ends_with(".espncdn.com"))
}

mod url {
    /// Returns the host of an `https` URL.
    pub fn host_of(url: &str) -> Option<&str> {
        let rest = url.strip_prefix("https://")?;
        let authority = rest.split(['/', '?', '#']).next()?;
        if authority.contains(['@', ':']) {
            return None;
        }
        Some(authority)
    }
}

/// Renders matchup images on demand and caches the PNG files on disk.
#[derive(Clone, Debug)]
pub struct ArtStore {
    dir: PathBuf,
    max_bytes: u64,
    client: EspnClient,
    /// Allow any logo host. Only tests set this.
    allow_any_host: bool,
}

impl ArtStore {
    /// Creates a store. The directory is created when the first image is written.
    #[must_use]
    pub fn new(dir: PathBuf, max_bytes: u64, client: EspnClient) -> Self {
        Self {
            dir,
            max_bytes,
            client,
            allow_any_host: false,
        }
    }

    /// Allows logo URLs on any host. Use it in tests with a local server.
    #[must_use]
    pub fn allow_any_logo_host(mut self) -> Self {
        self.allow_any_host = true;
        self
    }

    fn path_for(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.png"))
    }

    /// Returns the PNG bytes for an event key.
    ///
    /// A cached file is served first. Otherwise the store finds the event on the
    /// ESPN scoreboard, fetches both logos, renders, and stores the image.
    ///
    /// # Errors
    ///
    /// Returns [`ArtError::BadKey`] for a malformed key and
    /// [`ArtError::NotFound`] when ESPN has no such event with two logos.
    pub async fn get(&self, key: &str) -> Result<Vec<u8>, ArtError> {
        let (league, date, id) = parse_event_key(key).ok_or(ArtError::BadKey)?;
        let path = self.path_for(key);
        if let Ok(bytes) = fs::read(&path) {
            return Ok(bytes);
        }
        let events = self.client.scoreboard(league, date).await?;
        let event = events
            .iter()
            .find(|event| event.id == id)
            .ok_or(ArtError::NotFound)?;
        let (Some(away_url), Some(home_url)) = (&event.away.logo_url, &event.home.logo_url) else {
            return Err(ArtError::NotFound);
        };
        if !(self.allow_any_host || allowed_logo_host(away_url) && allowed_logo_host(home_url)) {
            return Err(ArtError::BadLogoHost);
        }
        let away = self.client.logo(away_url).await?;
        let home = self.client.logo(home_url).await?;
        let png = render_matchup(&away, &home)?;
        self.store(key, &png)?;
        Ok(png)
    }

    /// Writes a rendered image and removes old files above the size limit.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be written.
    pub fn store(&self, key: &str, png: &[u8]) -> Result<(), ArtError> {
        fs::create_dir_all(&self.dir)?;
        let path = self.path_for(key);
        let temporary = self.dir.join(format!("{key}.png.tmp"));
        fs::write(&temporary, png)?;
        fs::rename(&temporary, &path)?;
        enforce_size_limit(&self.dir, self.max_bytes, &path);
        Ok(())
    }
}

/// Removes the oldest `.png` files until the total size is at most `max_bytes`.
///
/// The file `keep` stays, even when it alone is above the limit.
pub fn enforce_size_limit(dir: &Path, max_bytes: u64, keep: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            Some((meta.modified().ok()?, meta.len(), entry.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
    files.sort();
    for (_, size, path) in files {
        if total <= max_bytes {
            break;
        }
        if path == keep {
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logo(color: [u8; 4], size: (u32, u32)) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        RgbaImage::from_pixel(size.0, size.1, Rgba(color))
            .write_to(&mut out, ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn rendered_image_is_16_by_9_png() {
        let png = render_matchup(
            &logo([200, 0, 0, 255], (500, 500)),
            &logo([0, 0, 200, 255], (300, 600)),
        )
        .unwrap();
        let decoded = image::load_from_memory_with_format(&png, ImageFormat::Png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (1280, 720));
        assert_eq!(decoded.width() * 9, decoded.height() * 16);
        let rgba = decoded.to_rgba8();
        assert_eq!(rgba.get_pixel(270, 360)[0], 200, "away logo at the left");
        assert_eq!(rgba.get_pixel(1010, 360)[2], 200, "home logo at the right");
        assert_eq!(*rgba.get_pixel(5, 5), BACKGROUND);
        let middle = (600..680).any(|x| (330..390).any(|y| *rgba.get_pixel(x, y) != BACKGROUND));
        assert!(middle, "the word at is drawn");
    }

    #[test]
    fn bad_logo_is_an_error() {
        assert!(render_matchup(b"not a png", b"not a png").is_err());
    }

    #[test]
    fn event_key_round_trips_and_rejects_bad_input() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        let key = event_key("NHL", date, "401").unwrap();
        assert_eq!(key, "NHL-20261104-401");
        assert_eq!(
            parse_event_key(&key),
            Some((League::Nhl, date, "401".to_owned()))
        );
        assert!(event_key("NHL", date, "../x").is_none());
        for bad in [
            "",
            "NHL",
            "XYZ-20261104-1",
            "NHL-2026-1",
            "NHL-20261104-a/b",
            "NHL-20261104-",
        ] {
            assert!(parse_event_key(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn logo_host_must_be_espn() {
        assert!(allowed_logo_host("https://a.espncdn.com/i/x.png"));
        assert!(!allowed_logo_host("http://a.espncdn.com/i/x.png"));
        assert!(!allowed_logo_host("https://evil.test/a.espncdn.com"));
        assert!(!allowed_logo_host("https://a.espncdn.com@evil.test/x.png"));
        assert!(!allowed_logo_host("https://notespncdn.com/x.png"));
    }

    #[test]
    fn cache_respects_size_limit_and_keeps_newest() {
        let dir = tempfile::tempdir().unwrap();
        let client = EspnClient::new().unwrap();
        let store = ArtStore::new(dir.path().to_path_buf(), 250, client);
        for index in 0..5 {
            store
                .store(&format!("NHL-20261104-{index}"), &[0_u8; 100])
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["NHL-20261104-3.png", "NHL-20261104-4.png"]);
    }

    #[tokio::test]
    async fn cached_file_is_served_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtStore::new(dir.path().to_path_buf(), 1024, EspnClient::new().unwrap());
        store.store("NHL-20261104-401", b"png-bytes").unwrap();
        assert_eq!(store.get("NHL-20261104-401").await.unwrap(), b"png-bytes");
        assert!(matches!(store.get("bad").await, Err(ArtError::BadKey)));
    }
}
