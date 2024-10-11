use std::cmp::Ordering;
use std::env;

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use aho_corasick::AhoCorasick;

use anyhow::{Context, Result};

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

#[derive(Debug)]
struct VersionedDirectory {
    path: PathBuf,
    version: Version,
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

const RELATIVE_PATH_TO_WEBCACHES: &[&str] = &["GenshinImpact_Data", "webCaches"];
const RELATIVE_PATH_TO_DATA2: &[&str] = &["Cache", "Cache_Data", "data_2"];

fn get_to_data2_file(genshin_install_path: &Path) -> Option<PathBuf> {
    let web_caches =
        genshin_install_path.join(RELATIVE_PATH_TO_WEBCACHES.iter().collect::<PathBuf>());

    let mut versioned_dirs = collect_versioned_directories(&web_caches);

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

const GACHA_URL_MARKER: &str = "e20190909gacha-v3";
const URL_END_MARKER: &str = "game_biz=hk4e_global";

// reverse of "https://gs.hoyoverse.com/"
const URL_START_REVERSED: &str = "/moc.esrevoyoh.sg//:sptth";

fn find_gacha_url_in_slice(content: &[u8]) -> Result<String> {
    let patterns = &[GACHA_URL_MARKER];
    let ac = AhoCorasick::builder().build(patterns)?;

    let gacha_marker_end = ac
        .find(&content)
        .context(format!("Failed to find pattern {}", patterns[0]))?
        .end();

    // Keep reading until "game_biz=hk4e_global" is encountered, thats where the URL ends.
    let rest_of_content = &content[gacha_marker_end..];
    let patterns = &[URL_END_MARKER];
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

    let reversed_patterns = &[URL_START_REVERSED];
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

fn find_gacha_url_in_data2(data2_path: &Path) -> Result<String> {
    let content = fs::read(data2_path)?;
    find_gacha_url_in_slice(&content)
}

fn main() {
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

    let data_path = get_to_data2_file(path);
    let result = find_gacha_url_in_data2(&data_path.unwrap());
    if let Ok(url) = result {
        println!("{}", url);
    } else {
        println!("Failed to find gacha URL");
    }
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

        let result = find_gacha_url_in_slice(&test_data);
        assert!(result.is_ok());
        assert_eq!(test_url, result.unwrap());
    }

    // Verify it can find the URL in binary data.
    #[test]
    fn find_gacha_url() {
        let test_url = "https://gs.hoyoverse.com/genshin/event/e20190909gacha-v3/index.html?anythinghere&game_biz=hk4e_global";
        let test_url_vec = test_url.as_bytes().to_vec();

        let result = find_gacha_url_in_slice(&test_url_vec);
        assert!(result.is_ok());
        assert_eq!(test_url, result.unwrap());
    }

    // gacha-v3 marker is in the url but cannot find the end.
    #[test]
    fn no_gacha_url_has_marker_no_end_marker() {
        let test_url =
            "https://gs.hoyoverse.com/genshin/event/e20190909gacha-v3/index.html?anythinghere";
        let test_url_vec = test_url.as_bytes().to_vec();
        let result = find_gacha_url_in_slice(&test_url_vec);
        assert!(result.is_err());
    }

    // gacha-v3 marker and game_biz=hk4e_global are present but cannot find https:// start.
    #[test]
    fn no_gacha_url_has_marker_has_end_marker_no_start_marker() {
        let test_url = "verse.com/genshin/event/e20190909gacha-v3/index.html?anythinghere";
        let test_url_vec = test_url.as_bytes().to_vec();
        let result = find_gacha_url_in_slice(&test_url_vec);
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

        assert!(get_to_data2_file(dir.path()).is_some());
        Ok(())
    }
}
