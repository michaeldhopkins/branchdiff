//! Image diff support for displaying image files in the TUI.
//!
//! This module handles:
//! - Detection of image files by extension (dynamic via `image` crate)
//! - SVG rasterization via `resvg`
//! - Image loading, downscaling, and caching
//! - LRU cache eviction

use anyhow::{Context, Result};
use image::{DynamicImage, ImageFormat};
use ratatui::layout::Rect;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

/// Maximum dimension for cached images (larger images are downscaled)
pub const MAX_CACHE_DIMENSION: u32 = 1024;

/// Maximum number of images to keep in cache
pub const MAX_CACHED_IMAGES: usize = 10;

/// Check if a file is a supported image format.
/// Uses the `image` crate's format detection - no hardcoded extension list.
pub fn is_image_file(path: &str) -> bool {
    // SVG handled separately via resvg
    if is_svg(path) {
        return true;
    }

    // Use image crate's extension detection + readability check
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    ImageFormat::from_extension(ext)
        .map(|fmt| fmt.can_read())
        .unwrap_or(false)
}

/// Check if a file is an SVG (handled via resvg, not image crate)
pub fn is_svg(path: &str) -> bool {
    path.to_lowercase().ends_with(".svg")
}

/// Check if content is a Git LFS pointer (not actual file content)
pub fn is_lfs_pointer(content: &[u8]) -> bool {
    content.starts_with(b"version https://git-lfs.github.com/spec/")
}

/// A loaded and cached image ready for display
pub struct CachedImage {
    /// Downscaled image for display (fits in MAX_CACHE_DIMENSION)
    pub display_image: DynamicImage,
    /// Original dimensions (for metadata display)
    pub original_width: u32,
    pub original_height: u32,
    /// Original file size in bytes (for metadata display)
    pub file_size: u64,
    /// Image format name (e.g., "PNG", "JPEG", "SVG")
    pub format_name: String,
}

impl CachedImage {
    /// Format metadata for display below image
    pub fn metadata_string(&self) -> String {
        let size = format_file_size(self.file_size);
        format!(
            "{}x{} {}, {}",
            self.original_width, self.original_height, self.format_name, size
        )
    }
}

/// Image diff state for a single file (before and after versions)
pub struct ImageDiffState {
    /// Before image (from base/merge-base), None if file is new
    pub before: Option<CachedImage>,
    /// After image (from working tree), None if file is deleted
    pub after: Option<CachedImage>,
}

/// LRU cache for loaded images
pub struct ImageCache {
    images: HashMap<String, ImageDiffState>,
    access_order: VecDeque<String>, // Most recent at back
}

impl Default for ImageCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageCache {
    pub fn new() -> Self {
        Self {
            images: HashMap::new(),
            access_order: VecDeque::new(),
        }
    }

    /// Get an image from cache, updating access order
    pub fn get(&mut self, path: &str) -> Option<&ImageDiffState> {
        if self.images.contains_key(path) {
            // Move to back of access order (most recently used)
            self.access_order.retain(|p| p != path);
            self.access_order.push_back(path.to_string());
            self.images.get(path)
        } else {
            None
        }
    }

    /// Get mutable reference to image from cache
    pub fn get_mut(&mut self, path: &str) -> Option<&mut ImageDiffState> {
        if self.images.contains_key(path) {
            self.access_order.retain(|p| p != path);
            self.access_order.push_back(path.to_string());
            self.images.get_mut(path)
        } else {
            None
        }
    }

    /// Check if path is in cache
    pub fn contains(&self, path: &str) -> bool {
        self.images.contains_key(path)
    }

    /// Insert an image into cache, evicting LRU if necessary
    pub fn insert(&mut self, path: String, state: ImageDiffState) {
        // Evict LRU if at capacity
        while self.images.len() >= MAX_CACHED_IMAGES {
            if let Some(oldest) = self.access_order.pop_front() {
                self.images.remove(&oldest);
            } else {
                break;
            }
        }

        self.access_order.push_back(path.clone());
        self.images.insert(path, state);
    }

    /// Remove stale images that are no longer in the diff
    pub fn evict_stale(&mut self, current_image_paths: &HashSet<&str>) {
        self.images
            .retain(|path, _| current_image_paths.contains(path.as_str()));
        self.access_order
            .retain(|path| current_image_paths.contains(path.as_str()));
    }

    /// Clear entire cache
    pub fn clear(&mut self) {
        self.images.clear();
        self.access_order.clear();
    }

    /// Number of cached images
    pub fn len(&self) -> usize {
        self.images.len()
    }

    /// Check if cache is empty
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }
}

