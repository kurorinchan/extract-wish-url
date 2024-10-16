use aho_corasick::AhoCorasick;
use anyhow::{Context, Result};
use enum_assoc::Assoc;
use std::cmp::Ordering;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use strum::IntoEnumIterator;
use strum_macros::EnumIter;

// If there are more games, add them here.
#[derive(Debug, Assoc, EnumIter)]
#[func(fn data_dir(&self) -> &'static str)]
#[func(fn marker(&self) -> &'static str)]
#[func(fn url_start(&self) -> &'static str)]
#[func(fn url_end(&self) -> &'static str)]
enum GameType {
    #[assoc(data_dir = "GenshinImpact_Data")]
    #[assoc(marker = "e20190909gacha-v3")]
    #[assoc(url_start = "https://gs.hoyoverse.com/")]
    #[assoc(url_end = "game_biz=hk4e_global")]
    GenshinGlobal,
    #[assoc(data_dir = "ZenlessZoneZero_Data")]
    #[assoc(marker = "e20230424gacha")]
    #[assoc(url_start = "https://gs.hoyoverse.com/")]
    #[assoc(url_end = "game_biz=nap_global")]
    ZenlessZoneZeroGlobal,
}

// Genshin's version folders have 4 numbers.
// The field names are arbitrary names that I gave, not from any source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Version {
    major: u32,
    minor: u32,
    patch: u32,
    other: u32,
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| self.minor.cmp(&other.minor))
            .then_with(|| self.patch.cmp(&other.patch))
            .then_with(|| self.other.cmp(&other.other))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct VersionedDirectory {
    path: PathBuf,
    version: Version,
}

struct PullExtractor {
    data_dir: PathBuf,
    url_start: String,
    marker: String,
    end_marker: String,
}

impl PullExtractor {
    fn new(install_path: &Path) -> Result<Self> {
        let game_type = GameType::iter()
            .find_map(|game_type| {
                let data_dir = install_path.join(game_type.data_dir());
                if !data_dir.is_dir() {
                    return None;
                }
                Some(game_type)
            })
            .with_context(|| {
                let prefix = "Failed to find one of the following directories:\n";
                let dirs = GameType::iter()
                    .map(|game_type| game_type.data_dir().to_string())
                    .collect::<Vec<String>>()
                    .join(" ");
                format!("{}{}", prefix, dirs)
            })?;

        let data_dir = install_path.join(game_type.data_dir());
        Ok(Self {
            data_dir,
            marker: game_type.marker().to_string(),
            url_start: game_type.url_start().to_string(),
            end_marker: game_type.url_end().to_string(),
        })
    }

    fn extract_url(&self) -> Result<String> {
        const WEB_CACHE_DIR_NAME: &str = "webCaches";
        let web_cache_dir = self.data_dir.join(WEB_CACHE_DIR_NAME);
        if !web_cache_dir.is_dir() {
            return Err(anyhow::anyhow!(
                "{} is not a directory",
                web_cache_dir.display()
            ));
        }

        let data2_path = get_to_data2_file(&web_cache_dir).context("Failed to find data_2 file")?;

        let content = fs::read(data2_path).context("Failed to read data_2 file")?;
        find_gacha_url_in_slice(&content, &self.marker, &self.url_start, &self.end_marker)
    }
}

fn filename_to_version(filename: &str) -> Option<Version> {
    let parts = filename.split('.').collect::<Vec<&str>>();
    if parts.len() != 4 {
        return None;
    }
    let major = parts[0].parse::<u32>().ok()?;
    let minor = parts[1].parse::<u32>().ok()?;
    let patch = parts[2].parse::<u32>().ok()?;
    let other = parts[3].parse::<u32>().ok()?;
    Some(Version {
        major,
        minor,
        patch,
        other,
    })
}

fn collect_versioned_directories(path: &Path) -> Vec<VersionedDirectory> {
    let files = fs::read_dir(path);
    if files.is_err() {
        return vec![];
    }

    let files = files.unwrap();

    files
        .filter_map(|file| {
            let file = file.ok()?;
            let filename = file.file_name();
            let filename_str = filename.to_str()?;

            let version = filename_to_version(filename_str)?;
            let versioned_directory = VersionedDirectory {
                path: path.join(filename_str).to_path_buf(),
                version,
            };
            Some(versioned_directory)
        })
        .collect()
}

const RELATIVE_PATH_TO_DATA2: &[&str] = &["Cache", "Cache_Data", "data_2"];

fn get_to_data2_file(web_cache_dir: &Path) -> Option<PathBuf> {
    let mut versioned_dirs = collect_versioned_directories(web_cache_dir);

    if versioned_dirs.is_empty() {
        println!("Failed to find any versioned directories");
        return None;
    }

    // Note that this is descending order, i.e. the biggest version is at the front.
    // The latest gacha info is in the latest webcache dir.
    versioned_dirs.sort_by(|a, b| b.version.cmp(&a.version));
    let latest_version_dir = &versioned_dirs[0].path;

    let data2_path = latest_version_dir.join(RELATIVE_PATH_TO_DATA2.iter().collect::<PathBuf>());
    if !data2_path.is_file() {
        return None;
    }
    Some(data2_path)
}

