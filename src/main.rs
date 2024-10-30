use anyhow::bail;
use anyhow::{Context, Result};
use bstr::ByteSlice;
use enum_assoc::Assoc;
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
use strum::IntoEnumIterator;
use strum_macros::EnumIter;

const MAX_URL_LENGTH: usize = 2048;

// If there are more games, add them here.
// TODO: This might not be necessary since there is a struct that contains this anyways. Move it
// in its contructor.
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
    #[assoc(marker = "getGachaLog")]
    #[assoc(url_start = "https://")]
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

// Function type for checking the gacha URL (&str) passed in. Since the testing could transform
// the URL, it returns a String on success.
type TestGachaUrlFn = Box<dyn Fn(&str) -> Result<String>>;

struct PullExtractor {
    data_dir: PathBuf,
    url_start: String,
    marker: String,
    end_marker: String,
    valid_url_check_fn: TestGachaUrlFn,
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

        let valid_url_check_fn: TestGachaUrlFn = match game_type {
            GameType::GenshinGlobal => {
                Box::new(|url| test_genshin_wish_url(url, "public-operation-hk4e-sg.hoyoverse.com"))
            }
            GameType::ZenlessZoneZeroGlobal => Box::new(|url: &str| test_zzz_signal_url(url)),
        };

        let data_dir = install_path.join(game_type.data_dir());
        Ok(Self {
            data_dir,
            marker: game_type.marker().to_string(),
            url_start: game_type.url_start().to_string(),
            end_marker: game_type.url_end().to_string(),
            valid_url_check_fn,
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
        let urls =
            find_gacha_urls_in_slice(&content, &self.marker, &self.url_start, &self.end_marker)?;
        if urls.is_empty() {
            return Err(anyhow::anyhow!("Found no gacha URLs"));
        }

        for url in urls {
            let result = (self.valid_url_check_fn)(&url);
            match result {
                Ok(url) => return Ok(url),
                Err(e) => {
                    log::debug!("Testing {} returned an error: {}", url, e);
                    continue;
                }
            }
        }

        Err(anyhow::anyhow!(
            "Failed to find a working gacha URL. Check the gacha logs in game first."
        ))
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
        anyhow::bail!("JSON retcode did not contain 0, it was {}", retcode)
    }
}

// TODO: A test with dependency injection would be good.
fn test_zzz_signal_url(url: &str) -> Result<String> {
    log::debug!("Checking zzz signal url: {}", url);
    let client = Client::new();
    let response = client
        .get(url)
        .header("Content-Type", "application/json")
        .send()
        .and_then(|resp| resp.json::<Value>())
        .context("Failed to get json response")?;

    log::debug!("Response JSON: {:?}", response);

    const RETURN_CODE_FIELD_NAME: &str = "retcode";
    let retcode = response.get(RETURN_CODE_FIELD_NAME).context(format!(
        "Response JSON from {} did not contain a {} field",
        url, RETURN_CODE_FIELD_NAME
    ))?;

    log::debug!("{} contained: {}", RETURN_CODE_FIELD_NAME, retcode);

    let retcode = retcode.as_i64().context("Not a number.")?;
    if retcode != 0 {
        bail!("Got non-zero return code: {}", retcode);
    }

    let parsed_url = Url::parse(url).context("Failed to parse URL")?;
    let mut query_params: Vec<(String, String)> = parsed_url.query_pairs().into_owned().collect();
    const KEYS_TO_KEEP: [&str; 5] = ["authkey", "authkey_ver", "sign_type", "game_biz", "lang"];
    query_params.retain(|(key, _)| KEYS_TO_KEEP.contains(&key.as_str()));

    Ok(format!(
        "{}://{}{}?{}",
        parsed_url.scheme(),
        parsed_url
            .host()
            .context(format!("Cannot find host in URL: {}", url))?,
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
    use tempfile::tempdir;

    use super::*;

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
        std::fs::create_dir_all(cache_data_dir.clone())?;
        std::fs::File::create(cache_data_dir.join("data_2"))?;

        assert!(
            get_to_data2_file(&dir.path().join("GenshinImpact_Data").join("webCaches")).is_some()
        );
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
}
