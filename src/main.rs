use clap::Parser;
use reqwest::Client;
use serde::Deserialize;
use std::env;
use std::error::Error;
use std::path::Path;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;

/// A tool to download all video cover images from a YouTube channel.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// The URL of the YouTube channel (e.g., https://www.youtube.com/@handle).
    #[arg(short, long)]
    channel_url: String,

    /// The directory where the images will be saved.
    #[arg(short, long)]
    output_dir: String,
}

// --- Structs for YouTube API Deserialization ---

/// Represents the top-level structure of the YouTube API response for search.
/// Used to find a channel ID from a custom handle.
#[derive(Deserialize, Debug)]
struct SearchListResponse {
    items: Vec<SearchResultItem>,
}

/// Represents a single search result item.
#[derive(Deserialize, Debug)]
struct SearchResultItem {
    id: SearchResultId,
}

/// Contains the ID of the search result (e.g., channelId).
#[derive(Deserialize, Debug)]
struct SearchResultId {
    #[serde(rename = "channelId")]
    channel_id: String,
}

/// Represents the top-level structure of the YouTube API response for channels.
/// Used to get the 'uploads' playlist ID.
#[derive(Deserialize, Debug)]
struct ChannelListResponse {
    items: Vec<ChannelItem>,
}

/// Represents a single channel item in the API response.
#[derive(Deserialize, Debug)]
struct ChannelItem {
    id: Option<String>,
    #[serde(rename = "contentDetails")]
    content_details: Option<ContentDetails>,
}

/// Contains details about the channel's content, including the uploads playlist.
#[derive(Deserialize, Debug)]
struct ContentDetails {
    #[serde(rename = "relatedPlaylists")]
    related_playlists: RelatedPlaylists,
}

/// Contains the ID of the uploads playlist.
#[derive(Deserialize, Debug)]
struct RelatedPlaylists {
    uploads: String,
}

/// Represents the top-level structure of the YouTube API response for playlist items.
#[derive(Deserialize, Debug)]
struct PlaylistItemListResponse {
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
    items: Vec<PlaylistItem>,
}

/// Represents a single video in a playlist.
#[derive(Deserialize, Debug)]
struct PlaylistItem {
    #[serde(rename = "contentDetails")]
    content_details: VideoContentDetails,
}

/// Contains the ID of the video.
#[derive(Deserialize, Debug)]
struct VideoContentDetails {
    #[serde(rename = "videoId")]
    video_id: String,
}

/// Resolves a YouTube channel URL to a channel ID.
/// Handles formats like /@handle, /channel/ID, and /user/username.
async fn get_channel_id_from_url(
    client: &Client,
    api_key: &str,
    channel_url: &str,
) -> Result<String, Box<dyn Error>> {
    let url_path = reqwest::Url::parse(channel_url)?.path().to_string();
    let path_parts: Vec<&str> = url_path.split('/').filter(|s| !s.is_empty()).collect();

    if path_parts.is_empty() {
        return Err("Invalid YouTube channel URL path.".into());
    }

    let first_part = path_parts[0];

    // Handle /@handle format by searching for the handle
    if first_part.starts_with('@') {
        let handle = &first_part[1..];
        println!("Found handle: {}. Searching for channel ID...", handle);
        let search_url = format!(
            "https://www.googleapis.com/youtube/v3/search?part=id&q={}&type=channel&key={}",
            handle, api_key
        );
        let response = client
            .get(&search_url)
            .send()
            .await?
            .json::<SearchListResponse>()
            .await?;
        return response
            .items
            .into_iter()
            .next()
            .map(|item| item.id.channel_id)
            .ok_or_else(|| format!("Could not find a channel ID for handle: {}", handle).into());
    }

    // Handle /channel/ID and /user/username formats
    if path_parts.len() >= 2 {
        let type_part = path_parts[0];
        let identifier = path_parts[1];

        // If it's a /channel/ID URL, the ID is right there.
        if type_part == "channel" {
            println!("Found channel ID directly in URL: {}", identifier);
            return Ok(identifier.to_string());
        }

        // If it's a legacy /user/username URL, we need to look it up.
        if type_part == "user" {
            println!(
                "Found legacy username: {}. Searching for channel ID...",
                identifier
            );
            let channel_list_url = format!(
                "https://www.googleapis.com/youtube/v3/channels?part=id&forUsername={}&key={}",
                identifier, api_key
            );
            let response = client
                .get(&channel_list_url)
                .send()
                .await?
                .json::<ChannelListResponse>()
                .await?;
            return response
                .items
                .into_iter()
                .next()
                .and_then(|item| item.id)
                .ok_or_else(|| {
                    format!("Could not find a channel ID for username: {}", identifier).into()
                });
        }
    }

    Err("Unsupported YouTube channel URL format. Please use a URL like https://www.youtube.com/@handle, https://www.youtube.com/channel/ID, or https://www.youtube.com/user/username".into())
}