/// Load image bytes and create a cached image with optional downscaling
pub fn load_and_cache(bytes: &[u8], format_name: &str) -> Result<CachedImage> {
    let file_size = bytes.len() as u64;

    let original = image::load_from_memory(bytes).context("Failed to decode image")?;
    let (ow, oh) = (original.width(), original.height());

    // Downscale if larger than cache limit
    let display_image = if ow > MAX_CACHE_DIMENSION || oh > MAX_CACHE_DIMENSION {
        let scale = MAX_CACHE_DIMENSION as f64 / ow.max(oh) as f64;
        let new_w = ((ow as f64) * scale) as u32;
        let new_h = ((oh as f64) * scale) as u32;
        original.resize(new_w, new_h, image::imageops::FilterType::Lanczos3)
    } else {
        original
    };

    Ok(CachedImage {
        display_image,
        original_width: ow,
        original_height: oh,
        file_size,
        format_name: format_name.to_string(),
    })
}

/// Rasterize SVG bytes to a DynamicImage
pub fn rasterize_svg(svg_bytes: &[u8], max_dimension: u32) -> Result<CachedImage> {
    let file_size = svg_bytes.len() as u64;

    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(svg_bytes, &options).context("Failed to parse SVG")?;
    let size = tree.size();

    // Scale to fit max_dimension while preserving aspect ratio
    let max_size = size.width().max(size.height());
    let scale = (max_dimension as f32 / max_size).min(1.0);
    let width = ((size.width() * scale) as u32).max(1);
    let height = ((size.height() * scale) as u32).max(1);

    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| anyhow::anyhow!("Failed to create pixmap for {}x{}", width, height))?;

    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // Convert to image::DynamicImage
    let rgba = image::RgbaImage::from_raw(width, height, pixmap.take())
        .ok_or_else(|| anyhow::anyhow!("Failed to create image from pixmap"))?;

    Ok(CachedImage {
        display_image: DynamicImage::ImageRgba8(rgba),
        original_width: size.width() as u32,
        original_height: size.height() as u32,
        file_size,
        format_name: "SVG".to_string(),
    })
}

/// Calculate display dimensions maintaining aspect ratio.
/// Terminal cells are approximately 2:1 (height:width in pixels).
pub fn fit_dimensions(img_width: u32, img_height: u32, max_w: u16, max_h: u16) -> (u16, u16) {
    // Handle zero inputs gracefully
    if img_width == 0 || img_height == 0 || max_w == 0 || max_h == 0 {
        return (1, 1);
    }

    // Terminal cells are ~2:1 (height:width in pixels), so adjust
    let cell_aspect = 2.0;
    let effective_max_h = (max_h as f64 * cell_aspect) as u32;

    let scale_w = max_w as f64 / img_width as f64;
    let scale_h = effective_max_h as f64 / img_height as f64;
    let scale = scale_w.min(scale_h).min(1.0); // Never upscale

    let display_w = ((img_width as f64) * scale) as u16;
    let display_h = ((img_height as f64) * scale / cell_aspect) as u16;

    (display_w.max(1), display_h.max(1))
}

/// Center a smaller rectangle within a larger area
pub fn center_in_area(img_w: u16, img_h: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(img_w) / 2;
    let y = area.y + area.height.saturating_sub(img_h) / 2;
    Rect::new(x, y, img_w, img_h)
}

