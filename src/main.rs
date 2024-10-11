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
        println!("Invalid filename: {}", filename);
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

fn get_to_data2_file(genshin_install_path: &Path) -> Option<PathBuf> {
    let web_caches = genshin_install_path
        .join("GenshinImpact_Data")
        .join("webCaches")
        .to_path_buf();
    let mut versioned_dirs = collect_versioned_directories(&web_caches);

    println!("Found {} versioned directories", versioned_dirs.len());
    for f in versioned_dirs.iter() {
        println!("  {:?}", f);
    }
    if versioned_dirs.is_empty() {
        println!("Failed to find any versioned directories");
        return None;
    }

    // Note that this is descending order, i.e. the biggest version is at the front.
    versioned_dirs.sort_by(|a, b| b.version.cmp(&a.version));

    let latest_version_dir = &versioned_dirs[0].path;

    let data2_path = latest_version_dir
        .join("Cache")
        .join("Cache_Data")
        .join("data_2");

    if !data2_path.is_file() {
        return None;
    }
    Some(data2_path)
}

fn find_gacha_url_in_slice(content: &[u8]) -> Result<String> {
    let patterns = &["e20190909gacha-v3"];
    let ac = AhoCorasick::builder()
        .ascii_case_insensitive(false)
        .build(patterns)
        .unwrap();

    println!("searching for pattern {:?}", patterns);
    let mat = ac.find(&content).context("failed to find pattern")?;
    println!("found pattern {:?}", mat);

    let gacha_marker_end = mat.end();

    // Keep reading until "game_biz=hk4e_global" is encountered, thats where the URL ends.
    let rest_of_content = &content[gacha_marker_end..];
    let patterns = &["game_biz=hk4e_global"];
    let ac = AhoCorasick::builder()
        .ascii_case_insensitive(false)
        .build(patterns)
        .unwrap();

    println!("searching for pattern {:?}", patterns);
    let mat = ac
        .find(rest_of_content)
        .context(format!("Failed to find {} in file", patterns[0]))?;
    println!("found pattern {:?}", mat);

    let url_end_pos = mat.end() + gacha_marker_end;

    let content = &content[..url_end_pos];
    let mut reversed_content: Vec<u8> = content.to_vec();
    reversed_content.reverse();

    // reverse of "https://gs.hoyoverse.com/"
    let reversed_patterns = &["/moc.esrevoyoh.sg//:sptth"];
    let ac = AhoCorasick::builder()
        .ascii_case_insensitive(false)
        .build(reversed_patterns)?;

    println!("searching for pattern {:?}", reversed_patterns);
    let mat = ac
        .find(&reversed_content)
        .context("Failed to find pattern")?;
    println!("found pattern {:?}", mat);

    let target_url = &reversed_content[..mat.end()];
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
    println!("Hello, {}!", args[1]);
    let path = Path::new(args[1].as_str());
    if !path.exists() {
        println!("{} does not exist", path.display());
        std::process::exit(1);
    }

    let data_path = get_to_data2_file(path);
    let result = find_gacha_url_in_data2(&data_path.unwrap());
    println!("{:?}", result);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_gacha_url() {
        let test_url = "https://gs.hoyoverse.com/genshin/event/e20190909gacha-v3/index.html?anythinghere&game_biz=hk4e_global";
        let test_url_vec = test_url.as_bytes().to_vec();
        let result = find_gacha_url_in_slice(&test_url_vec);
        assert!(result.is_ok());

        assert_eq!(test_url, result.unwrap());
    }
}
