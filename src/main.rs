use anyhow::anyhow;
use anyhow::bail;
use anyhow::{Context, Result};
use bstr::ByteSlice;
use reqwest::blocking::Client;
use reqwest::Url;
use serde_json::Value;
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

const MAX_URL_LENGTH: usize = 2048;

// Function type for checking the gacha URL (&str) passed in. Since the testing could transform
// the URL, it returns a String on success.
type TestGachaUrlFn = Box<dyn Fn(&str) -> Result<String>>;

struct GameTypeData {
    data_dir_name: &'static str,
    marker: &'static str,
    url_start: &'static str,
    url_end: &'static str,
    valid_url_check_fn: TestGachaUrlFn,
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

pub struct PullExtractor {
    install_path: PathBuf,
    game_type: GameTypeData,
}

impl PullExtractor {
    pub fn new(install_path: &Path) -> Result<Self> {
        let game_types = [
            // Add more games here.
            GameTypeData {
                data_dir_name: "GenshinImpact_Data",
                marker: "e20190909gacha-v3",
                url_start: "https://gs.hoyoverse.com/",
                url_end: "game_biz=hk4e_global",
                valid_url_check_fn: Box::new(|url| {
                    test_genshin_wish_url(url, "public-operation-hk4e-sg.hoyoverse.com")
                }),
            },
            GameTypeData {
                data_dir_name: "ZenlessZoneZero_Data",
                marker: "getGachaLog",
                url_start: "https://",
                url_end: "game_biz=nap_global",
                valid_url_check_fn: Box::new(|url: &str| test_zzz_signal_url(url)),
            },
        ];
        let game_type = game_types
            .into_iter()
            .find_map(|game_type| {
                let data_dir = install_path.join(game_type.data_dir_name);
                if !data_dir.is_dir() {
                    return None;
                }
                Some(game_type)
            })
            .with_context(|| {
                format!(
                    "Failed to find data directory in {}",
                    install_path.display()
                )
            })?;

        Ok(Self {
            install_path: install_path.to_path_buf(),
            game_type,
        })
    }