/// Format file size for human-readable display
pub fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Get format name from file path extension
pub fn format_name_from_path(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_uppercase())
        .unwrap_or_else(|| "Unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_image_file_common_formats() {
        assert!(is_image_file("photo.png"));
        assert!(is_image_file("photo.PNG")); // Case insensitive
        assert!(is_image_file("icon.jpeg"));
        assert!(is_image_file("icon.jpg"));
        assert!(is_image_file("anim.gif"));
        assert!(is_image_file("modern.webp"));
        assert!(is_image_file("icon.bmp"));
        assert!(is_image_file("favicon.ico"));
    }

    #[test]
    fn test_is_image_file_svg() {
        assert!(is_image_file("LOGO.SVG"));
        assert!(is_image_file("diagram.svg"));
    }

    #[test]
    fn test_is_image_file_not_images() {
        assert!(!is_image_file("document.pdf"));
        assert!(!is_image_file("code.rs"));
        assert!(!is_image_file("data.json"));
        assert!(!is_image_file("video.mp4"));
        assert!(!is_image_file("noextension"));
    }

    #[test]
    fn test_is_svg() {
        assert!(is_svg("logo.svg"));
        assert!(is_svg("DIAGRAM.SVG"));
        assert!(!is_svg("photo.png"));
    }

    #[test]
    fn test_is_lfs_pointer() {
        let lfs_content = b"version https://git-lfs.github.com/spec/v1\noid sha256:abc123\nsize 12345";
        assert!(is_lfs_pointer(lfs_content));

        let normal_content = b"\x89PNG\r\n\x1a\n"; // PNG header
        assert!(!is_lfs_pointer(normal_content));
    }

    #[test]
    fn test_fit_dimensions_landscape() {
        // 1920x1080 into 80x24 panel
        let (w, h) = fit_dimensions(1920, 1080, 80, 24);
        assert!(w <= 80);
        assert!(h <= 24);
    }

    #[test]
    fn test_fit_dimensions_portrait() {
        // 600x1200 into 40x30 panel
        let (w, h) = fit_dimensions(600, 1200, 40, 30);
        assert!(w <= 40);
        assert!(h <= 30);
    }

    #[test]
    fn test_fit_dimensions_no_upscale() {
        // 10x10 into 80x24 - should stay small (no upscaling)
        let (w, h) = fit_dimensions(10, 10, 80, 24);
        assert!(w <= 10);
        assert!(h <= 10);
    }

    #[test]
    fn test_fit_dimensions_zero_input() {
        // Should not panic, should return minimum valid size
        let (w, h) = fit_dimensions(0, 0, 80, 24);
        assert!(w >= 1);
        assert!(h >= 1);
    }

    #[test]
    fn test_fit_dimensions_zero_container() {
        // Container has no space - should handle gracefully
        let (w, h) = fit_dimensions(100, 100, 0, 0);
        assert!(w >= 1);
        assert!(h >= 1);
    }

    #[test]
    fn test_fit_dimensions_huge_image() {
        // 100k x 100k image - should not overflow
        let (w, h) = fit_dimensions(100_000, 100_000, 80, 24);
        assert!(w <= 80);
        assert!(h <= 24);
    }

    #[test]
    fn test_center_in_area() {
        let area = Rect::new(0, 0, 80, 24);
        let centered = center_in_area(20, 10, area);
        assert_eq!(centered.x, 30); // (80-20)/2
        assert_eq!(centered.y, 7); // (24-10)/2
        assert_eq!(centered.width, 20);
        assert_eq!(centered.height, 10);
    }

    #[test]
    fn test_format_file_size() {
        assert_eq!(format_file_size(512), "512 B");
        assert_eq!(format_file_size(1024), "1.0 KB");
        assert_eq!(format_file_size(1536), "1.5 KB");
        assert_eq!(format_file_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_file_size(2 * 1024 * 1024 + 512 * 1024), "2.5 MB");
    }

    #[test]
    fn test_format_name_from_path() {
        assert_eq!(format_name_from_path("photo.png"), "PNG");
        assert_eq!(format_name_from_path("icon.jpeg"), "JPEG");
        assert_eq!(format_name_from_path("logo.svg"), "SVG");
        assert_eq!(format_name_from_path("noextension"), "Unknown");
    }

    #[test]
    fn test_image_cache_lru_eviction() {
        let mut cache = ImageCache::new();

        // Fill cache to capacity
        for i in 0..MAX_CACHED_IMAGES {
            let path = format!("image{}.png", i);
            cache.insert(
                path,
                ImageDiffState {
                    before: None,
                    after: None,
                },
            );
        }

        assert_eq!(cache.len(), MAX_CACHED_IMAGES);

        // Insert one more - should evict oldest
        cache.insert(
            "new_image.png".to_string(),
            ImageDiffState {
                before: None,
                after: None,
            },
        );

        assert_eq!(cache.len(), MAX_CACHED_IMAGES);
        assert!(!cache.contains("image0.png")); // First one evicted
        assert!(cache.contains("new_image.png")); // New one present
    }

    #[test]
    fn test_image_cache_evict_stale() {
        let mut cache = ImageCache::new();

        cache.insert(
            "keep.png".to_string(),
            ImageDiffState {
                before: None,
                after: None,
            },
        );
        cache.insert(
            "remove.png".to_string(),
            ImageDiffState {
                before: None,
                after: None,
            },
        );

        let current: HashSet<&str> = ["keep.png"].iter().copied().collect();
        cache.evict_stale(&current);

        assert!(cache.contains("keep.png"));
        assert!(!cache.contains("remove.png"));
    }

    #[test]
    fn test_cached_image_metadata_string() {
        let cached = CachedImage {
            display_image: DynamicImage::new_rgba8(1, 1),
            original_width: 1920,
            original_height: 1080,
            file_size: 2 * 1024 * 1024,
            format_name: "PNG".to_string(),
        };

        assert_eq!(cached.metadata_string(), "1920x1080 PNG, 2.0 MB");
    }
}