fn find_gacha_url_in_slice(
    content: &[u8],
    marker: &str,
    url_start: &str,
    end_marker: &str,
) -> Result<String> {
    let patterns = &[marker];
    let ac = AhoCorasick::builder().build(patterns)?;

    let gacha_marker_end = ac
        .find(content)
        .context(format!("Failed to find pattern {}", patterns[0]))?
        .end();

    // Keep reading until "game_biz=hk4e_global" is encountered, thats where the URL ends.
    let rest_of_content = &content[gacha_marker_end..];
    let patterns = &[end_marker];
    let ac = AhoCorasick::builder().build(patterns)?;

    let mat = ac
        .find(rest_of_content)
        .context(format!("Failed to find {} in file", patterns[0]))?;

    // Note that this variable contains the index from the beginning of |content|.
    // Since URLs can only be a certain length, the value in this variable is used to slice
    // |content| to find the beginning of the URL.
    let url_end_pos = mat.end() + gacha_marker_end;
    const MAX_URL_LENGTH: usize = 2048;
    let url_search_start_pos = if url_end_pos < MAX_URL_LENGTH {
        0
    } else {
        url_end_pos - MAX_URL_LENGTH
    };

    let potential_url_slice = &content[url_search_start_pos..url_end_pos];

    // There could be multiple URL start markers in the slice.
    // So reverse the potential slice and find the reversed pattern, to find the first occurrence.
    let mut reversed_slice: Vec<u8> = potential_url_slice.to_vec();
    reversed_slice.reverse();

    // Note that the URL start marker is also reversed here to match the reversed data above.
    let reversed_start_url = url_start.chars().rev().collect::<String>();
    let reversed_patterns = &[&reversed_start_url];
    let ac = AhoCorasick::builder().build(reversed_patterns)?;

    let mat = ac.find(&reversed_slice).context(format!(
        "Failed to find reversed pattern: {}",
        reversed_patterns[0]
    ))?;

    let target_url = &reversed_slice[..mat.end()];
    let mut target_url: Vec<u8> = target_url.to_vec();
    target_url.reverse();
    let target_url = String::from_utf8(target_url.to_vec())?;
    Ok(target_url)
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        println!("Usage: {} <path to genshin install directory>", args[0]);
        std::process::exit(1);
    }
    let path = Path::new(args[1].as_str());
    if !path.exists() {
        println!("{} does not exist", path.display());
        std::process::exit(1);
    }

    let extractor = PullExtractor::new(path)?;
    let url = extractor.extract_url();
    if let Ok(url) = url {
        println!("{}", url);
    } else {
        println!("Failed to find gacha URL");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn find_gacha_url_only_url() {
        let test_url = "https://gs.hoyoverse.com/genshin/event/e20190909gacha-v3/index.html?anythinghere&game_biz=hk4e_global";

        // any data.
        let mut test_data: Vec<u8> = vec![2, 8, 11, 22, 93];
        test_data.extend_from_slice(test_url.as_bytes());
        // More irrelevant data at end.
        test_data.extend_from_slice(&[43, 100, 65, 2, 1, 4, 73]);

        let result = find_gacha_url_in_slice(
            &test_data,
            "gacha-v3",
            "https://gs.hoyoverse.com/",
            "game_biz=hk4e_global",
        );
        assert!(result.is_ok());
        assert_eq!(test_url, result.unwrap());
    }

    // Verify it can find the URL in binary data.
    #[test]
    fn find_gacha_url() {
        let test_url = "https://gs.hoyoverse.com/genshin/event/e20190909gacha-v3/index.html?anythinghere&game_biz=hk4e_global";
        let test_url_vec = test_url.as_bytes().to_vec();

        let result = find_gacha_url_in_slice(
            &test_url_vec,
            "gacha-v3",
            "https://gs.hoyoverse.com/",
            "game_biz=hk4e_global",
        );
        assert!(result.is_ok());
        assert_eq!(test_url, result.unwrap());
    }

    // gacha-v3 marker is in the url but cannot find the end.
    #[test]
    fn no_gacha_url_has_marker_no_end_marker() {
        let test_url =
            "https://gs.hoyoverse.com/genshin/event/e20190909gacha-v3/index.html?anythinghere";
        let test_url_vec = test_url.as_bytes().to_vec();
        let result = find_gacha_url_in_slice(
            &test_url_vec,
            "gacha-v3",
            "https://gs.hoyoverse.com/",
            "game_biz=hk4e_global",
        );
        assert!(result.is_err());
    }

    // gacha-v3 marker and game_biz=hk4e_global are present but cannot find https:// start.
    #[test]
    fn no_gacha_url_has_marker_has_end_marker_no_start_marker() {
        let test_url = "verse.com/genshin/event/e20190909gacha-v3/index.html?anythinghere";
        let test_url_vec = test_url.as_bytes().to_vec();
        let result = find_gacha_url_in_slice(
            &test_url_vec,
            "gacha-v3",
            "https://gs.hoyoverse.com/",
            "game_biz=hk4e_global",
        );
        assert!(result.is_err());
    }

    #[test]
    fn get_data2_path() -> Result<()> {
        let dir = tempdir()?;
        let cache_data_dir = dir
            .path()
            .join("GenshinImpact_Data")
            .join("webCaches")
            .join("4.5.6.7")
            .join("Cache")
            .join("Cache_Data");
        std::fs::create_dir_all(cache_data_dir.clone())?;
        std::fs::File::create(cache_data_dir.join("data_2"))?;

        assert!(
            get_to_data2_file(&dir.path().join("GenshinImpact_Data").join("webCaches")).is_some()
        );
        Ok(())
    }
}
