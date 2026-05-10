//! Shared qBittorrent WebUI types.

#![allow(dead_code)]

use serde::{Deserialize, Deserializer};

/// Where the torrent to add comes from.
#[derive(Debug, Clone)]
pub enum TorrentSource {
    /// Raw `.torrent` bytes, uploaded as a multipart file part. `filename`
    /// is the display name qBittorrent will log/show.
    File { filename: String, bytes: Vec<u8> },
    /// A magnet URI or `http(s)://...torrent` URL. qBittorrent fetches it
    /// itself.
    Url(String),
}

/// Parameters for `POST /api/v2/torrents/add`. Mirrors the subset of fields
/// `tql` needs; other fields can be added as legs require them.
#[derive(Debug, Clone, Default)]
pub struct AddTorrentParams {
    pub category: Option<String>,
    /// qBittorrent expects a comma-separated list in a single `tags` field.
    pub tags: Vec<String>,
    pub paused: Option<bool>,
    /// `autoTMM` — automatic torrent management. When set, qBittorrent
    /// chooses the save path from the category config; when false, the
    /// torrent uses `savepath`.
    pub auto_tmm: Option<bool>,
    pub savepath: Option<String>,
}

/// Optional filters for `GET /api/v2/torrents/info`. All fields are optional;
/// `Default` returns "no filters" which lists every torrent.
#[derive(Debug, Clone, Default)]
pub struct TorrentsInfoQuery {
    /// Restrict to these info hashes. Sent as a CSV string in the `hashes`
    /// query parameter. Empty = no restriction.
    pub hashes: Vec<String>,
    /// Restrict to this category.
    pub category: Option<String>,
    /// Restrict to torrents carrying this tag.
    pub tag: Option<String>,
}

/// A single entry returned by `GET /api/v2/torrents/info`. We deserialize
/// only the fields `tql` cares about — qBittorrent returns many more (state,
/// progress, ratio, …) and we'll add them as later legs need them.
#[derive(Debug, Clone, Deserialize)]
pub struct TorrentInfo {
    pub hash: String,
    pub name: String,
    /// qBittorrent returns `""` for "no category"; we normalize to `None`.
    #[serde(default, deserialize_with = "deserialize_empty_as_none")]
    pub category: Option<String>,
    /// qBittorrent returns tags as a single comma-separated string; we split
    /// into a `Vec` (empty when there are no tags).
    #[serde(default, deserialize_with = "deserialize_csv_tags")]
    pub tags: Vec<String>,
    pub save_path: String,
}

fn deserialize_empty_as_none<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = Option::<String>::deserialize(d)?;
    Ok(s.filter(|v| !v.is_empty()))
}

fn deserialize_csv_tags<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = Option::<String>::deserialize(d)?.unwrap_or_default();
    Ok(s.split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect())
}