/// Fetches the uploads playlist ID for a given YouTube channel ID.
async fn get_uploads_playlist_id(
    client: &Client,
    api_key: &str,
    channel_id: &str,
) -> Result<String, Box<dyn Error>> {
    let url = format!(
        "https://www.googleapis.com/youtube/v3/channels?part=contentDetails&id={}&key={}",
        channel_id, api_key
    );
    let response = client
        .get(&url)
        .send()
        .await?
        .json::<ChannelListResponse>()
        .await?;

    if let Some(item) = response.items.into_iter().next() {
        if let Some(details) = item.content_details {
            return Ok(details.related_playlists.uploads);
        }
    }
    Err("Could not find uploads playlist for the channel.".into())
}

/// Fetches all video IDs from a given playlist.
async fn get_all_video_ids(
    client: &Client,
    api_key: &str,
    playlist_id: &str,
) -> Result<Vec<String>, Box<dyn Error>> {
    let mut video_ids = Vec::new();
    let mut page_token: Option<String> = None;

    loop {
        let mut url = format!(
            "https://www.googleapis.com/youtube/v3/playlistItems?part=contentDetails&playlistId={}&key={}&maxResults=50",
            playlist_id, api_key
        );

        if let Some(token) = &page_token {
            url.push_str(&format!("&pageToken={}", token));
        }

        let response: PlaylistItemListResponse = client.get(&url).send().await?.json().await?;

        for item in response.items {
            video_ids.push(item.content_details.video_id);
        }

        page_token = response.next_page_token;
        if page_token.is_none() {
            break;
        }
    }

    Ok(video_ids)
}

/// Downloads a single video thumbnail at its highest resolution.
async fn download_thumbnail(
    client: &Client,
    video_id: &str,
    output_dir: &str,
) -> Result<(), Box<dyn Error>> {
    // maxresdefault provides the highest possible resolution.
    let thumbnail_url = format!("https://img.youtube.com/vi/{}/maxresdefault.jpg", video_id);
    let response = client.get(&thumbnail_url).send().await?;

    if response.status().is_success() {
        let file_path = Path::new(output_dir).join(format!("{}.jpg", video_id));
        let mut file = File::create(&file_path).await?;
        let bytes = response.bytes().await?;
        file.write_all(&bytes).await?;
        println!("Downloaded thumbnail for video ID: {}", video_id);
    } else {
        // If maxresdefault.jpg doesn't exist, YouTube returns a 404.
        // We could add a fallback to 'hqdefault.jpg' here if needed.
        eprintln!(
            "Failed to download max-res thumbnail for video ID {}. It might not exist. Status: {}",
            video_id,
            response.status()
        );
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    let api_key =
        env::var("YOUTUBE_API_KEY").map_err(|_| "YOUTUBE_API_KEY environment variable not set.")?;

    let client = Client::new();

    // Create the output directory if it doesn't exist
    fs::create_dir_all(&args.output_dir).await?;

    println!("Resolving channel URL: {}", args.channel_url);
    let channel_id = get_channel_id_from_url(&client, &api_key, &args.channel_url).await?;
    println!("Resolved to channel ID: {}", channel_id);

    println!("Fetching uploads playlist ID for channel...");
    let uploads_playlist_id = get_uploads_playlist_id(&client, &api_key, &channel_id).await?;
    println!("Found uploads playlist ID: {}", uploads_playlist_id);

    println!("Fetching all video IDs from the playlist...");
    let video_ids = get_all_video_ids(&client, &api_key, &uploads_playlist_id).await?;
    println!("Found {} videos in the channel.", video_ids.len());

    let mut download_tasks = Vec::new();

    for video_id in &video_ids {
        let client = client.clone();
        let output_dir = args.output_dir.clone();
        let video_id = video_id.clone();

        let task = tokio::spawn(async move {
            if let Err(e) = download_thumbnail(&client, &video_id, &output_dir).await {
                eprintln!("Error downloading thumbnail for {}: {}", video_id, e);
            }
        });
        download_tasks.push(task);
    }

    // Wait for all the download tasks to complete.
    for task in download_tasks {
        task.await?;
    }

    println!("\nDownload process finished!");
    Ok(())
}
