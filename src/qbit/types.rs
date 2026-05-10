//! Shared qBittorrent WebUI types.

#![allow(dead_code)]

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
