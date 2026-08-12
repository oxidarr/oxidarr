//! The canonical release record produced by every indexer implementation.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One search result, normalised across all indexer protocols.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Release {
    /// Release name as published by the tracker.
    pub title: String,
    /// Size in bytes. Zero when the tracker does not report one.
    pub size: u64,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    pub grabs: Option<u32>,
    /// Human-facing details page.
    pub details_url: Option<String>,
    /// Direct download URL for the `.torrent` or `.nzb`.
    pub download_url: Option<String>,
    /// Magnet URI, when the tracker offers one.
    pub magnet_url: Option<String>,
    pub info_hash: Option<String>,
    pub publish_date: Option<DateTime<Utc>>,
    /// Newznab category ids this release maps onto.
    pub categories: Vec<u32>,
    /// Ratio multiplier applied to downloaded bytes; 0.0 means freeleech.
    pub download_volume_factor: f32,
    /// Ratio multiplier applied to uploaded bytes.
    pub upload_volume_factor: f32,
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<u32>,
    pub tvdb_id: Option<u32>,
}

impl Default for Release {
    fn default() -> Self {
        Self {
            title: String::new(),
            size: 0,
            seeders: None,
            leechers: None,
            grabs: None,
            details_url: None,
            download_url: None,
            magnet_url: None,
            info_hash: None,
            publish_date: None,
            categories: Vec::new(),
            download_volume_factor: 1.0,
            upload_volume_factor: 1.0,
            imdb_id: None,
            tmdb_id: None,
            tvdb_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_volume_factors_are_neutral() {
        let r = Release::default();
        assert!((r.download_volume_factor - 1.0).abs() < f32::EPSILON);
        assert!((r.upload_volume_factor - 1.0).abs() < f32::EPSILON);
    }
}