    pub fn extract_url(&self) -> Result<String> {
        const WEB_CACHE_DIR_NAME: &str = "webCaches";
        let web_cache_dir = self
            .install_path
            .join(self.game_type.data_dir_name)
            .join(WEB_CACHE_DIR_NAME);
        if !web_cache_dir.is_dir() {
            return Err(anyhow::anyhow!(
                "{} is not a directory",
                web_cache_dir.display()
            ));
        }

        let data2_path = get_to_data2_file(&web_cache_dir).context("Failed to find data_2 file")?;

        let content = fs::read(data2_path).context("Failed to read data_2 file")?;
        let urls = find_gacha_urls_in_slice(
            &content,
            self.game_type.marker,
            self.game_type.url_start,
            self.game_type.url_end,
        )?;
        if urls.is_empty() {
            bail!("Found no gacha URLs");
        }

        for url in urls {
            let result = (self.game_type.valid_url_check_fn)(&url);
            match result {
                Ok(url) => return Ok(url),
                Err(e) => {
                    log::debug!("Testing {} returned an error: {}", url, e);
                    continue;
                }
            }
        }

        bail!("Failed to find a working gacha URL. Check the gacha logs in game first.")
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

fn find_gacha_urls_in_slice(
    content: &[u8],
    marker: &str,
    url_start: &str,
    end_marker: &str,
) -> Result<Vec<String>> {
    let mut urls = vec![];
    for marker_match in content.find_iter(marker) {
        let gacha_marker_end = marker_match + marker.len();
        let rest_of_content = &content[gacha_marker_end..];

        // Note that this variable contains the index from the beginning of |content|.
        let url_end_pos = rest_of_content
            .find(end_marker)
            .context("Failed to find end marker")?
            + end_marker.len()
            + gacha_marker_end;

        // Since URLs can only be a certain length, the value in this variable is used to slice
        // |content| to find the beginning of the URL.
        let url_search_start_pos = url_end_pos.saturating_sub(MAX_URL_LENGTH);

        let potential_url_slice = &content[url_search_start_pos..url_end_pos];

        // Although there could be multiple URLs in the slice, since the slice ends
        // with the end marker, the last occurrence of the url start marker must
        // be the start of the URL.
        let url_start_pos = potential_url_slice
            .rfind(url_start)
            .context("Failed to find url start")?;

        urls.push(
            String::from_utf8(potential_url_slice[url_start_pos..].to_vec())
                .context("Failed to convert URL to string")?,
        );
    }
    Ok(urls)
}

fn test_genshin_wish_url(url: &str, api_host: &str) -> Result<String> {
    log::debug!("Checking genshin wish url: {}", url);
    let client = Client::new();
    let mut uri =
        reqwest::Url::parse(url).with_context(|| format!("{} is not a valid URL", url))?;

    uri.set_path("gacha_info/api/getGachaLog");
    uri.set_host(Some(api_host))
        .with_context(|| format!("Failed to set host to {}", api_host))?;
    uri.set_fragment(None);

    let mut query_params: HashMap<Cow<str>, Cow<str>> = uri.query_pairs().collect();
    query_params.insert("lang".into(), "en".into());
    query_params.insert("gacha_type".into(), "301".into());
    query_params.insert("size".into(), "5".into());
    query_params.insert("lang".into(), "en-us".into());

    uri.set_query(Some(
        &serde_urlencoded::to_string(&query_params).context("Failed to set query params")?,
    ));

    let response = client
        .get(uri.as_str())
        .header("Content-Type", "application/json")
        .send()?
        .json::<Value>()?;

    let retcode = response
        .get("retcode")
        .context("Failed to find retcode in response JSON")?;

    log::debug!("Got retcode: {}", retcode);

    let retcode = retcode
        .as_i64()
        .context("Failed to convert retcode to i64")?;
    if retcode != 0 {
        Ok(url.to_string())
    } else {
        bail!("JSON retcode did not contain 0, it was {}", retcode)
    }
}

// TODO: A test with dependency injection would be good.
fn test_zzz_signal_url(url: &str) -> Result<String> {
    log::debug!("Checking zzz signal url: {}", url);

    // A hack to get localhost url to always use HTTP. Only good for testing.
    // In its own block so that any variables in this "test only" code does not contaminate
    // the rest of the code.
    let mut parsed_url = Url::parse(url).context("Failed to parse URL")?;
    if parsed_url.scheme() == "https" && parsed_url.host_str() == Some("127.0.0.1") {
        parsed_url
            .set_scheme("http")
            .map_err(|_| anyhow!("Failed to change scheme to http"))?;
    }

    let client = Client::new();
    let response = client
        .get(parsed_url.clone())
        .header("Content-Type", "application/json")
        .send()
        .context("Failed to get response")?
        .json::<Value>()
        .context("Failed to get json response")?;

    const RETURN_CODE_FIELD_NAME: &str = "retcode";
    let retcode = response.get(RETURN_CODE_FIELD_NAME).context(format!(
        "Response JSON from {} did not contain a {} field",
        parsed_url, RETURN_CODE_FIELD_NAME
    ))?;

    log::debug!("{} contained: {}", RETURN_CODE_FIELD_NAME, retcode);

    let retcode = retcode.as_i64().context("Not a number.")?;
    if retcode != 0 {
        bail!("Got non-zero return code: {}", retcode);
    }

    // Recreate parsed_url from original URL again, so that it would be unmodified even for tests.
    let parsed_url = Url::parse(url).context("Failed to parse URL")?;
    let mut query_params: Vec<(String, String)> = parsed_url.query_pairs().into_owned().collect();
    const KEYS_TO_KEEP: [&str; 5] = ["authkey", "authkey_ver", "sign_type", "game_biz", "lang"];
    query_params.retain(|(key, _)| KEYS_TO_KEEP.contains(&key.as_str()));

    Ok(format!(
        "{}://{}{}?{}",
        parsed_url.scheme(),
        parsed_url
            .host()
            .context(format!("Cannot find host in URL: {}", parsed_url))?,
        parsed_url.path(),
        serde_urlencoded::to_string(&query_params)
            .with_context(|| format!("Failed to serialize query params {:?}", &query_params))?
    ))
}

fn main() -> Result<()> {
    env_logger::init();

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
    let result = extractor.extract_url();
    if let Ok(url) = result {
        println!("Found gacha URL! Copy the URL below:");
        println!("{}", url);
    } else {
        println!(
            "Failed to find gacha URL with error: {}",
            result.unwrap_err()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{BufWriter, Write};

    use super::*;
    use tempfile::tempdir;

    // Verify it can find the URL in binary data.
    #[test]
    fn find_gacha_url_only_url() {
        let test_url = "https://gs.hoyoverse.com/genshin/event/e20190909gacha-v3/index.html?anythinghere&game_biz=hk4e_global";

        // any data.
        let mut test_data: Vec<u8> = vec![2, 8, 11, 22, 93];
        test_data.extend_from_slice(test_url.as_bytes());
        // More irrelevant data at end.
        test_data.extend_from_slice(&[43, 100, 65, 2, 1, 4, 73]);

        let result = find_gacha_urls_in_slice(
            &test_data,
            "gacha-v3",
            "https://gs.hoyoverse.com/",
            "game_biz=hk4e_global",
        );
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(1, result.len());
        assert_eq!(test_url, result[0]);
    }

    #[test]
    fn find_gacha_url() {
        let test_url = "https://gs.hoyoverse.com/genshin/event/e20190909gacha-v3/index.html?anythinghere&game_biz=hk4e_global";
        let test_url_vec = test_url.as_bytes().to_vec();

        let result = find_gacha_urls_in_slice(
            &test_url_vec,
            "gacha-v3",
            "https://gs.hoyoverse.com/",
            "game_biz=hk4e_global",
        );
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(1, result.len());
        assert_eq!(test_url, result[0]);
    }

    // Verify that it can find multiple urls.
    #[test]
    fn find_gacha_urls_in_ascii() {
        let test_url1 = "https://gs.hoyoverse.com/genshin/event/e20190909gacha-v3/index.html?ANYDATA11111&game_biz=hk4e_global";
        let test_url2 = "https://gs.hoyoverse.com/genshin/event/e20190909gacha-v3/index.html?DIFFERTDATA22222&game_biz=hk4e_global";

        let test_data: Vec<u8> = [
            test_url1.as_bytes(),
            // any data.
            &[0xFF, 0x00, 0x3A, 0xBC],
            test_url2.as_bytes(),
        ]
        .concat();

        let result = find_gacha_urls_in_slice(
            &test_data,
            "gacha-v3",
            "https://gs.hoyoverse.com/",
            "game_biz=hk4e_global",
        );
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(2, result.len());
        assert_eq!(test_url1, result[0]);
        assert_eq!(test_url2, result[1]);
    }

    // gacha-v3 marker is in the url but cannot find the end.
    #[test]
    fn no_gacha_url_has_marker_no_end_marker() {
        let test_url =
            "https://gs.hoyoverse.com/genshin/event/e20190909gacha-v3/index.html?anythinghere";
        let test_url_vec = test_url.as_bytes().to_vec();
        let result = find_gacha_urls_in_slice(
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
        let result = find_gacha_urls_in_slice(
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
        std::fs::create_dir_all(&cache_data_dir)?;
        std::fs::File::create(cache_data_dir.join("data_2"))?;

        assert!(
            get_to_data2_file(&dir.path().join("GenshinImpact_Data").join("webCaches")).is_some()
        );
        Ok(())
    }

    #[test]
    fn get_data2_path_multiple_versions() -> Result<()> {
        let dir = tempdir()?;
        let older_cache = dir
            .path()
            .join("GenshinImpact_Data")
            .join("webCaches")
            .join("1.2.3.5000")
            .join("Cache")
            .join("Cache_Data");
        std::fs::create_dir_all(&older_cache)?;
        std::fs::File::create(older_cache.join("data_2"))?;

        let newer_cache = dir
            .path()
            .join("GenshinImpact_Data")
            .join("webCaches")
            // Although the right most number is smaller, this is newer.
            .join("1.2.4.0")
            .join("Cache")
            .join("Cache_Data");
        std::fs::create_dir_all(&newer_cache)?;
        std::fs::File::create(newer_cache.join("data_2"))?;

        let data_path =
            get_to_data2_file(&dir.path().join("GenshinImpact_Data").join("webCaches")).unwrap();

        assert_eq!(data_path, newer_cache.join("data_2"));
        Ok(())
    }

    #[test]
    fn test_zzz_url() -> Result<()> {
        let mut server = mockito::Server::new();
        let url= format!("{}{}{}",
            "http://", 
            &server.host_with_port(),
            // Note that these include the required params.
            "/index.html?extraparam=1234&authkey=key&authkey_ver=2&sign_type=sometype&game_biz=nap_global&lang=en&more=stuff&andsomemore=fluffs");

        // Create a mock
        let mock = server
            // This path matches the above.
            .mock("GET", "/index.html?extraparam=1234&authkey=key&authkey_ver=2&sign_type=sometype&game_biz=nap_global&lang=en&more=stuff&andsomemore=fluffs")
            .with_status(200)
            .with_header("content-type", "application/json")
            // A minimal JSON to return retcode=0.
            .with_body(r#"{"retcode": 0}"#)
            .create();

        let result = test_zzz_signal_url(&url)?;
        // Verify that extraneous params are removed.
        // Hardcoded 127.0.0.1 without a port number. Note that
        // server.host_with_port() includes a port number.
        // Its ok to change the host here if the framework changes.
        assert_eq!("http://127.0.0.1/index.html?authkey=key&authkey_ver=2&sign_type=sometype&game_biz=nap_global&lang=en", result);
        mock.assert();
        Ok(())
    }

    #[test]
    fn test_zzz_url_retcode_not_0() {
        let mut server = mockito::Server::new();
        let url= format!("{}{}{}",
            "http://", 
            &server.host_with_port(),
            // Note that these include the required params.
            "/index.html?extraparam=1234&authkey=key&authkey_ver=2&sign_type=sometype&game_biz=nap_global&lang=en&more=stuff&andsomemore=fluffs");

        // Create a mock
        let mock = server
            // This path matches the above.
            .mock("GET", "/index.html?extraparam=1234&authkey=key&authkey_ver=2&sign_type=sometype&game_biz=nap_global&lang=en&more=stuff&andsomemore=fluffs")
            .with_status(200)
            .with_header("content-type", "application/json")
            // Retcode is -1! The function should return an error.
            .with_body(r#"{"retcode": -1}"#)
            .create();

        let result = test_zzz_signal_url(&url);
        assert!(result.is_err());
        mock.assert();
    }

    // TODO Might be good to move this to tests/ as integration tests. But this requires seprating
    // this to library + executable first.
    #[test]
    fn test_genshin_pull_extractor_new() -> Result<()> {
        let dir = tempdir()?;
        let cache_data_dir = dir
            .path()
            .join("GenshinImpact_Data")
            .join("webCaches")
            .join("4.5.6.7")
            .join("Cache")
            .join("Cache_Data");
        std::fs::create_dir_all(&cache_data_dir)?;
        std::fs::File::create(cache_data_dir.join("data_2"))?;
        PullExtractor::new(dir.path())?;
        Ok(())
    }

    #[test]
    fn test_zzz_pull_extractor_new() -> Result<()> {
        let dir = tempdir()?;
        let cache_data_dir = dir
            .path()
            .join("ZenlessZoneZero_Data")
            .join("webCaches")
            .join("4.5.6.7")
            .join("Cache")
            .join("Cache_Data");
        std::fs::create_dir_all(&cache_data_dir)?;
        std::fs::File::create(cache_data_dir.join("data_2"))?;
        PullExtractor::new(dir.path())?;
        Ok(())
    }

    #[test]
    fn test_zzz_pull_extractor_extract() -> Result<()> {
        let dir = tempdir()?;
        let cache_data_dir = dir
            .path()
            .join("ZenlessZoneZero_Data")
            .join("webCaches")
            .join("4.5.6.7")
            .join("Cache")
            .join("Cache_Data");
        std::fs::create_dir_all(&cache_data_dir)?;
        let data_2_file = std::fs::File::create(cache_data_dir.join("data_2"))?;
        let extractor = PullExtractor::new(dir.path())?;

        let mut server = mockito::Server::new();
        let url= format!("{}{}{}",
            "https://", 
            &server.host_with_port(),
            // Note that these include the required params.
            "/getGachaLog/index.html?lang=en&extraparam=1234&authkey=key&authkey_ver=2&sign_type=sometype&game_biz=nap_global");

        let mut writer = BufWriter::new(data_2_file);
        writer.write_all(url.as_bytes())?;
        writer.flush()?;

        // Create a mock
        let mock = server
            // This path matches the above.
            .mock("GET", "/getGachaLog/index.html?lang=en&extraparam=1234&authkey=key&authkey_ver=2&sign_type=sometype&game_biz=nap_global")
            .with_status(200)
            .with_header("content-type", "application/json")
            // Retcode is -1! The function should return an error.
            .with_body(r#"{"retcode": 0}"#)
            .create();

        let result = extractor.extract_url()?;
        assert_eq!("https://127.0.0.1/getGachaLog/index.html?lang=en&authkey=key&authkey_ver=2&sign_type=sometype&game_biz=nap_global", result);
        mock.assert();

        Ok(())
    }
}
